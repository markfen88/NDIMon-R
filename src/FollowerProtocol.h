#pragma once
#include <nlohmann/json.hpp>
#include <chrono>
#include <cstdint>
#include <cerrno>
#include <stdexcept>
#include <string>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

namespace follower {
using nlohmann::json;

inline json failure(const std::string& error) {
    return {{"ok", false}, {"errors", json::array({error})}};
}

// Revisions are global document revisions. Equal revisions are permitted for
// retries and for handing over another complete domain at the same revision.
// `"reset": true` accepts an older revision: the control plane sends it when the
// volatile cache belongs to another lineage of the durable state (a purged and
// reinstalled device that was not rebooted), whose revisions say nothing here.
struct Revision {
    bool applied = false;
    uint64_t value = 0;
    uint64_t check(const json& request) const {
        const auto& r = request.at("revision");
        if (!r.is_number_integer() || (!r.is_number_unsigned() && r.get<int64_t>() < 0))
            throw std::runtime_error("revision: expected non-negative integer");
        const auto next = r.get<uint64_t>();
        bool reset = false;
        if (request.contains("reset")) {
            if (!request.at("reset").is_boolean())
                throw std::runtime_error("reset: expected boolean");
            reset = request.at("reset").get<bool>();
        }
        if (applied && next < value && !reset)
            throw std::runtime_error("revision: older than last applied revision");
        return next;
    }
    void commit(uint64_t next) { value = next; applied = true; }
    json status() const { return applied ? json(value) : json(nullptr); }
};

// One request/response per connection, newline-terminated UTF-8 JSON. Bound
// both size and elapsed time so a partial writer cannot hold the finder loop.
inline json read_line(int fd) {
    constexpr size_t max_command = 1024 * 1024;
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(1);
    std::string buffer;
    char chunk[2048];
    while (buffer.size() < max_command) {
        const auto left = std::chrono::duration_cast<std::chrono::milliseconds>(
            deadline - std::chrono::steady_clock::now()).count();
        if (left <= 0) break;
        pollfd pfd{fd, POLLIN, 0};
        const int ready = poll(&pfd, 1, static_cast<int>(left));
        if (ready < 0 && errno == EINTR) continue;
        if (ready <= 0) break;
        const auto n = read(fd, chunk, sizeof(chunk));
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        buffer.append(chunk, static_cast<size_t>(n));
        const auto end = buffer.find('\n');
        if (end != std::string::npos && end < max_command)
            return json::parse(buffer.substr(0, end));
    }
    throw std::runtime_error("framing: incomplete or oversized JSON line");
}

inline void reply(int fd, const json& response) {
    const auto line = response.dump() + "\n";
    size_t offset = 0;
    while (offset < line.size()) {
        const auto n = send(fd, line.data() + offset, line.size() - offset, MSG_NOSIGNAL | MSG_DONTWAIT);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) break;
        offset += static_cast<size_t>(n);
    }
}
} // namespace follower
