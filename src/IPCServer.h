#pragma once
#include <string>
#include <functional>
#include <thread>
#include <atomic>
#include <vector>
#include <mutex>
#include <nlohmann/json.hpp>

struct IPCCommand {
    std::string action;
    std::string source_name;
    std::string source_ip;
    bool tally_program = false;
    bool tally_preview = false;
    uint32_t res_width      = 0;
    uint32_t res_height     = 0;
    uint32_t res_refresh    = 0;
    double   res_refresh_hz = 0.0;
    int output_index     = 0;      // which display output (0-based)
    std::string scale_mode_str;    // for set_scale_mode action
    bool source_available = false; // for show_splash action
    uint32_t rotation_degrees = 0; // for set_rotation action
};

using IPCCommandCallback = std::function<void(const IPCCommand&)>;
using IPCStatusProvider  = std::function<nlohmann::json()>;
using IPCQueryCallback   = std::function<nlohmann::json(const IPCCommand&)>;

class IPCServer {
public:
    IPCServer();
    ~IPCServer();

    bool start(const std::string& socket_path = "/tmp/ndi-decoder.sock");
    void stop();

    void set_command_callback(IPCCommandCallback cb) { cmd_cb_    = std::move(cb); }
    void set_status_provider(IPCStatusProvider  cb)  { status_cb_ = std::move(cb); }
    // For commands that need to return data (e.g. get_modes, set_resolution)
    void set_query_callback(IPCQueryCallback    cb)  { query_cb_  = std::move(cb); }
    // Called when a new subscriber connects (used to replay buffered events)
    void set_subscriber_connected_callback(std::function<void()> cb) { sub_connected_cb_ = std::move(cb); }

    // Push an event JSON to all active subscribers
    void push_event(const nlohmann::json& event);

private:
    void server_thread();
    // Returns true = close fd after call, false = subscriber (fd kept open)
    bool handle_client(int client_fd);

    int server_fd_ = -1;
    std::string socket_path_;
    std::thread thread_;
    std::atomic<bool> running_{false};
    IPCCommandCallback cmd_cb_;
    IPCStatusProvider  status_cb_;
    IPCQueryCallback   query_cb_;
    std::function<void()> sub_connected_cb_;

    std::vector<int> subscribers_;
    std::mutex       subscribers_mutex_;
};
