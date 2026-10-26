// NDI Source Finder - discovers NDI sources on the network and writes JSON files

#include <cstddef>
#include <Processing.NDI.Lib.h>
#include <nlohmann/json.hpp>
#include "../src/NdiSdkConfig.h"
#include "../src/FollowerConfig.h"
#include "../src/FollowerSocket.h"
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
#include <grp.h>
#include <pwd.h>
#include <cstdlib>

#ifdef HAVE_SYSTEMD
#include <systemd/sd-daemon.h>
#endif

static const char* PARAM_FILE    = "/run/ndimon/ndi_src_find_param";
static const char* SRC_LIST_FILE = "/run/ndimon/finder/ndimon-sources.json";
static const char* WEBUI_LIST    = "/run/ndimon/finder/NDIFinderSrcList.json";
static const char* FIND_SETTINGS = "/etc/ndimon-find-settings.json";
static const char* NDI_CONFIG    = "/etc/ndi-config.json";
static const char* NDI_GROUP     = "/etc/ndi-group.json";

static std::atomic<bool> g_running{true};
static follower::NdiFollower g_follower;

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
    // The fixed trigger is root-owned in the root-owned shared parent. Clear
    // it in place: finder must never replace it with a symlink for control.
    if (path == PARAM_FILE) {
        std::ofstream trigger(path, std::ios::trunc);
        if (trigger) trigger << content;
        return;
    }
    // Atomic write: tmp + rename so readers (ndimon-control) never see a partial
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
    std::string rxpm = ndisdk::kDefaultRxpm;  // TCP/UDP/Multicast/M-TCP/RUDP
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

static void pin_ndi_home() {
    ndisdk::pin_config_dir("finder");
}

// Write ~/.ndi/ndi-config.v1.json before NDIlib_initialize() so NDI SDK 6.2+
// picks up the discovery server address and groups at startup.
static void write_ndi_sdk_config(const FindSettings& s) {
    std::string ndi_dir = ndisdk::config_dir();
    mkdir(ndi_dir.c_str(), 0770);
    std::string path = ndi_dir + "/ndi-config.v1.json";

    std::string groups   = json_escape(s.groups.empty() ? "public" : s.groups);
    std::string extra    = json_escape(s.extra_ips);
    std::string discovery;
    if (s.use_discovery_server && !s.discovery_server_ip.empty())
        discovery = json_escape(s.discovery_server_ip);

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
        + ndisdk::config_tail_json(s.rxpm) +
        "  }\n"
        "}\n";

    if (ndisdk::file_has_content(path, json)) return;
    if (ndisdk::write_file_atomic(path, json)) {
        if (ndi_dir == "/var/lib/ndimon/.ndi") {
            // The emergency legacy fallback is still shared by the two users.
            if (auto* group = getgrnam("ndimon"))
                chown(path.c_str(), static_cast<uid_t>(-1), group->gr_gid);
            chmod(path.c_str(), 0664);
        } else chmod(path.c_str(), 0600);
        std::cout << "[ NDIFinder ] NDI config written: " << path << "\n";
    } else {
        std::cerr << "[ NDIFinder ] WARNING: could not write " << path << "\n";
    }
}

static std::string boot_setting(const char* name, const char* fallback) {
    const char* dir = std::getenv("CREDENTIALS_DIRECTORY");
    return dir && *dir ? std::string(dir) + "/" + name : fallback;
}

static FindSettings load_settings() {
    FindSettings s;
    if (g_follower.config) {
        const auto& n = *g_follower.config;
        s.use_discovery_server = n.finder.discovery_server_mode == "NDIDisServEn";
        s.discovery_server_ip = n.finder.discovery_server_ip;
        s.extra_ips = n.off_subnet_ips;
        s.groups = n.group.groups;
        s.rxpm = n.transport.rxpm;
        return s;
    }
    auto find = read_json(boot_setting("find-settings", FIND_SETTINGS));
    if (!find.empty() && find.is_object()) {
        s.use_discovery_server = (find.value("NDIDisServ", "") == "NDIDisServEn");
        s.discovery_server_ip  = find.value("NDIDisServIP", "");
    }
    auto cfg = read_json(boot_setting("ndi-config", NDI_CONFIG));
    if (!cfg.empty() && cfg.is_string()) s.extra_ips = cfg.get<std::string>();
    auto grp = read_json(boot_setting("ndi-group", NDI_GROUP));
    if (!grp.empty() && grp.is_object()) s.groups = grp.value("ndi_groups", "public");
    auto rx = read_json(boot_setting("receive-settings", "/etc/ndimon-rx-settings.json"));
    if (!rx.empty() && rx.is_object()) s.rxpm = rx.value("Rxpm", std::string(ndisdk::kDefaultRxpm));
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
    if (!ndisdk::network_selection().ready()) return;
    std::cout << "[ NDIFinder ] run ones\n";

    // Load settings and write NDI SDK config BEFORE NDIlib_initialize() so
    // NDI SDK 6.2+ uses the discovery server from the very start.
    auto settings = load_settings();
    write_ndi_sdk_config(settings);

    if (!NDIlib_initialize()) {
        std::cerr << "[ NDIFinder ] NDIlib_initialize failed\n";
        return;
    }

    write_ndi_sdk_config(settings);

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
    // The selected interface may still be waiting for its DHCP lease at boot:
    // wait for it instead of exiting and being restarted (NI-18, W3).
    const auto selected_ndi_network = ndisdk::wait_for_selection(
        [] { return ndisdk::network_selection(); },
        std::chrono::seconds(60), std::chrono::milliseconds(250),
        [](std::chrono::milliseconds waited) {
            if (!g_running) return false;
            if (waited.count() == 0)
                std::cout << "[NDIFinder] Waiting for the selected NDI network to get an address\n";
            return true;
        },
        [](std::chrono::milliseconds interval) { std::this_thread::sleep_for(interval); });
    if (!selected_ndi_network.ready()) {
        std::cerr << "[NDIFinder] Selected NDI network unavailable; refusing automatic fallback\n";
        return;
    }
    // Load settings and write NDI SDK config BEFORE NDIlib_initialize() so
    // NDI SDK 6.2+ uses the discovery server from the very start.
    auto settings = load_settings();
    write_ndi_sdk_config(settings);

    if (!NDIlib_initialize()) {
        std::cerr << "[ NDIFinder ] NDIlib_initialize failed\n";
        return;
    }

    write_ndi_sdk_config(settings);

    NDIlib_find_instance_t finder = nullptr;
    NDIlib_find_create_t create = {};
    std::string extra_ips_combined;
    auto recreate = [&]() {
        if (finder) { NDIlib_find_destroy(finder); finder = nullptr; }
        create = {};
        create.show_local_sources = true;
        create.p_groups = settings.groups.empty() ? nullptr : settings.groups.c_str();
        extra_ips_combined = settings.extra_ips;
        if (settings.use_discovery_server && !settings.discovery_server_ip.empty()) {
            extra_ips_combined = settings.discovery_server_ip;
            if (!settings.extra_ips.empty()) extra_ips_combined += "," + settings.extra_ips;
        }
        create.p_extra_ips = extra_ips_combined.empty() ? nullptr : extra_ips_combined.c_str();
        finder = NDIlib_find_create_v2(&create);
        return finder != nullptr;
    };
    if (!recreate()) { NDIlib_destroy(); return; }
    follower::Listener control;
    if (!control.start("/run/ndimon/finder/control.sock", "ndimon-finder"))
        std::cerr << "[NDIFinder] WARNING: cannot listen on /run/ndimon/finder/control.sock\n";

    std::vector<SrcEntry> last_list;

#ifdef HAVE_SYSTEMD
    sd_notify(0, "READY=1");
#endif
    std::cout << "[ NDIFinder ] continuous discovery running\n";
    // The loop serves the follower socket every slice and does the rest of its
    // work once per tick. The SDK's wait cannot be woken by a request, so it runs
    // in slices: a request waits for one slice, not for the whole tick (VV-2 N4).
    using Clock = std::chrono::steady_clock;
    constexpr auto kTick = std::chrono::milliseconds(500);
    constexpr auto kSlice = std::chrono::milliseconds(50);
    auto next_tick = Clock::now();
    auto next_watchdog = Clock::now() + std::chrono::seconds(10);
    while (g_running) {
        control.poll_all([&](const nlohmann::json& request) -> nlohmann::json {
            const auto action = request.value("action", "");
            if (action == "status" || action == "health")
                return {{"ok", true}, {"alive", finder != nullptr}, {"revision", g_follower.revision.status()}};
            if (action != "apply_config") return follower::failure("unknown action");
            auto result = g_follower.apply_config(request, [&](const follower::NdiConfig& n) {
                const auto previous = settings;
                NDIlib_find_destroy(finder);
                finder = nullptr;
                NDIlib_destroy();
                settings = {n.finder.discovery_server_mode == "NDIDisServEn",
                            n.finder.discovery_server_ip, n.off_subnet_ips,
                            n.group.groups, n.transport.rxpm};
                // Transport keys are read only at SDK init. Reuse the existing
                // SDK lifetime here, without a new process restart path.
                write_ndi_sdk_config(settings);
                if (NDIlib_initialize()) {
                    write_ndi_sdk_config(settings);
                    if (recreate()) { last_list.clear(); return true; }
                    NDIlib_destroy();
                }
                settings = previous;
                write_ndi_sdk_config(settings);
                if (NDIlib_initialize()) { write_ndi_sdk_config(settings); recreate(); }
                return false;
            });
            if (result.value("ok", false)) {
                if (!ndisdk::write_file_atomic(ndisdk::config_dir() + "/finder-config.json", request.dump()))
                    std::cerr << "[NDIFinder] WARNING: cannot cache follower snapshot\n";
                else chmod((ndisdk::config_dir() + "/finder-config.json").c_str(), 0600);
            }
            return result;
        });
        if (!finder) break;
        const bool tick = Clock::now() >= next_tick;
        if (tick) {
            next_tick = Clock::now() + kTick;
            if (ndisdk::network_selection().signature() != selected_ndi_network.signature()) break;
            // Check for command from param file
            std::string param = read_file(PARAM_FILE);
            if (param == "-run") {
                write_file(PARAM_FILE, "");
                // Once followed, load_settings() returns memory, never /etc.
                NDIlib_find_destroy(finder);
                finder = nullptr;
                settings = load_settings();
                write_ndi_sdk_config(settings);
                if (!recreate()) break;
                last_list.clear();
            } else if (param == "-rs") {
                write_file(PARAM_FILE, "");
                write_source_list({});
                last_list.clear();
            }
        }

        const bool sources_changed = NDIlib_find_wait_for_sources(finder, kSlice.count());
#ifdef HAVE_SYSTEMD
        if (Clock::now() >= next_watchdog) { // every ~10s
            sd_notify(0, "WATCHDOG=1");
            next_watchdog = Clock::now() + std::chrono::seconds(10);
        }
#endif
        if (!tick && !sources_changed) continue;
        uint32_t count = 0;
        const NDIlib_source_t* sources = NDIlib_find_get_current_sources(finder, &count);

        // Copy out immediately — the SDK's source pointers are invalidated by
        // the next finder call, so we must not retain them across iterations.
        std::vector<SrcEntry> current = copy_sources(sources, count);

        // Name or address. A sender can keep its NDI name and change IP.
        bool changed = (current.size() != last_list.size());
        if (!changed) {
            for (size_t i = 0; i < current.size(); i++) {
                if (current[i].name != last_list[i].name ||
                    current[i].ip != last_list[i].ip) {
                    changed = true;
                    break;
                }
            }
        }

        if (changed) {
            std::cout << "[ NDIFinder ] delta - list changed count=" << count << "\n";
            last_list = std::move(current);
            write_source_list(last_list);
        }
    }

    if (finder) NDIlib_find_destroy(finder);
    NDIlib_destroy();
}

int main(int argc, char* argv[]) {
    pin_ndi_home();
    // Unbuffered output for journald: through a pipe stdout is block-buffered,
    // and the log would appear only when the finder stops.
    std::cout << std::unitbuf;
    std::cerr << std::unitbuf;
    {
        std::ifstream cached(ndisdk::config_dir() + "/finder-config.json");
        if (cached) {
            try { g_follower.apply_config(nlohmann::json::parse(cached)); }
            catch (const std::exception& e) {
                std::cerr << "[NDIFinder] Cannot restore follower snapshot: " << e.what() << "\n";
            }
        }
    }
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
