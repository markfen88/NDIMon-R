#include "IPCServer.h"
#include <iostream>
#include <cstring>
#include <algorithm>
#include <cerrno>
#include <string>
#include <unistd.h>
#include <fcntl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <poll.h>

IPCServer::IPCServer() = default;

IPCServer::~IPCServer() {
    stop();
}

bool IPCServer::start(const std::string& socket_path) {
    socket_path_ = socket_path;
    unlink(socket_path_.c_str());

    server_fd_ = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (server_fd_ < 0) {
        std::cerr << "[IPC] socket() failed\n";
        return false;
    }

    struct sockaddr_un addr = {};
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, socket_path_.c_str(), sizeof(addr.sun_path) - 1);

    if (bind(server_fd_, (struct sockaddr*)&addr, sizeof(addr)) < 0) {
        std::cerr << "[IPC] bind() failed\n";
        close(server_fd_);
        server_fd_ = -1;
        return false;
    }

    // Restrict the control socket to the owner (ndimon-r and ndimon-api run as
    // the same user). Without this, any local user could drive the decoder.
    if (chmod(socket_path_.c_str(), 0600) != 0)
        std::cerr << "[IPC] WARNING: chmod 0600 on socket failed\n";

    listen(server_fd_, 5);
    running_ = true;
    thread_ = std::thread(&IPCServer::server_thread, this);
    std::cout << "[IPC] listening on " << socket_path_ << "\n";
    return true;
}

void IPCServer::stop() {
    running_ = false;
    if (server_fd_ >= 0) {
        shutdown(server_fd_, SHUT_RDWR);
        close(server_fd_);
        server_fd_ = -1;
    }
    // Close all subscriber connections so they get a close/EOF
    {
        std::lock_guard<std::mutex> lk(subscribers_mutex_);
        for (int fd : subscribers_) close(fd);
        subscribers_.clear();
    }
    if (thread_.joinable()) thread_.join();
    unlink(socket_path_.c_str());
}

void IPCServer::server_thread() {
    while (running_) {
        struct pollfd pfd = { server_fd_, POLLIN, 0 };
        int r = poll(&pfd, 1, 500);
        if (r <= 0) continue;

        int client_fd = accept(server_fd_, nullptr, nullptr);
        if (client_fd < 0) continue;

        if (handle_client(client_fd)) {
            close(client_fd);
        }
        // else: subscriber fd left open, managed via subscribers_ list
    }
}

bool IPCServer::handle_client(int client_fd) {
    // Read until newline (commands are newline-terminated JSON).
    // Single read() can return a partial command if the kernel splits the
    // write — keep pulling until \n or we hit the size cap.
    static constexpr size_t kMaxCommand = 64 * 1024;
    std::string buf;
    buf.reserve(1024);
    char chunk[2048];
    while (buf.size() < kMaxCommand) {
        struct pollfd pfd = { client_fd, POLLIN, 0 };
        int pr = poll(&pfd, 1, 1000);
        if (pr <= 0) break;
        ssize_t n = read(client_fd, chunk, sizeof(chunk));
        if (n <= 0) break;
        buf.append(chunk, chunk + n);
        if (buf.find('\n') != std::string::npos) break;
    }
    if (buf.empty()) return true;

    try {
        auto j = nlohmann::json::parse(buf);
        std::string action = j.value("action", "");

        // Subscriber mode: keep fd open and push events to it
        if (action == "subscribe") {
            // Non-blocking writes so a stalled subscriber can't block the
            // recv/decode/IPC threads that call push_event().
            int fl = fcntl(client_fd, F_GETFL, 0);
            if (fl >= 0) fcntl(client_fd, F_SETFL, fl | O_NONBLOCK);
            {
                std::lock_guard<std::mutex> lk(subscribers_mutex_);
                subscribers_.push_back(client_fd);
            }
            std::cout << "[IPC] subscriber connected (fd=" << client_fd << ")\n";
            if (sub_connected_cb_) sub_connected_cb_();
            return false;  // caller must NOT close fd
        }

        // Build IPCCommand
        IPCCommand cmd;
        cmd.action          = action;
        cmd.source_name     = j.value("source_name", "");
        cmd.source_ip       = j.value("source_ip",   "");
        cmd.tally_program   = j.value("tally_program", false);
        cmd.tally_preview   = j.value("tally_preview", false);
        cmd.res_width       = j.value("width",      0u);
        cmd.res_height      = j.value("height",     0u);
        cmd.res_refresh     = j.value("refresh",    0u);
        cmd.res_refresh_hz  = j.value("refresh_hz", 0.0);
        cmd.output_index     = j.value("output",  0);
        cmd.scale_mode_str   = j.value("scale_mode", "");
        cmd.source_available  = j.value("source_available", false);
        cmd.rotation_degrees  = j.value("rotation", 0u);

        // Commands that return JSON data
        if (action == "status" && status_cb_) {
            auto resp = status_cb_().dump() + "\n";
            write(client_fd, resp.c_str(), resp.size());
            return true;
        }

        if ((action == "get_status_all" || action == "get_modes" ||
             action == "set_resolution" || action == "health") && query_cb_) {
            auto resp = query_cb_(cmd).dump() + "\n";
            write(client_fd, resp.c_str(), resp.size());
            return true;
        }

        // Fire-and-forget commands
        // set_scale_mode, set_output_source, auto_resolution, connect, disconnect, set_tally, reload_config
        if (cmd_cb_) {
            cmd_cb_(cmd);
        }

        const char* ok = "{\"ok\":true}\n";
        write(client_fd, ok, strlen(ok));
    } catch (const std::exception& e) {
        std::cerr << "[IPC] parse error: " << e.what() << "\n";
    }
    return true;
}

void IPCServer::push_event(const nlohmann::json& event) {
    std::string msg = event.dump() + "\n";
    std::lock_guard<std::mutex> lk(subscribers_mutex_);
    std::vector<int> dead;
    for (int fd : subscribers_) {
        // MSG_DONTWAIT on top of the per-fd O_NONBLOCK and MSG_NOSIGNAL so a
        // disconnected subscriber doesn't kill us with SIGPIPE.
        ssize_t r = send(fd, msg.c_str(), msg.size(), MSG_DONTWAIT | MSG_NOSIGNAL);
        if (r < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                // Slow subscriber: drop this event rather than stall.
                continue;
            }
            dead.push_back(fd);
        }
    }
    for (int fd : dead) {
        close(fd);
        subscribers_.erase(std::remove(subscribers_.begin(), subscribers_.end(), fd),
                           subscribers_.end());
    }
}
