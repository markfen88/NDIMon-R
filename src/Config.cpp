#include "Config.h"
#include <fstream>
#include <iostream>

static const char* DEC1_SETTINGS    = "/etc/ndimon-dec1-settings.json";
static const char* SPLASH_SETTINGS  = "/etc/ndimon-splash-settings.json";
static const char* RX_SETTINGS      = "/etc/ndimon-rx-settings.json";
static const char* FIND_SETTINGS    = "/etc/ndimon-find-settings.json";
static const char* NDI_SRC          = "/etc/ndimon-sources.json";
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
    try {
        std::ofstream f(path);
        f << j.dump(2);
    } catch (const std::exception& e) {
        std::cerr << "[Config] write_json " << path << ": " << e.what() << "\n";
    }
}

std::string Config::settings_path(int ch_num) {
    return "/etc/ndimon-dec" + std::to_string(ch_num) + "-settings.json";
}

void Config::load() {
    // Decoder settings (ch1 / legacy)
    auto dec = read_json(DEC1_SETTINGS);
    if (!dec.empty() && dec.is_object()) {
        decoder.ch_num           = dec.value("ChNum", 1);
        decoder.ndi_audio        = dec.value("NDIAudio", "NDIAudioEn");
        decoder.screensaver_mode = dec.value("ScreenSaverMode", "BlackSS");
        decoder.tally_mode       = dec.value("TallyMode", "TallyOff");
        decoder.color_space      = dec.value("ColorSpace", "YUV");
        decoder.source_selection = dec.value("SourceSelection", "NDI");
        connected_source_name    = dec.value("SourceName", "");
        connected_source_ip      = dec.value("SourceIP", "");
    }

    // Transport
    auto rx = read_json(RX_SETTINGS);
    if (!rx.empty() && rx.is_object()) {
        transport.rxpm = rx.value("Rxpm", "TCP");
    }

    // Finder / discovery server
    auto find = read_json(FIND_SETTINGS);
    if (!find.empty() && find.is_object()) {
        finder.discovery_server_mode = find.value("NDIDisServ", "NDIDisServDis");
        finder.discovery_server_ip   = find.value("NDIDisServIP", "");
    }

    // Device
    auto dev = read_json(DEVICE_SETTINGS);
    if (!dev.empty() && dev.is_object()) {
        device.mode          = dev.value("mode", "decode");
        device.video_output  = dev.value("videooutput", "hdmi");
        device.ndi_recv_name = dev.value("ndi_recv_name", "");
    }

    // NDI groups
    auto grp = read_json(NDI_GROUP);
    if (!grp.empty() && grp.is_object()) {
        ndi_group.groups = grp.value("ndi_groups", "public");
    }

    // Off-subnet IPs
    auto cfg = read_json(NDI_CONFIG);
    if (!cfg.empty() && cfg.is_string()) {
        off_subnet_ips = cfg.get<std::string>();
    }

    // Current source (from src json) — informational only
    (void)read_json(NDI_SRC);

    // Splash screen config
    auto sp = read_json(SPLASH_SETTINGS);
    if (!sp.empty() && sp.is_object()) {
        splash.bg_idle    = sp.value("bg_idle",    "#0D1B2A");
        splash.bg_live    = sp.value("bg_live",    "#0D2B1A");
        splash.accent_idle = sp.value("accent_idle", "#4488CC");
        splash.accent_live = sp.value("accent_live", "#22FF88");
        splash.logo_path  = sp.value("logo_path",  "");
        splash.logo_x_pct = sp.value("logo_x_pct", 50.0f);
        splash.logo_y_pct = sp.value("logo_y_pct", 40.0f);
        splash.logo_w_pct = sp.value("logo_w_pct", 30.0f);
        splash.text_idle  = sp.value("text_idle",  "No Signal");
        splash.text_live  = sp.value("text_live",  "Signal Available");
        splash.text_x_pct = sp.value("text_x_pct", 50.0f);
        splash.text_y_pct = sp.value("text_y_pct", 62.0f);
        splash.text_scale = sp.value("text_scale", 3);
        splash.show_box   = sp.value("show_box",   true);
    }

    // OSD config
    auto od = read_json(OSD_SETTINGS);
    if (!od.empty() && od.is_object()) {
        osd.enabled = od.value("enabled", false);
        osd.text    = od.value("text",    "");
    }
}

void Config::save() {
    nlohmann::json dec;
    dec["ChNum"]           = decoder.ch_num;
    dec["NDIAudio"]        = decoder.ndi_audio;
    dec["ScreenSaverMode"] = decoder.screensaver_mode;
    dec["TallyMode"]       = decoder.tally_mode;
    dec["ColorSpace"]      = decoder.color_space;
    dec["SourceSelection"] = decoder.source_selection;
    dec["SourceName"]      = connected_source_name;
    dec["SourceIP"]        = connected_source_ip;
    dec["videooutput"]     = device.video_output;
    dec["ScaleMode"]       = "letterbox";
    write_json(DEC1_SETTINGS, dec);

    nlohmann::json rx;
    rx["Rxpm"] = transport.rxpm;
    write_json(RX_SETTINGS, rx);

    // Find settings (DS IP, DS mode) and NDI groups are managed by the
    // Node.js API — do NOT write them here or we'll overwrite user changes.

    nlohmann::json dev;
    dev["mode"]          = device.mode;
    dev["videooutput"]   = device.video_output;
    dev["ndi_recv_name"] = device.ndi_recv_name;
    write_json(DEVICE_SETTINGS, dev);

}

void Config::save_splash() {
    nlohmann::json j;
    j["bg_idle"]     = splash.bg_idle;
    j["bg_live"]     = splash.bg_live;
    j["accent_idle"] = splash.accent_idle;
    j["accent_live"] = splash.accent_live;
    j["logo_path"]   = splash.logo_path;
    j["logo_x_pct"]  = splash.logo_x_pct;
    j["logo_y_pct"]  = splash.logo_y_pct;
    j["logo_w_pct"]  = splash.logo_w_pct;
    j["text_idle"]   = splash.text_idle;
    j["text_live"]   = splash.text_live;
    j["text_x_pct"]  = splash.text_x_pct;
    j["text_y_pct"]  = splash.text_y_pct;
    j["text_scale"]  = splash.text_scale;
    j["show_box"]    = splash.show_box;
    write_json(SPLASH_SETTINGS, j);
}

void Config::save_osd() {
    nlohmann::json j;
    j["enabled"] = osd.enabled;
    j["text"]    = osd.text;
    write_json(OSD_SETTINGS, j);
}

// ---------------------------------------------------------------------------
// Per-output config
// ---------------------------------------------------------------------------
OutputConfig Config::get_output(int ch_num) const {
    OutputConfig out;
    auto j = read_json(settings_path(ch_num));
    if (!j.empty() && j.is_object()) {
        out.preferred_mode = j.value("videooutput", "");
        out.scale_mode     = j.value("ScaleMode", "letterbox");
        out.source_name    = j.value("SourceName", "");
        out.source_ip      = j.value("SourceIP", "");
        out.output_alias   = j.value("output_alias", "");
    } else if (ch_num == 1) {
        // Fall back to legacy dec1 settings
        out.preferred_mode = device.video_output;
        out.source_name    = connected_source_name;
        out.source_ip      = connected_source_ip;
        out.scale_mode     = "letterbox";
    }
    return out;
}

void Config::set_output(int ch_num, const OutputConfig& out) {
    // Read existing JSON to preserve other fields
    std::string path = settings_path(ch_num);
    nlohmann::json j = read_json(path);
    if (j.empty()) j = nlohmann::json::object();

    j["videooutput"]  = out.preferred_mode;
    j["ScaleMode"]    = out.scale_mode;
    j["SourceName"]   = out.source_name;
    j["SourceIP"]     = out.source_ip;
    j["output_alias"] = out.output_alias;

    write_json(path, j);

    // Keep legacy in-memory fields in sync for ch1 (used by get_output fallback)
    if (ch_num == 1) {
        connected_source_name = out.source_name;
        connected_source_ip   = out.source_ip;
        device.video_output   = out.preferred_mode;
        // NOTE: save() is intentionally NOT called here.
        // The merged write above already persisted SourceName/SourceIP correctly.
        // save() would overwrite the merged write with a stripped-down version,
        // losing fields like output_alias, and re-write FIND_SETTINGS/RX_SETTINGS
        // from potentially stale in-memory values.
    }
}
