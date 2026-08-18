// NDI Source Finder - discovers NDI sources on the network and writes JSON files

#include <cstddef>
#include <Processing.NDI.Lib.h>
#include <nlohmann/json.hpp>
#include <iostream>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>
#include <chrono>
#include <thread>
#include <csignal>
#include <atomic>
#include <cstring>
#include <cstdio>
#include <unistd.h>
#include <sys/stat.h>

#ifdef HAVE_SYSTEMD
#include <systemd/sd-daemon.h>
#endif

static const char* PARAM_FILE    = "/etc/ndi_src_find_param";
static const char* SRC_LIST_FILE = "/etc/ndimon-sources.json";
static const char* WEBUI_LIST    = "/etc/NDIFinderSrcList.json";
static const char* FIND_SETTINGS = "/etc/ndimon-find-settings.json";
static const char* NDI_CONFIG    = "/etc/ndi-config.json";
static const char* NDI_GROUP     = "/etc/ndi-group.json";

static std::atomic<bool> g_running{true};

static void signal_handler(int) {
    g_running = false;
}

static std::string read_file(const std::string& path) {
    try {
        std::ifstream f(path);
        if (!f.is_open()) return "";
        std::ostringstream ss;
        ss << f.rdbuf();
        return ss.str();
    } catch (...) { return ""; }
}

static void write_file(const std::string& path, const std::string& content) {
    // Atomic write: tmp + rename so readers (ndimon-api) never see a partial
    // file. rename(2) is atomic within a filesystem.
    try {
        std::string tmp = path + ".tmp";
        {
            std::ofstream f(tmp);
            if (!f.is_open()) return;
            f << content;
        }
        if (std::rename(tmp.c_str(), path.c_str()) != 0)
            std::remove(tmp.c_str());
    } catch (...) {}
}

// Owned copy of a discovered source. NDIlib_source_t pointers returned by
// NDIlib_find_get_current_sources are only valid until the next call into the
// finder, so we copy the strings immediately rather than holding the structs.
struct SrcEntry {
    std::string name;
    std::string ip;
};

static std::vector<SrcEntry> copy_sources(const NDIlib_source_t* sources, uint32_t count) {
    std::vector<SrcEntry> out;
    out.reserve(count);
    for (uint32_t i = 0; i < count; i++) {
        SrcEntry e;
        e.name = sources[i].p_ndi_name    ? sources[i].p_ndi_name    : "";
        e.ip   = sources[i].p_url_address ? sources[i].p_url_address : "";
        out.push_back(std::move(e));
    }
    return out;
}

static nlohmann::json read_json(const std::string& path) {
    try {
        auto s = read_file(path);
        if (s.empty()) return {};
        return nlohmann::json::parse(s);
    } catch (...) { return {}; }
}

struct FindSettings {
    bool use_discovery_server = false;
    std::string discovery_server_ip;
    std::string extra_ips;  // off-subnet IPs
    std::string groups;
    std::string rxpm = "RUDP";  // transport mode (TCP/UDP/Multicast/M-TCP/RUDP)
};

// Escape a string for embedding inside a JSON string literal.
static std::string json_escape(const std::string& s) {
    std::string out;
    out.reserve(s.size() + 8);
    for (char c : s) {
        switch (c) {
            case '"':  out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n";  break;
            case '\r': out += "\\r";  break;
            case '\t': out += "\\t";  break;
            default:
                if ((unsigned char)c < 0x20) { char b[8]; snprintf(b, sizeof(b), "\\u%04x", c); out += b; }
                else out += c;
        }
    }
    return out;
}

// Mirror of ndimon-r's transport mapping so the shared ndi-config.v1.json keeps
// consistent transport keys regardless of which process wrote it last.
static std::string transport_json(const std::string& rxpm) {
    bool rudp  = (rxpm == "RUDP" || rxpm == "Multicast" || rxpm.empty());
    bool mcast = (rxpm == "Multicast");
    std::string r = rudp ? "true" : "false";
    std::string m = mcast ? "true" : "false";
    return std::string("    \"rudp\": { \"recv\": { \"enable\": ") + r + " } },\n"
         + "    \"multicast\": { \"recv\": { \"enable\": " + m + " } },\n";
}

// Write ~/.ndi/ndi-config.v1.json before NDIlib_initialize() so NDI SDK 6.2+
// picks up the discovery server address and groups at startup.
static void write_ndi_sdk_config(const FindSettings& s) {
    const char* home = getenv("HOME");
    if (!home || !home[0]) home = "/root";
    std::string ndi_dir = std::string(home) + "/.ndi";
    mkdir(ndi_dir.c_str(), 0755);
    std::string path = ndi_dir + "/ndi-config.v1.json";

    std::string groups   = json_escape(s.groups.empty() ? "public" : s.groups);
    std::string extra    = json_escape(s.extra_ips);
    std::string discovery;
    if (s.use_discovery_server && !s.discovery_server_ip.empty())
        discovery = json_escape(s.discovery_server_ip);

    // Include codec passthrough keys so the NDI SDK delivers H.264/H.265 as
    // compressed bitstream rather than decoding internally. Without these,
    // the SDK renders a "Video Decoder not Found" error frame on Noble (Ubuntu
    // 24.04) where libavcodec.so.61 is absent. ndimon-r also writes this file,
    // but we write it here too so we never overwrite it without the codec keys.
    std::string json =
        "{\n"
        "  \"ndi\": {\n"
        "    \"groups\": {\n"
        "      \"recv\": \"" + groups + "\",\n"
        "      \"send\": \"" + groups + "\"\n"
        "    },\n"
        "    \"networks\": {\n"
        "      \"ips\": \"" + extra + "\",\n"
        "      \"discovery\": \"" + discovery + "\"\n"
        "    },\n"
        + transport_json(s.rxpm) +
        "    \"codec\": {\n"
        "      \"h264\": { \"passthrough\": true },\n"
        "      \"h265\": { \"passthrough\": true }\n"
        "    }\n"
        "  }\n"
        "}\n";

    std::ofstream f(path);
    if (f) { f << json; std::cout << "[ NDIFinder ] NDI config written: " << path << "\n"; }
    else   { std::cerr << "[ NDIFinder ] WARNING: could not write " << path << "\n"; }
}

static FindSettings load_settings() {
    FindSettings s;
    auto find = read_json(FIND_SETTINGS);
    if (!find.empty() && find.is_object()) {
        s.use_discovery_server = (find.value("NDIDisServ", "") == "NDIDisServEn");
        s.discovery_server_ip  = find.value("NDIDisServIP", "");
    }
    auto cfg = read_json(NDI_CONFIG);
    if (!cfg.empty() && cfg.is_string()) s.extra_ips = cfg.get<std::string>();
    auto grp = read_json(NDI_GROUP);
    if (!grp.empty() && grp.is_object()) s.groups = grp.value("ndi_groups", "public");
    auto rx = read_json("/etc/ndimon-rx-settings.json");
    if (!rx.empty() && rx.is_object()) s.rxpm = rx.value("Rxpm", "RUDP");
    return s;
}

static void write_source_list(const std::vector<SrcEntry>& sources) {
    nlohmann::json j;
    j["count"] = (int)sources.size() + 1;
    j["list"]["None"] = "None";
    for (auto& s : sources) {
        if (!s.name.empty()) j["list"][s.name] = s.ip;
    }
    write_file(SRC_LIST_FILE, j.dump());

    // Web UI list format
    nlohmann::json webui;
    webui["ndi"] = nlohmann::json::array();
    webui["ndi"].push_back("None");
    for (auto& s : sources) {
        if (!s.name.empty()) webui["ndi"].push_back(s.name);
    }
    write_file(WEBUI_LIST, webui.dump());

    std::cout << "[ NDIFinder ] Write List to " << SRC_LIST_FILE
              << " count=" << sources.size() << "\n";
}

static void run_once() {
    std::cout << "[ NDIFinder ] run ones\n";

    // Load settings and write NDI SDK config BEFORE NDIlib_initialize() so
    // NDI SDK 6.2+ uses the discovery server from the very start.
    auto settings = load_settings();
    write_ndi_sdk_config(settings);

    if (!NDIlib_initialize()) {
        std::cerr << "[ NDIFinder ] NDIlib_initialize failed\n";
        return;
    }

    NDIlib_find_create_t find_create = {};
    find_create.show_local_sources = true;
    find_create.p_groups = settings.groups.empty() ? nullptr : settings.groups.c_str();

    // Discovery server — combine DS IP + extra_ips so both are used
    std::string extra_ips_combined;
    if (settings.use_discovery_server && !settings.discovery_server_ip.empty()) {
        std::cout << "[ NDIFinder ] Discovery Server Enabled " << settings.discovery_server_ip << "\n";
        extra_ips_combined = settings.discovery_server_ip;
        if (!settings.extra_ips.empty())
            extra_ips_combined += "," + settings.extra_ips;
        find_create.p_extra_ips = extra_ips_combined.c_str();
    } else {
        std::cout << "[ NDIFinder ] Discovery Server Disabled\n";
        if (!settings.extra_ips.empty())
            find_create.p_extra_ips = settings.extra_ips.c_str();
    }

    NDIlib_find_instance_t finder = NDIlib_find_create_v2(&find_create);
    if (!finder) {
        std::cerr << "[ NDIFinder ] NDIlib_find_create_v2 failed\n";
        NDIlib_destroy();
        return;
    }

    std::cout << "[ NDIFinder ] NDIFinderFindSrcs\n";
    std::cout << "[ NDIFinder ] Looking for sources ...\n";

    // Wait up to 5 seconds for sources to appear
    for (int i = 0; i < 10 && g_running; i++) {
        NDIlib_find_wait_for_sources(finder, 500);
    }

    uint32_t count = 0;
    const NDIlib_source_t* sources = NDIlib_find_get_current_sources(finder, &count);
    std::cout << "[ NDIFinder ] Source count from network " << count << "\n";

    write_source_list(copy_sources(sources, count));

    NDIlib_find_destroy(finder);
    NDIlib_destroy();
}

static void run_continuous() {
    // Load settings and write NDI SDK config BEFORE NDIlib_initialize() so
    // NDI SDK 6.2+ uses the discovery server from the very start.
    auto settings = load_settings();
    write_ndi_sdk_config(settings);

    if (!NDIlib_initialize()) {
        std::cerr << "[ NDIFinder ] NDIlib_initialize failed\n";
        return;
    }

    NDIlib_find_create_t find_create = {};
    find_create.show_local_sources = true;
    find_create.p_groups = settings.groups.empty() ? nullptr : settings.groups.c_str();
    std::string extra_ips_combined;
    if (settings.use_discovery_server && !settings.discovery_server_ip.empty()) {
        extra_ips_combined = settings.discovery_server_ip;
        if (!settings.extra_ips.empty())
            extra_ips_combined += "," + settings.extra_ips;
        find_create.p_extra_ips = extra_ips_combined.c_str();
    } else if (!settings.extra_ips.empty())
        find_create.p_extra_ips = settings.extra_ips.c_str();

    NDIlib_find_instance_t finder = NDIlib_find_create_v2(&find_create);
    if (!finder) {
        NDIlib_destroy();
        return;
    }

    std::vector<SrcEntry> last_list;

#ifdef HAVE_SYSTEMD
    sd_notify(0, "READY=1");
#endif
    std::cout << "[ NDIFinder ] continuous discovery running\n";
    int wd_counter = 0;
    while (g_running) {
        // Check for command from param file
        std::string param = read_file(PARAM_FILE);
        if (param == "-run") {
            write_file(PARAM_FILE, "");
            // Re-read settings and recreate finder
            settings = load_settings();
            NDIlib_find_destroy(finder);
            find_create.p_groups = settings.groups.empty() ? nullptr : settings.groups.c_str();
            if (settings.use_discovery_server && !settings.discovery_server_ip.empty()) {
                extra_ips_combined = settings.discovery_server_ip;
                if (!settings.extra_ips.empty())
                    extra_ips_combined += "," + settings.extra_ips;
                find_create.p_extra_ips = extra_ips_combined.c_str();
            } else if (!settings.extra_ips.empty())
                find_create.p_extra_ips = settings.extra_ips.c_str();
            finder = NDIlib_find_create_v2(&find_create);
            if (!finder) break;
            last_list.clear();
        } else if (param == "-rs") {
            write_file(PARAM_FILE, "");
            write_source_list({});
            last_list.clear();
        }

        NDIlib_find_wait_for_sources(finder, 500);
#ifdef HAVE_SYSTEMD
        if (++wd_counter >= 20) { // every ~10s
            sd_notify(0, "WATCHDOG=1");
            wd_counter = 0;
        }
#endif
        uint32_t count = 0;
        const NDIlib_source_t* sources = NDIlib_find_get_current_sources(finder, &count);

        // Copy out immediately — the SDK's source pointers are invalidated by
        // the next finder call, so we must not retain them across iterations.
        std::vector<SrcEntry> current = copy_sources(sources, count);

        // Check if list changed (by name)
        bool changed = (current.size() != last_list.size());
        if (!changed) {
            for (size_t i = 0; i < current.size(); i++) {
                if (current[i].name != last_list[i].name) { changed = true; break; }
            }
        }

        if (changed) {
            std::cout << "[ NDIFinder ] delta - list changed count=" << count << "\n";
            last_list = std::move(current);
            write_source_list(last_list);
        }
    }

    NDIlib_find_destroy(finder);
    NDIlib_destroy();
}

int main(int argc, char* argv[]) {
    signal(SIGINT,  signal_handler);
    signal(SIGTERM, signal_handler);

    std::string mode = "continuous";
    if (argc > 1) mode = argv[1];

    if (mode == "-ls") {
        std::cout << "[ NDIFinder ] List source file:\n";
        std::cout << read_file(SRC_LIST_FILE) << "\n";
        return 0;
    } else if (mode == "-rs") {
        write_source_list({});
        run_once();
        return 0;
    } else if (mode == "-run") {
        run_once();
        return 0;
    }

    // Default: continuous mode (run as service)
    run_continuous();
    return 0;
}
