#include "Config.h"
#include "FollowerConfig.h"
#include <optional>
#include <fstream>
#include <iostream>
#include <cstdio>
#include <cstring>
#include <cerrno>
#include <fcntl.h>
#include <grp.h>
#include <sys/stat.h>
#include <unistd.h>

static const char* DEC1_SETTINGS    = "/etc/ndimon-dec1-settings.json";
static const char* SPLASH_SETTINGS  = "/etc/ndimon-splash-settings.json";
static const char* RX_SETTINGS      = "/etc/ndimon-rx-settings.json";
static const char* FIND_SETTINGS    = "/etc/ndimon-find-settings.json";
static const char* DEVICE_SETTINGS  = "/etc/ndimon-device-settings.json";
static const char* NDI_CONFIG       = "/etc/ndi-config.json";
static const char* NDI_GROUP        = "/etc/ndi-group.json";
static const char* OSD_SETTINGS     = "/etc/ndimon-osd-settings.json";

Config& Config::instance() {
    static Config cfg;
    return cfg;
}

nlohmann::json Config::read_json(const std::string& path) {
    try {
        std::ifstream f(path);
        if (!f.is_open()) return {};
        return nlohmann::json::parse(f);
    } catch (...) {
        return {};
    }
}

void Config::write_json(const std::string& path, const nlohmann::json& j) {
    const std::string tmp = path + ".tmp";
    try {
        {
            std::ofstream f(tmp);
            if (!f.is_open()) throw std::runtime_error("cannot open " + tmp);
            f << j.dump(2);
        }
        auto* group = getgrnam("ndimon");
        int fd = open(tmp.c_str(), O_RDONLY | O_NOFOLLOW);
        if (fd < 0) throw std::runtime_error("cannot reopen config temporary");
        bool secured = group && fchown(fd, 0, group->gr_gid) == 0 && fchmod(fd, 0640) == 0 && fsync(fd) == 0;
        close(fd);
        if (!secured) throw std::runtime_error("cannot secure config temporary");
        if (std::rename(tmp.c_str(), path.c_str()) != 0)
            throw std::runtime_error("rename failed: " + std::string(strerror(errno)));
    } catch (const std::exception& e) {
        std::cerr << "[Config] write_json " << path << ": " << e.what() << "\n";
        std::remove(tmp.c_str());
    }
}

std::string Config::settings_path(int ch_num) {
    return "/etc/ndimon-dec" + std::to_string(ch_num) + "-settings.json";
}

void Config::load() {
    std::lock_guard<std::mutex> lk(mutex_);
    // Disk source-of-truth changed — drop per-output cache so next get_output
    // re-reads from disk.
    if (!followed_.count("outputs")) output_cache_.clear();

    // Decoder settings (ch1 / legacy)
    auto dec = followed_.count("outputs") && followed_.count("ndi") && followed_.count("audio")
        ? nlohmann::json{} : read_json(DEC1_SETTINGS);
    if (!dec.empty() && dec.is_object()) {
        if (!followed_.count("audio")) decoder.ndi_audio = dec.value("NDIAudio", "NDIAudioEn");
        if (!followed_.count("outputs")) {
            decoder.ch_num           = dec.value("ChNum", 1);
            decoder.screensaver_mode = dec.value("ScreenSaverMode", "BlackSS");
            decoder.source_selection = dec.value("SourceSelection", "NDI");
            connected_source_name    = dec.value("SourceName", "");
            connected_source_ip      = dec.value("SourceIP", "");
        }
        if (!followed_.count("ndi")) {
            decoder.tally_mode = dec.value("TallyMode", "TallyOff");
            decoder.color_space = dec.value("ColorSpace", "YUV");
        }
    }

    // Transport
    auto rx = followed_.count("ndi") ? nlohmann::json{} : read_json(RX_SETTINGS);
    if (!rx.empty() && rx.is_object()) {
        transport.rxpm = rx.value("Rxpm", std::string("RUDP"));
    }

    // Finder / discovery server
    auto find = followed_.count("ndi") ? nlohmann::json{} : read_json(FIND_SETTINGS);
    if (!find.empty() && find.is_object()) {
        finder.discovery_server_mode = find.value("NDIDisServ", "NDIDisServDis");
        finder.discovery_server_ip   = find.value("NDIDisServIP", "");
    }

    // Device
    auto dev = followed_.count("device") && followed_.count("audio") ? nlohmann::json{} : read_json(DEVICE_SETTINGS);
    if (!dev.empty() && dev.is_object()) {
        if (!followed_.count("device")) {
            device.mode          = dev.value("mode", "decode");
            device.video_output  = dev.value("videooutput", "hdmi");
            device.ndi_recv_name = dev.value("ndi_recv_name", "");
            device.decode_mode   = dev.value("decode_mode", "auto");
        }
        if (!followed_.count("audio")) {
            device.audio_engine = dev.value("audio_engine", "v1");
            if (device.audio_engine != "v2") device.audio_engine = "v1";
        }
    }

    // NDI groups
    auto grp = followed_.count("ndi") ? nlohmann::json{} : read_json(NDI_GROUP);
    if (!grp.empty() && grp.is_object()) {
        ndi_group.groups = grp.value("ndi_groups", "public");
    }

    // Off-subnet IPs
    auto cfg = followed_.count("ndi") ? nlohmann::json{} : read_json(NDI_CONFIG);
    if (!cfg.empty() && cfg.is_string()) {
        off_subnet_ips = cfg.get<std::string>();
    }

    // Splash screen config
    auto sp = followed_.count("display") ? nlohmann::json{} : read_json(SPLASH_SETTINGS);
    if (!sp.empty() && sp.is_object()) {
        splash.bg_idle    = sp.value("bg_idle",    "#0D1B2A");
        splash.bg_live    = sp.value("bg_live",    "#0D2B1A");
        splash.accent_idle = sp.value("accent_idle", "#4488CC");
        splash.accent_live = sp.value("accent_live", "#22FF88");
        splash.logo_path  = sp.value("logo_path",  "");
        splash.logo_x_pct = sp.value("logo_x_pct", 50.0f);
        splash.logo_y_pct = sp.value("logo_y_pct", 50.0f);
        splash.logo_w_pct = sp.value("logo_w_pct", 50.0f);
        splash.text_idle  = sp.value("text_idle",  "No Signal");
        splash.text_live  = sp.value("text_live",  "Signal Available");
        splash.text_height_pct = sp.value("text_height_pct", 4.0f);
        // show_box from older settings files is ignored; the backdrop replaced it.
        splash.backdrop          = sp.value("backdrop",          "gradient");
        splash.show_signal_text  = sp.value("show_signal_text",  true);
        splash.show_device_name  = sp.value("show_device_name",  true);
        splash.show_device_url   = sp.value("show_device_url",   true);
        splash.show_sources_available = sp.value("show_sources_available", true);
    }

    // OSD config
    auto od = followed_.count("display") ? nlohmann::json{} : read_json(OSD_SETTINGS);
    if (!od.empty() && od.is_object()) {
        osd.enabled = od.value("enabled", false);
        osd.text    = od.value("text",    "");
    }
}

void Config::save_device() {
    std::lock_guard<std::mutex> lk(mutex_);
    if (followed_.count("device")) return;
    // Only persist device-level settings that the C++ side owns.
    // All other config files are owned by ndimon-control.
    auto j = read_json(DEVICE_SETTINGS);
    if (j.empty()) j = nlohmann::json::object();
    j["mode"]          = device.mode;
    j["videooutput"]   = device.video_output;
    j["ndi_recv_name"] = device.ndi_recv_name;
    write_json(DEVICE_SETTINGS, j);
}

// ---------------------------------------------------------------------------
// Per-output config
// ---------------------------------------------------------------------------
OutputConfig Config::get_output(int ch_num) const {
    std::lock_guard<std::mutex> lk(mutex_);

    auto it = output_cache_.find(ch_num);
    if (it != output_cache_.end()) return it->second;

    OutputConfig out;
    if (followed_.count("outputs")) return out;
    auto j = read_json(settings_path(ch_num));
    if (!j.empty() && j.is_object()) {
        out.preferred_mode = j.value("videooutput", "");
        out.scale_mode     = j.value("ScaleMode", "letterbox");
        out.source_name    = j.value("SourceName", "");
        out.source_ip      = j.value("SourceIP", "");
        out.output_alias   = j.value("output_alias", "");
        out.rotation       = j.value("rotation", 0u);
    } else if (ch_num == 1) {
        // Fall back to legacy dec1 settings
        out.preferred_mode = device.video_output;
        out.source_name    = connected_source_name;
        out.source_ip      = connected_source_ip;
        out.scale_mode     = "letterbox";
    }
    output_cache_[ch_num] = out;
    return out;
}

void Config::set_runtime_ip(const std::string& ip) {
    std::lock_guard<std::mutex> lk(mutex_);
    device.device_ip = ip;
}

std::string Config::runtime_ip() const {
    std::lock_guard<std::mutex> lk(mutex_);
    return device.device_ip;
}

void Config::set_output(int ch_num, const OutputConfig& out) {
    std::lock_guard<std::mutex> lk(mutex_);
    // The control plane owns desired selections/modes after handover. Runtime
    // observations (DS routing, unsupported rotation) must not rewrite them.
    if (followed_.count("outputs")) return;

    // Read existing JSON to preserve other fields
    std::string path = settings_path(ch_num);
    nlohmann::json j = read_json(path);
    if (j.empty()) j = nlohmann::json::object();

    j["videooutput"]  = out.preferred_mode;
    j["ScaleMode"]    = out.scale_mode;
    j["SourceName"]   = out.source_name;
    j["SourceIP"]     = out.source_ip;
    j["output_alias"] = out.output_alias;
    j["rotation"]     = out.rotation;

    write_json(path, j);
    output_cache_[ch_num] = out;

    // Keep legacy in-memory fields in sync for ch1 (used by get_output fallback)
    if (ch_num == 1) {
        connected_source_name = out.source_name;
        connected_source_ip   = out.source_ip;
        device.video_output   = out.preferred_mode;
    }
}

nlohmann::json Config::applied_revision() const {
    std::lock_guard<std::mutex> lk(mutex_);
    return revision_.status();
}

nlohmann::json Config::follower_snapshot() const {
    std::lock_guard<std::mutex> lk(mutex_);
    return {{"action", "apply_config"}, {"revision", revision_.value}, {"config", followed_config_}};
}

bool Config::follows(const std::string& domain) const {
    std::lock_guard<std::mutex> lk(mutex_);
    return followed_.count(domain) != 0;
}

ndimon::audio::AudioSettings Config::audio_settings(std::string* error) const {
    std::lock_guard<std::mutex> lk(mutex_);
    if (followed_.count("audio")) {
        if (error) error->clear();
        return audio_settings_;
    }
    return ndimon::audio::AudioSettings::load("/etc/ndimon-audio-settings.json", error);
}

nlohmann::json Config::apply_config(const nlohmann::json& request, AudioApply apply_audio) {
    using nlohmann::json;
    std::lock_guard<std::mutex> lk(mutex_);
    try {
        const uint64_t next = revision_.check(request);
        const auto& config = request.at("config");
        if (!config.is_object() || config.empty())
            return follower::failure("config: expected non-empty domain object");
        std::optional<DeviceConfig> dev;
        std::optional<std::unordered_map<int, OutputConfig>> outputs;
        std::optional<follower::NdiConfig> ndi;
        std::optional<follower::AudioConfig> audio;
        std::optional<follower::DisplayConfig> display;
        std::optional<ndisdk::NetworkBinding> network;
        json errors = json::array();
        for (auto it = config.begin(); it != config.end(); ++it) {
            try {
                if (it.key() == "device") dev = follower::parse_device(it.value());
                else if (it.key() == "outputs") outputs = follower::parse_outputs(it.value());
                else if (it.key() == "ndi") ndi = follower::parse_ndi(it.value());
                else if (it.key() == "audio") audio = follower::parse_audio(it.value(), next);
                else if (it.key() == "display") display = follower::parse_display(it.value());
                else if (it.key() == "network") network = follower::parse_network(it.value());
                else throw std::runtime_error("unknown follower domain");
            } catch (const std::exception& e) {
                errors.push_back(it.key() + ": " + e.what());
            }
        }
        if (!errors.empty()) return {{"ok", false}, {"errors", errors}};
        if (audio && apply_audio) {
            const auto error = apply_audio(audio->settings);
            if (!error.empty()) return follower::failure("audio: " + error);
        }
        if (dev) {
            // Audio and observed address have independent owners.
            dev->audio_engine = device.audio_engine;
            dev->device_ip = device.device_ip;
            device = *dev;
        }
        if (outputs) {
            output_cache_ = *outputs;
            const auto first = output_cache_.find(1);
            connected_source_name = first == output_cache_.end() ? "" : first->second.source_name;
            connected_source_ip = first == output_cache_.end() ? "" : first->second.source_ip;
            decoder.screensaver_mode = first == output_cache_.end() ? "BlackSS" : first->second.screensaver_mode;
            decoder.source_selection = first == output_cache_.end() ? "NDI" : first->second.source_selection;
        }
        if (ndi) {
            transport = ndi->transport;
            finder = ndi->finder;
            ndi_group = ndi->group;
            off_subnet_ips = ndi->off_subnet_ips;
            decoder.tally_mode = ndi->tally_mode;
            decoder.color_space = ndi->color_space;
        }
        if (audio) {
            device.audio_engine = audio->engine;
            audio_settings_ = audio->settings;
            decoder.ndi_audio = audio->primary_enabled ? "NDIAudioEn" : "NDIAudioDis";
        }
        if (display) { splash = display->splash; osd = display->osd; }
        if (network) ndisdk::set_network_binding(*network);
        for (auto it = config.begin(); it != config.end(); ++it) {
            followed_.insert(it.key());
            followed_config_[it.key()] = it.value();
        }
        revision_.commit(next);
        return {{"ok", true}, {"revision", next}};
    } catch (const std::exception& e) {
        return follower::failure(e.what());
    }
}
