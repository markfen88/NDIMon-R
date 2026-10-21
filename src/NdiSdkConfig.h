#pragma once
// Shared by ndimon-r and ndimon-finder: both processes write the same
// ~/.ndi/ndi-config.v1.json, so the transport keys and the write itself live here.

#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <string>
#include <unistd.h>
#include <ifaddrs.h>
#include <arpa/inet.h>
#include <net/if.h>
#include <regex>
#include <sstream>

namespace ndisdk {

struct NetworkSelection {
    bool managed = false;
    std::string interface;
    std::string address;
    std::string signature() const { return managed ? interface + ":" + address : "automatic"; }
    bool ready() const { return !managed || !address.empty(); }
};

inline NetworkSelection network_selection(const std::string& path = "/etc/ndimon-network-interfaces.json",
                                          const std::string& role = "ndi") {
    std::ifstream stream(path);
    if (!stream) return {};
    NetworkSelection selection;
    selection.managed = true;
    std::ostringstream text;
    text << stream.rdbuf();
    const std::string value = text.str();
    if (value.size() > 4096) return selection;
    std::smatch match;
    static const std::regex ndi_field("\\\"ndi\\\"\\s*:\\s*\\\"([A-Za-z0-9_.-]{1,15})\\\"");
    static const std::regex management_field("\\\"management\\\"\\s*:\\s*\\\"([A-Za-z0-9_.-]{1,15})\\\"");
    const auto& field = role == "management" ? management_field : ndi_field;
    if (!std::regex_search(value, match, field)) return selection;
    selection.interface = match[1];
    struct ifaddrs* interfaces = nullptr;
    if (getifaddrs(&interfaces) != 0) return selection;
    for (auto* item = interfaces; item; item = item->ifa_next) {
        if (!item->ifa_addr || item->ifa_addr->sa_family != AF_INET ||
            !(item->ifa_flags & IFF_UP) || selection.interface != item->ifa_name) continue;
        char address[INET_ADDRSTRLEN] = {};
        const auto* socket = reinterpret_cast<const struct sockaddr_in*>(item->ifa_addr);
        if (inet_ntop(AF_INET, &socket->sin_addr, address, sizeof(address))) {
            selection.address = address;
            break;
        }
    }
    freeifaddrs(interfaces);
    return selection;
}

// RUDP is the NDI 5+ default and the transport the NDI docs recommend.
inline constexpr const char* kDefaultRxpm = "RUDP";

struct TransportKeys {
    bool rudp      = true;   // ndi.rudp.recv.enable
    bool multicast = false;  // ndi.multicast.recv.enable
    bool tcp       = false;  // ndi.tcp.recv.enable      (multi-TCP)
    bool unicast   = false;  // ndi.unicast.recv.enable  (UDP + FEC)
};

// Rxpm -> receive keys. Exactly one transport is enabled so the result does not
// depend on the SDK's defaults for keys we leave unset (the docs contradict
// themselves on those). The SDK falls back TCP-wards when a mode is not
// supported by both sides: multi-TCP -> unicast UDP -> single TCP.
//   RUDP       rudp
//   Multicast  rudp + multicast (sender must multicast; otherwise RUDP is used)
//   M-TCP      tcp
//   UDP        unicast
//   TCP        none enabled: the base single-TCP connection
// An empty or unrecognised value is treated as the default.
inline TransportKeys transport_keys(const std::string& rxpm) {
    TransportKeys k;
    if (rxpm == "Multicast")  { k.multicast = true; }
    else if (rxpm == "M-TCP") { k.rudp = false; k.tcp = true; }
    else if (rxpm == "UDP")   { k.rudp = false; k.unicast = true; }
    else if (rxpm == "TCP")   { k.rudp = false; }
    return k;
}

// True for the values the UI/API offers.
inline bool is_known_rxpm(const std::string& rxpm) {
    return rxpm == "RUDP" || rxpm == "Multicast" || rxpm == "M-TCP" ||
           rxpm == "UDP"  || rxpm == "TCP";
}

// JSON fragment holding the four receive objects, no trailing comma.
inline std::string transport_json(const std::string& rxpm) {
    const TransportKeys k = transport_keys(rxpm);
    auto b = [](bool v) { return v ? "true" : "false"; };
    auto obj = [&](const char* name, bool v) {
        return std::string("    \"") + name + "\": { \"recv\": { \"enable\": " + b(v) + " } }";
    };
    return obj("rudp", k.rudp) + ",\n" + obj("multicast", k.multicast) + ",\n" +
           obj("tcp", k.tcp) + ",\n" + obj("unicast", k.unicast) + "\n";
}

// transport_json() plus, when NDIMON_NDI_LOG_DIR is set, the SDK's own log
// (ndi.log.*): its timestamped lines include "Dropping a video packet and requesting
// a keyframe". Both processes must emit identical keys, so both call this.
inline std::string config_tail_json(const std::string& rxpm) {
    std::string t = transport_json(rxpm);
    const auto network = network_selection();
    if (network.managed) {
        // Callers refuse SDK initialization when this interface has no address.
        // Never represent a missing selected interface as automatic selection.
        t = "    \"adapters\": { \"allowed\": [\"" + network.address + "\"] },\n" + t;
    }
    const char* dir = std::getenv("NDIMON_NDI_LOG_DIR");
    if (dir && *dir) {
        std::string esc;
        for (const char* c = dir; *c; ++c) { if (*c == '"' || *c == '\\') esc += '\\'; esc += *c; }
        t.pop_back();  // trailing newline
        t += ",\n    \"log\": { \"enable\": true, \"level\": 5, \"dir\": \"" + esc + "\" }\n";
    }
    return t;
}

// Write-to-tmp-then-rename so the other process (or the SDK) never reads a
// half-written file. The tmp name carries the pid: both processes may write.
inline bool write_file_atomic(const std::string& path, const std::string& content) {
    const std::string tmp = path + ".tmp." + std::to_string(getpid());
    {
        std::ofstream f(tmp, std::ios::binary | std::ios::trunc);
        if (!f) return false;
        f << content;
        f.flush();
        if (!f) { f.close(); std::remove(tmp.c_str()); return false; }
    }
    if (std::rename(tmp.c_str(), path.c_str()) != 0) {
        std::remove(tmp.c_str());
        return false;
    }
    return true;
}

}  // namespace ndisdk
