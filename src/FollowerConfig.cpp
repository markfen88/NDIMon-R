#include "FollowerConfig.h"
#include <cmath>
#include <regex>
#include <stdexcept>

namespace follower {
using nlohmann::json;
static void require(bool ok, const std::string& error) {
    if (!ok) throw std::runtime_error(error);
}
static std::string text(const json& j, const char* key) {
    const auto s = j.at(key).get<std::string>();
    require(s.find('\0') == std::string::npos, std::string(key) + ": embedded NUL");
    return s;
}
static std::string choice(const json& j, const char* key,
                          std::initializer_list<const char*> values) {
    const auto s = text(j, key);
    for (const char* v : values) if (s == v) return s;
    throw std::runtime_error(std::string(key) + ": unsupported value");
}
static int integer(const json& j, int low, int high) {
    require(j.is_number_integer(), "expected integer");
    if (j.is_number_unsigned()) {
        auto n = j.get<uint64_t>();
        require(n >= static_cast<uint64_t>(low) && n <= static_cast<uint64_t>(high), "integer out of range");
        return static_cast<int>(n);
    }
    auto n = j.get<int64_t>();
    require(n >= low && n <= high, "integer out of range");
    return static_cast<int>(n);
}
static bool ipv4(const std::string& s) {
    in_addr address{};
    return inet_pton(AF_INET, s.c_str(), &address) == 1;
}
static bool host(const std::string& s) {
    if (s.empty() || s.size() > 253) return false;
    if (s.find_first_not_of("0123456789.") == std::string::npos) return ipv4(s);
    if (s.back() == '.') return false;
    std::istringstream labels(s);
    std::string label;
    while (std::getline(labels, label, '.')) {
        if (!std::regex_match(label, std::regex("[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?")))
            return false;
    }
    return true;
}
static std::string list(const json& j, bool addresses) {
    require(j.is_array(), "expected array");
    std::string result;
    for (const auto& item : j) {
        const auto s = item.get<std::string>();
        require(!s.empty() && s.find_first_of(",\r\n") == std::string::npos &&
                    s.find('\0') == std::string::npos, "invalid list entry");
        require(!addresses || ipv4(s), "expected IPv4 address");
        if (!result.empty()) result += ',';
        result += s;
    }
    return result;
}

DeviceConfig parse_device(const json& j) {
    DeviceConfig d;
    d.mode = choice(j, "mode", {"decode", "encode"});
    d.decode_mode = choice(j, "decode_mode", {"auto", "hardware", "software"});
    d.ndi_recv_name = text(j, "ndi_alias");
    d.host_name = text(j, "hostname");
    d.device_name = text(j.at("about"), "device_name");
    return d;
}

std::unordered_map<int, OutputConfig> parse_outputs(const json& j) {
    require(j.is_object() && j.size() <= 8, "expected at most eight outputs");
    std::unordered_map<int, OutputConfig> outputs;
    for (auto it = j.begin(); it != j.end(); ++it) {
        require(it.key().size() == 1 && it.key()[0] >= '1' && it.key()[0] <= '8', "invalid output number");
        const auto& v = it.value();
        OutputConfig o;
        o.output_alias = text(v, "alias");
        o.source_selection = text(v, "source_selection");
        require(o.source_selection == "NDI", "only NDI source selection is supported");
        const auto saver = choice(v, "screensaver", {"splash", "black", "capture"});
        o.screensaver_mode = saver == "splash" ? "SplashSS" : saver == "capture" ? "CaptureSS" : "BlackSS";
        const auto& video = v.at("video");
        o.scale_mode = choice(video, "scale_mode", {"letterbox", "stretch", "crop"});
        o.rotation = integer(video.at("rotation"), 0, 270);
        require(o.rotation == 0 || o.rotation == 180, "rotation must be 0 or 180");
        const auto& resolution = video.at("resolution");
        if (resolution != "auto") {
            require(resolution.is_object() && resolution.size() == 1, "invalid resolution");
            const auto& mode = resolution.at("mode");
            const int width = integer(mode.at("width"), 1, 16384);
            const int height = integer(mode.at("height"), 1, 16384);
            o.refresh_hz = mode.at("refresh_hz").get<double>();
            require(std::isfinite(o.refresh_hz) && o.refresh_hz >= 0, "invalid refresh_hz");
            o.preferred_mode = std::to_string(width) + "x" + std::to_string(height);
        }
        const auto& source = v.at("source");
        if (!source.is_null()) {
            o.source_name = text(source, "name");
            o.source_ip = text(source, "endpoint");
            require(!o.source_name.empty() && o.source_name != "None", "invalid source name");
            if (!o.source_ip.empty()) {
                const auto colon = o.source_ip.find(':');
                require(ipv4(o.source_ip.substr(0, colon)), "invalid source IPv4 endpoint");
                if (colon != std::string::npos) {
                    const auto port = o.source_ip.substr(colon + 1);
                    require(std::regex_match(port, std::regex("[0-9]{1,5}")) &&
                                std::stoi(port) >= 1 && std::stoi(port) <= 65535, "invalid source port");
                }
            }
        }
        outputs.emplace(it.key()[0] - '0', std::move(o));
    }
    return outputs;
}

NdiConfig parse_ndi(const json& j) {
    NdiConfig n;
    const auto receive = choice(j, "receive", {"rudp", "multicast", "multi_tcp", "udp", "tcp"});
    n.transport.rxpm = receive == "rudp" ? "RUDP" : receive == "multicast" ? "Multicast" :
                       receive == "multi_tcp" ? "M-TCP" : receive == "udp" ? "UDP" : "TCP";
    n.finder.discovery_server_ip = list(j.at("discovery_servers"), false);
    for (const auto& server : j.at("discovery_servers"))
        require(host(server.get<std::string>()), "invalid discovery server IP or hostname");
    n.finder.discovery_server_mode = n.finder.discovery_server_ip.empty() ? "NDIDisServDis" : "NDIDisServEn";
    n.finder.receiver_advertising = j.value("receiver_advertising", true);
    n.finder.remote_control = j.value("remote_control", true);
    n.group.groups = list(j.at("groups"), false);
    require(!n.group.groups.empty(), "at least one NDI group is required");
    n.off_subnet_ips = list(j.at("off_subnet_sources"), true);
    const auto tally = choice(j, "tally", {"on", "off", "video"});
    n.tally_mode = tally == "on" ? "TallyOn" : tally == "video" ? "VideoMode" : "TallyOff";
    n.color_space = choice(j, "color_space", {"yuv", "rgb"}) == "yuv" ? "YUV" : "RGB";
    return n;
}

AudioConfig parse_audio(const json& j, uint64_t revision) {
    AudioConfig a;
    a.engine = choice(j, "engine", {"v1", "v2"});
    // Reuse the file loader's validation and runtime projection; the desired
    // graph stores kind/nullable PCM and settings rather than legacy type/engine.
    // Legacy file revisions were limited to JavaScript's safe integer range;
    // the follower envelope is the Rust document's full u64 revision.
    json legacy = {{"version", a.engine == "v2" ? 2 : 1},
                   {"engine", j.at("settings")}, {"sources", j.at("sources")},
                   {"sinks", j.at("sinks")}, {"outputs", j.at("outputs")},
                   {"routes", j.at("routes")}, {"video_delay_ms", j.at("video_delay_ms")}};
    require(legacy["sources"].is_object() && legacy["sinks"].is_object() &&
                legacy["outputs"].is_object(), "invalid audio graph");
    for (auto& source : legacy["sources"]) {
        source["type"] = source.at("kind");
        source.erase("kind");
        if (source.at("alsa_pcm").is_null()) source.erase("alsa_pcm");
    }
    for (auto& sink : legacy["sinks"]) sink["type"] = "alsa";
    std::string error;
    a.settings = ndimon::audio::AudioSettings::parse(legacy, &error);
    require(error.empty(), error);
    a.settings.revision = revision;
    if (legacy["outputs"].contains("1"))
        a.primary_enabled = legacy["outputs"]["1"].at("enabled").get<bool>();
    return a;
}

DisplayConfig parse_display(const json& j) {
    DisplayConfig d;
    const auto& s = j.at("splash");
    // Preserve the legacy API's free-form color strings; the renderer chooses
    // its existing fallback for strings it cannot render.
    d.splash.bg_idle = text(s, "bg_idle");
    d.splash.bg_live = text(s, "bg_live");
    d.splash.accent_idle = text(s, "accent_idle");
    d.splash.accent_live = text(s, "accent_live");
    auto percent = [&](const char* key) {
        const auto n = s.at(key).get<double>();
        require(std::isfinite(n) && n >= 0 && n <= 100, "splash percentage out of range");
        return static_cast<float>(n);
    };
    d.splash.logo_x_pct = percent("logo_x_pct");
    d.splash.logo_y_pct = percent("logo_y_pct");
    d.splash.logo_w_pct = percent("logo_w_pct");
    d.splash.text_height_pct = percent("text_height_pct");
    d.splash.text_idle = text(s, "text_idle");
    d.splash.text_live = text(s, "text_live");
    d.splash.backdrop = choice(s, "backdrop", {"gradient", "flat"});
    d.splash.show_signal_text = s.at("show_signal_text").get<bool>();
    d.splash.show_device_name = s.at("show_device_name").get<bool>();
    d.splash.show_device_url = s.at("show_device_url").get<bool>();
    d.splash.show_sources_available = s.at("show_sources_available").get<bool>();
    if (!s.at("logo").is_null()) {
        const auto hash = text(s.at("logo"), "sha256");
        const auto media = choice(s.at("logo"), "media_type", {"image/png", "image/jpeg"});
        require(std::regex_match(hash, std::regex("[0-9a-f]{64}")), "invalid logo hash");
        d.splash.logo_path = "/var/lib/ndimon/assets/" + hash + (media == "image/png" ? ".png" : ".jpg");
    }
    d.osd.enabled = j.at("osd").at("enabled").get<bool>();
    d.osd.text = text(j.at("osd"), "text");
    size_t units = 0;
    for (unsigned char c : d.osd.text)
        if ((c & 0xc0) != 0x80) units += c >= 0xf0 ? 2 : 1;
    require(units <= 128, "OSD text exceeds 128 UTF-16 units");
    return d;
}

ndisdk::NetworkBinding parse_network(const json& j) {
    ndisdk::NetworkBinding binding;
    const auto base = text(j, "interface");
    require(base.empty() || std::regex_match(base, std::regex("[A-Za-z0-9_.-]{1,15}")), "invalid interface");
    const auto& networks = j.at("networks");
    require(networks.is_array(), "networks must be an array");
    const auto& roles = j.at("roles");
    const auto ndi = text(roles, "ndi"), management = text(roles, "management");
    if (base.empty() && j.at("backend").is_null() && networks.empty() &&
        ndi.empty() && management.empty() && text(roles, "audio").empty()) return binding;
    binding.managed = true;
    auto resolve = [&](const std::string& role) {
        if (role.empty()) return std::string{};
        for (const auto& n : networks) {
            if (text(n, "id") != role) continue;
            std::string name = base;
            if (!n.at("vlan_id").is_null())
                name += "." + std::to_string(integer(n.at("vlan_id"), 1, 4094));
            require(!name.empty() && name.size() <= 15, "invalid selected interface");
            return name;
        }
        throw std::runtime_error("role references missing network");
    };
    binding.ndi = resolve(ndi);
    binding.management = resolve(management);
    return binding;
}
} // namespace follower
