#pragma once
#include "FollowerProtocol.h"
#include <cstring>
#include <iostream>
#include <grp.h>
#include <sys/stat.h>
#include <sys/un.h>

namespace follower {
// Finder has no event subscribers or worker threads: dispatch on its main
// loop, where all NDI SDK lifetime operations already happen.
class Listener {
public:
    ~Listener() {
        if (fd_ >= 0) { close(fd_); unlink(path_.c_str()); }
    }
    Listener() = default;
    Listener(const Listener&) = delete;
    Listener& operator=(const Listener&) = delete;
    bool start(const std::string& path, const std::string& group_name = "ndimon") {
        sockaddr_un address{};
        if (fd_ >= 0 || path.size() >= sizeof(address.sun_path)) return false;
        fd_ = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
        if (fd_ < 0) return false;
        path_ = path;
        address.sun_family = AF_UNIX;
        std::strcpy(address.sun_path, path.c_str());
        unlink(path.c_str());
        if (bind(fd_, reinterpret_cast<sockaddr*>(&address), sizeof(address)) != 0) {
            close(fd_); fd_ = -1; return false;
        }
        bool secured = chmod(path.c_str(), 0660) == 0;
        if (!group_name.empty()) {
            if (auto* group = getgrnam(group_name.c_str()))
                secured = chown(path.c_str(), static_cast<uid_t>(-1), group->gr_gid) == 0 && secured;
        }
        if (!secured || listen(fd_, 5) != 0) {
            close(fd_); fd_ = -1; unlink(path.c_str()); return false;
        }
        return true;
    }
    // Serve one waiting connection; false when none was waiting.
    template<class Handler> bool poll_once(Handler handler) {
        const int client = accept4(fd_, nullptr, nullptr, SOCK_CLOEXEC);
        if (client < 0) return false;
        try { reply(client, handler(read_line(client))); }
        catch (const std::exception& e) { reply(client, failure(e.what())); }
        close(client);
        return true;
    }
    // Serve every connection that is already waiting (a bounded burst, so one
    // chatty client cannot hold the loop), not one per loop iteration.
    template<class Handler> int poll_all(Handler handler, int limit = 16) {
        int served = 0;
        while (served < limit && poll_once(handler)) ++served;
        return served;
    }
private:
    int fd_ = -1;
    std::string path_;
};
} // namespace follower
