#include "AlsaAudio.h"
#include "AutoResolutionPolicy.h"
#include "Config.h"
#include "DRMDisplay.h"
#include "DelayedFrameQueue.h"
#include "IPCServer.h"
#include "NDIReceiver.h"
#include "NDIVideoLayout.h"
#include "NdiSdkConfig.h"
#include "PlatformDetect.h"
#include "Telemetry.h"
#include "VideoDecoder.h"
#include "audio/AlsaSink.h"
#include "audio/AlsaSource.h"
#include "audio/AudioEngine.h"
#include "audio/AudioMasterLog.h"
#include "audio/AudioSettings.h"
#include "audio/HdmiPcm.h"
#include "audio/NdiAudioSource.h"
#include "audio/UnavailableAudio.h"

#include <set>
#include <iostream>
#include <memory>
#include <thread>
#include <atomic>
#include <csignal>
#include <cstdlib>
#include <cstdio>
#include <string>
#include <mutex>
#include <condition_variable>
#include <queue>
#include <set>
#include <vector>
#include <map>
#include <chrono>
#include <fstream>
#include <iterator>
#include <algorithm>
#include <dirent.h>
#include <unistd.h>
#include <fcntl.h>
#include <grp.h>
#include <pwd.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <ifaddrs.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <drm_fourcc.h>
#include <xf86drm.h>
#include <Processing.NDI.Lib.h>

#ifdef HAVE_SYSTEMD
#include <systemd/sd-daemon.h>
#endif

static std::atomic<bool> g_running{true};
static int64_t now_ms() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now().time_since_epoch()).count();
}

// Write ~/.ndi/ndi-config.v1.json so the NDI SDK uses the discovery server
// and correct groups for BOTH finding sources AND registering this receiver.
// Must be called before NDIlib_initialize().
// Escape a string for safe embedding inside a JSON string literal.
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
    ndisdk::pin_config_dir("decoder");
}

// pin_ndi_home() must already have run (once, at the top of main, before any
// thread exists): setenv() here would race with SDK threads calling getenv().
static void write_ndi_sdk_config(const Config& cfg) {
    std::string ndi_dir = ndisdk::config_dir();
    mkdir(ndi_dir.c_str(), 0770);
    std::string path = ndi_dir + "/ndi-config.v1.json";

    std::string groups = json_escape(cfg.ndi_group.groups.empty() ? "public" : cfg.ndi_group.groups);
    std::string ips    = json_escape(cfg.off_subnet_ips);
    std::string discovery;
    if (cfg.finder.discovery_server_mode == "NDIDisServEn" &&
        !cfg.finder.discovery_server_ip.empty()) {
        discovery = json_escape(cfg.finder.discovery_server_ip);
    }

    // Build JSON manually to avoid pulling nlohmann into this helper.
    std::string json =
        "{\n"
        "  \"ndi\": {\n"
        "    \"groups\": {\n"
        "      \"recv\": \"" + groups + "\",\n"
        "      \"send\": \"" + groups + "\"\n"
        "    },\n"
        "    \"networks\": {\n"
        "      \"ips\": \"" + ips + "\",\n"
        "      \"discovery\": \"" + discovery + "\"\n"
        "    },\n"
        + ndisdk::config_tail_json(cfg.transport.rxpm) +
        "  }\n"
        "}\n";

    // Every apply_config calls this; only a change is written and logged.
    if (ndisdk::file_has_content(path, json)) return;
    if (ndisdk::write_file_atomic(path, json)) {
        if (ndi_dir == "/var/lib/ndimon/.ndi") {
            // The emergency legacy fallback is still shared by the two users.
            if (auto* group = getgrnam("ndimon"))
                chown(path.c_str(), static_cast<uid_t>(-1), group->gr_gid);
            chmod(path.c_str(), 0664);
        } else chmod(path.c_str(), 0600);
        std::cout << "[NDIMon-R] NDI config written (transport="
                  << cfg.transport.rxpm << "): " << path << "\n";
    } else {
        std::cerr << "[NDIMon-R] WARNING: could not write " << path << "\n";
    }
}

// The splash must advertise the web interface's selected management address.
static std::string get_primary_ip() {
    const auto management = ndisdk::network_selection("/etc/ndimon-network-interfaces.json", "management");
    if (management.managed) return management.address;
    struct ifaddrs* ifa = nullptr;
    getifaddrs(&ifa);
    std::string result;
    for (auto* p = ifa; p; p = p->ifa_next) {
        if (!p->ifa_addr || p->ifa_addr->sa_family != AF_INET) continue;
        auto* sin = reinterpret_cast<struct sockaddr_in*>(p->ifa_addr);
        uint32_t addr = ntohl(sin->sin_addr.s_addr);
        if ((addr >> 24) == 127) continue;  // skip loopback
        char buf[INET_ADDRSTRLEN];
        inet_ntop(AF_INET, &sin->sin_addr, buf, sizeof(buf));
        result = buf;
        break;
    }
    if (ifa) freeifaddrs(ifa);
    return result;
}

// Returns last 4 hex digits of the first non-loopback LAN MAC address (e.g. "A1B2").
// Used to build the default NDI alias "NDIMON-XXXX".
static std::string get_primary_mac_suffix() {
    // Enumerate /sys/class/net and use the first real wired interface's MAC,
    // rather than guessing from a hardcoded (ARM-biased) name list. Skip
    // loopback, virtual, and wireless interfaces; prefer ethernet (type 1).
    auto mac_suffix_from = [](const std::string& iface) -> std::string {
        std::ifstream f("/sys/class/net/" + iface + "/address");
        if (!f) return {};
        std::string mac; std::getline(f, mac);
        std::string raw;
        for (char c : mac) if (c != ':') raw += c;
        if (raw.size() < 4 || raw == "000000000000") return {};
        std::string suffix = raw.substr(raw.size() - 4);
        for (auto& c : suffix) c = (char)toupper((unsigned char)c);
        return suffix;
    };

    DIR* d = opendir("/sys/class/net");
    if (d) {
        std::vector<std::string> names;
        struct dirent* e;
        while ((e = readdir(d)) != nullptr) {
            std::string n = e->d_name;
            if (n == "." || n == ".." || n == "lo") continue;
            if (n.rfind("veth", 0) == 0 || n.rfind("docker", 0) == 0 ||
                n.rfind("br-", 0) == 0  || n.rfind("virbr", 0) == 0 ||
                n.rfind("wl", 0) == 0)  // skip wireless
                continue;
            names.push_back(n);
        }
        closedir(d);
        std::sort(names.begin(), names.end());  // stable, deterministic pick
        for (const auto& n : names) {
            auto s = mac_suffix_from(n);
            if (!s.empty()) return s;
        }
    }
    return "0000";
}

static void signal_handler(int) {
    g_running = false;
}

// Format a DRM connector name into a short human-readable output label.
// "HDMI-A-1" -> "HDMI-1", "HDMI-A-2" -> "HDMI-2", "DP-1" -> "DP-1"
static std::string default_output_alias(const std::string& conn) {
    if (conn.rfind("HDMI-A-", 0) == 0) return "HDMI-" + conn.substr(7);
    return conn;
}

// ===========================================================================
// DisplayWorker — owns one output: DRM display + NDI receiver + decoder + audio
// ===========================================================================
class DisplayWorker {
public:
    DisplayWorker(const std::string& connector_name,
                  const std::string& device,
                  const std::string& preferred_mode,
                  ScaleMode scale,
                  bool has_audio,
                  int ch_num,
                  int lease_fd = -1)
        : connector_name_(connector_name)
        , device_(device)
        , preferred_mode_(preferred_mode)
        , resolution_policy_(preferred_mode.empty() || preferred_mode == "auto")
        , scale_mode_(scale)
        , has_audio_(has_audio)
        , ch_num_(ch_num)
        , lease_fd_(lease_fd)
    {}

    void set_audio_engine(ndimon::audio::AudioEngine* engine) { engine_ = engine; }
    void update_audio_capture() {
        if (!recv_)
            return;
        const bool legacy_enabled = Config::instance().decoder.ndi_audio == "NDIAudioEn";
        recv_->set_audio_enabled(engine_ ? engine_->needs_ndi_audio(ch_num_) &&
                                               (engine_->uses_output_graph() || legacy_enabled)
                                         : audio_ && legacy_enabled);
    }
    const std::string& connector_name() const { return connector_name_; }
    int pull_audio(int frames,int channels,float* const* planes,int* captured_channels) { return recv_ ? recv_->pull_audio(frames,channels,planes,captured_channels) : 0; }
    int video_delay_cap_ms() {
        std::lock_guard<std::mutex> lock(frame_mtx_);
        return frame_queue_.max_delay_ms();
    }
    int effective_video_delay_ms() {
        std::lock_guard<std::mutex> lock(frame_mtx_);
        return frame_queue_.effective_delay_ms();
    }
    void set_video_delay(int ms) {
        std::lock_guard<std::mutex> lock(frame_mtx_);
        frame_queue_.set_delay_ms(engine_ && has_audio_ ? ms : 0);
        frame_cv_.notify_one();
    }
    void set_ipc(IPCServer* ipc) { ipc_ = ipc; }

    // Replay a Discovery Server route the control plane may have missed (it
    // was restarting) while this output is still on that source.
    void replay_remote_source() {
        if (!ipc_) return;
        nlohmann::json ev;
        {
            std::lock_guard<std::mutex> lk(source_mutex_);
            if (!remote_origin_) return;
            ev = remote_source_event(source_name_);
        }
        ipc_->push_event(ev);
    }

    nlohmann::json remote_source_event(const std::string& name) const {
        return {{"type", "remote_source"}, {"output", ch_num_ - 1}, {"source", name}};
    }

    // The control plane already holds this source because the Discovery
    // Server put the receiver on it: connecting again would only interrupt it.
    bool on_remote_source(const std::string& name, const std::string& ip) const {
        std::lock_guard<std::mutex> lk(source_mutex_);
        return remote_origin_ && ip.empty() && name == source_name_;
    }

    // Push current connection state to newly connected subscriber so the control
    // supervisor can start a retry loop immediately if the saved source is unavailable.
    // drm_ready is included so the supervisor won't start a reconnect loop for an
    // output with no physical display (avoids fighting with workers that have
    // no display but have a saved source from a previous Discovery Server route).
    void push_connection_state() {
        if (!ipc_) return;
        nlohmann::json ev;
        ev["type"]      = "connection";
        ev["output"]    = ch_num_ - 1;
        ev["connected"] = connected_.load();
        ev["source"]    = connected_.load() ? last_source_ : "";
        ev["drm_ready"] = drm_ready();
        ipc_->push_event(ev);
    }

    ~DisplayWorker() { stop(); }

    bool start() {
        // Create DRM display
        drm_ = std::make_unique<DRMDisplay>();
        drm_->set_scale_mode(scale_mode_);
        bool drm_ok;
        if (lease_fd_ >= 0) {
            drm_ok = drm_->init(lease_fd_, connector_name_, preferred_mode_, device_);
        } else {
            drm_ok = drm_->init(device_, connector_name_, preferred_mode_);
        }
        if (!drm_ok) {
            std::cerr << "[Worker" << ch_num_ << "] DRM init failed for "
                      << connector_name_ << " — waiting for hotplug\n";
            // Don't destroy drm_ — check_hotplug() needs the fd
        } else {
            // Apply persisted rotation before first splash
            apply_saved_rotation();
            drm_->show_splash(false);
            std::cout << "[Worker" << ch_num_ << "] DRM ready: "
                      << drm_->width() << "x" << drm_->height()
                      << "@" << drm_->refresh() << "\n";
        }

        // Create video decoder
        {
            std::lock_guard<std::mutex> lk(decoder_mutex_);
            decode_mode_ = Config::instance().device.decode_mode;
            decoder_ = VideoDecoder::create();
            if (!decoder_) {
                std::cerr << "[Worker" << ch_num_ << "] No video decoder available\n";
            } else {
                decoder_->set_frame_callback([this](DecodedFrame& f) {
                    on_decoded(f);
                });
            }
        }

        // Create audio (only one worker gets audio)
        if (!engine_ && has_audio_ && !audio_in_use_.exchange(true)) {
            auto audio = std::make_unique<AlsaAudio>();
            std::string hdmi_dev = AlsaAudio::find_hdmi_device(connector_name_);
            if (audio->init(hdmi_dev, 48000, 2)) {
                audio_ = std::move(audio);
                std::cout << "[Worker" << ch_num_ << "] ALSA audio initialized\n";
            } else {
                std::cerr << "[Worker" << ch_num_ << "] ALSA init failed\n";
                audio_in_use_ = false;
            }
        }

        // Create NDI receiver
        recv_ = std::make_unique<NDIReceiver>();
        recv_->set_audio_pull_mode(engine_ != nullptr);
        update_audio_capture();

        recv_->set_video_callback([this](NDIVideoFrame& vf) {
            on_video(vf);
        });
        if (DropTelemetry::enabled()) {
            telemetry_ = std::make_unique<DropTelemetry>(ch_num_);
            recv_->set_pull_callback([this](int64_t ts) { telemetry_->on_pull(ts); });
        }
        recv_->set_audio_callback([this](NDIAudioFrame& af) {
            on_audio(af);
        });
        recv_->set_connection_callback([this](bool connected, const std::string& src) {
            on_connection(connected, src);
        });
        recv_->set_remote_source_callback([this](const std::string& name) {
            // The SDK already switched the receiver on a Discovery Server
            // command. Record it and tell the control plane, which saves it as
            // this output's source; it must never be answered with a connect.
            {
                std::lock_guard<std::mutex> lk(source_mutex_);
                source_name_ = name;
                source_ip_.clear();
                remote_origin_ = !name.empty();
                if (!name.empty()) last_source_ = name;
            }
            std::cout << "[Worker" << ch_num_ << "] Discovery Server "
                      << (name.empty() ? "disconnected the output" : "routed to: " + name) << "\n";
            if (ipc_) ipc_->push_event(remote_source_event(name));
        });

        // Auto-connect to last source
        auto& cfg = Config::instance();
        auto out_cfg = cfg.get_output(ch_num_);
        {
            std::lock_guard<std::mutex> lk(source_mutex_);
            source_name_ = out_cfg.source_name;
            source_ip_   = out_cfg.source_ip;
        }

        // Set recv name (device alias + output alias) — visible in NDI discovery tools
        recv_->set_recv_name(build_recv_name());

        // Seed transport so the first reload_config doesn't spuriously recreate
        // the recv (the SDK already read this from ndi-config.v1.json at startup).
        recv_->set_transport(cfg.transport.rxpm);

        if (cfg.finder.discovery_server_mode == "NDIDisServEn" &&
            !cfg.finder.discovery_server_ip.empty()) {
            recv_->set_discovery_server(cfg.finder.discovery_server_ip);
        }
        recv_->set_advertising(cfg.finder.receiver_advertising, cfg.finder.remote_control,
                               build_recv_name());

        // Always create and register the NDI receiver so this device appears
        // as a receiver in the DS regardless of display state.
        // Connecting to a source is deferred until DRM is ready.
        ndi_started_ = true;
        recv_->init_recv();

        if (drm_ok) {
            if (!out_cfg.source_name.empty() && out_cfg.source_name != "None") {
                std::cout << "[Worker" << ch_num_ << "] Auto-connecting to: "
                          << out_cfg.source_name << "\n";
                connect_source(out_cfg.source_name, out_cfg.source_ip);
            }
        } else {
            std::cout << "[Worker" << ch_num_ << "] No display — NDI receiver registered, awaiting hotplug\n";
        }

        fps_last_time_ = std::chrono::steady_clock::now();

        // Start dedicated display thread for uncompressed frames
        display_running_ = true;
        display_thr_ = std::thread(&DisplayWorker::display_loop, this);

        return true;
    }

    void stop() {
        if (engine_)
            engine_->unregister_ndi(ch_num_); // revoke and acknowledge this receiver's pulls
        // Stop display thread first so it doesn't race with drm_ teardown
        display_running_ = false;
        frame_cv_.notify_all();
        if (display_thr_.joinable()) display_thr_.join();
        drain_queued_frames();

        if (recv_) {
            recv_->disconnect();
            recv_.reset();
        }
        {
            std::lock_guard<std::mutex> lk(decoder_mutex_);
            if (decoder_) {
                decoder_->flush();
                decoder_->destroy();
                decoder_.reset();
            }
        }
        if (audio_) {
            audio_->destroy();
            audio_.reset();
            audio_in_use_ = false;
        }
        if (drm_) {
            if (drm_->is_initialized()) drm_->show_black();
            drm_->destroy();
            drm_.reset();
        }
    }

    void connect_source(const std::string& name, const std::string& ip) {
        bool real_source = !name.empty() && name != "None";
        // Refuse to connect when no display is attached — the NDI receiver
        // stays registered with DS (visible/monitorable) but won't stream.
        if (real_source && !drm_ready()) {
            std::cout << "[Worker" << ch_num_ << "] No display — ignoring connect request for \"" << name << "\"\n";
            return;
        }
        // Mark as not connected immediately — status queries will report
        // disconnected until the new source establishes its connection.
        connected_ = false;
        {
            std::lock_guard<std::mutex> lk(source_mutex_);
            // A retry of the routed source keeps its origin; anything else is local.
            remote_origin_ = remote_origin_ && real_source && name == source_name_;
            source_name_ = name;
            source_ip_   = ip;
        }

        // Build properties metadata to send to the source on connection:
        // device identity and audio/video/general configuration.
        if (recv_ && real_source) {
            auto& cfg = Config::instance();
            auto out  = cfg.get_output(ch_num_);
            bool audio_en  = (cfg.decoder.ndi_audio    == "NDIAudioEn");
            bool tally_on  = (cfg.decoder.tally_mode   == "TallyOn");
            std::string mode = out.preferred_mode.empty() ? "auto" : out.preferred_mode;
            std::string alias = out.output_alias.empty()
                ? default_output_alias(connector_name_) : out.output_alias;

            // Device
            std::string xml =
                "<ndi_product long_name=\"" + cfg.device.ndi_recv_name + "\""
                " short_name=\"" + alias + "\""
                " manufacturer=\"NDI Decoder\""
                " model=\"NDI Decoder\""
                " session=\"" + alias + "\""
                " serial=\"" + cfg.device.ndi_recv_name + "\"/>";

            // Audio
            xml += std::string("<ndi_audio_setup")
                + " enabled=\"" + (audio_en ? "true" : "false") + "\""
                + " channels=\"2\" sample_rate=\"48000\" bit_depth=\"32\"/>";

            // Video
            xml += "<ndi_video_setup"
                   " output=\"" + mode + "\""
                   " connector=\"" + connector_name_ + "\""
                   " scale_mode=\"" + out.scale_mode + "\"/>";

            // General
            xml += std::string("<ndi_general_setup")
                + " tally=\""       + cfg.decoder.tally_mode       + "\""
                + " screensaver=\"" + cfg.decoder.screensaver_mode  + "\""
                + " color_space=\"" + cfg.decoder.color_space       + "\""
                + " ndi_group=\""   + cfg.ndi_group.groups          + "\""
                + " tally_on=\""    + (tally_on ? "true" : "false") + "\"/>";

            recv_->set_connect_metadata(xml);
        }

        // Show splash immediately for any real source switch — gives visual feedback
        // during the switch and covers the case where the new source is unavailable.
        // Video will overlay once the new source starts streaming.
        if (real_source && drm_ && drm_->is_initialized()) {
            drm_->set_streaming(false);
            drm_->show_splash(false);
        }

        if (recv_) recv_->connect(name, ip);

        if (!real_source) {
            // Disconnecting — clear streaming flag so splash can render
            if (drm_) drm_->set_streaming(false);
            if (drm_ && drm_->is_initialized()) drm_->show_splash(false);
        }

        // Always persist the source selection so config reloads and discovery
        // server routing heartbeats cannot override a deliberate "None" choice.
        // (Transient signal-loss disconnects use disconnect_source(), which
        // intentionally keeps the last source in config for auto-reconnect.)
        auto& cfg = Config::instance();
        OutputConfig oc = cfg.get_output(ch_num_);
        oc.source_name = real_source ? name : "";
        oc.source_ip   = real_source ? ip   : "";
        cfg.set_output(ch_num_, oc);

        // connect() has stopped the recv thread, so no new callback is in
        // on_video. Drop frames from the previous source.
        drain_queued_frames();
    }

    void disconnect_source() {
        if (recv_) recv_->disconnect();
        // Explicitly update connected_ flag so status queries reflect the
        // disconnect immediately. recv_->disconnect() restarts the thread with
        // was_connected=false so conn_cb_ won't fire; we must do it here.
        connected_ = false;
        if (ipc_) {
            nlohmann::json ev;
            ev["type"]      = "connection";
            ev["output"]    = ch_num_ - 1;
            ev["connected"] = false;
            ev["source"]    = "";
            ev["drm_ready"] = drm_ready();
            ipc_->push_event(ev);
        }
        // Drop both queues so a stale HX frame cannot put the old picture
        // back up after splash. Saved source and the connection event stay.
        drain_queued_frames();
        if (drm_) drm_->set_streaming(false);
        // Schedule splash render on main-loop tick() instead of blocking the
        // IPC thread — at 4K the software fill can take hundreds of ms.
        splash_requested_ = true;
        // Do NOT clear source from persistent config so the device can
        // auto-reconnect after a reboot or transient signal loss.
    }

    void forget_source() {
        // disconnect_source handles drain + splash + IPC event + connected_ flag.
        // forget additionally clears in-memory source and persists empty source.
        disconnect_source();
        {
            std::lock_guard<std::mutex> lk(source_mutex_);
            source_name_.clear();
            source_ip_.clear();
            remote_origin_ = false;
        }
        auto& cfg = Config::instance();
        OutputConfig oc = cfg.get_output(ch_num_);
        oc.source_name = "";
        oc.source_ip   = "";
        cfg.set_output(ch_num_, oc);
    }

    void set_tally(bool program, bool preview) {
        if (recv_) recv_->set_tally(program, preview);
    }

    void set_audio_enabled(bool en) {
        if (recv_) recv_->set_audio_enabled(en && (audio_ != nullptr || engine_ != nullptr));
    }

    void set_discovery_server(const std::string& ip) {
        if (recv_) recv_->set_discovery_server(ip);
    }

    void reload_config() {
        auto& cfg = Config::instance();
        auto out_cfg = cfg.get_output(ch_num_);
        {
            std::string error;
            auto audio = cfg.audio_settings(&error);
            if (error.empty()) set_video_delay(audio.video_delay_ms);
        }

        // Update scale mode
        ScaleMode sm = ScaleMode::Letterbox;
        if (out_cfg.scale_mode == "stretch") sm = ScaleMode::Stretch;
        else if (out_cfg.scale_mode == "crop") sm = ScaleMode::Crop;
        if (sm != scale_mode_) {
            scale_mode_ = sm;
            if (drm_) drm_->set_scale_mode(sm);
        }

        // Rotation
        apply_saved_rotation();

        // OSD
        if (drm_) {
            drm_->set_osd_enabled(cfg.osd.enabled);
            drm_->set_osd_text(cfg.osd.text);
        }

        // Discovery server — recreate advertiser if DS address changed
        if (recv_) {
            std::string ds;
            if (cfg.finder.discovery_server_mode == "NDIDisServEn" &&
                !cfg.finder.discovery_server_ip.empty())
                ds = cfg.finder.discovery_server_ip;
            if (ds != recv_->get_discovery_server()) {
                write_ndi_sdk_config(cfg);   // update ndi-config.v1.json
                recv_->reload_discovery(ds); // recreate advertiser with new DS
            }
            recv_->set_advertising(cfg.finder.receiver_advertising, cfg.finder.remote_control,
                                   build_recv_name());
        }

        // Transport mode — the SDK reads transport settings from
        // ndi-config.v1.json when it starts, not when a receiver is created
        // (measured on the RK3588: recreating the receiver kept the old
        // transport). Apply a change by leaving cleanly; systemd restarts the
        // decoder (Restart=always) and it reconnects to the saved source.
        if (recv_ && cfg.transport.rxpm != recv_->get_transport()) {
            std::cout << "[Worker" << ch_num_ << "] transport '" << recv_->get_transport()
                      << "' -> '" << cfg.transport.rxpm << "' — restarting to apply\n";
            write_ndi_sdk_config(cfg);
            g_running = false;
        }

        // Legacy tally is global, including receivers on outputs other than ch1.
        // Apply both on and off to an already connected source.
        if (recv_ && connected_.load())
            recv_->set_tally(cfg.decoder.tally_mode == "TallyOn", false);

        // One output owns ALSA; the legacy setting controls that owner.
        update_audio_capture();

        // Decode mode (auto/hardware/software) — recreate the decoder so the
        // factory re-selects the backend. The next compressed frame re-inits it.
        if (cfg.device.decode_mode != decode_mode_) {
            std::cout << "[Worker" << ch_num_ << "] decode_mode '" << decode_mode_
                      << "' -> '" << cfg.device.decode_mode << "' — rebuilding decoder\n";
            decode_mode_ = cfg.device.decode_mode;
            std::queue<DecodedFrame> pending;
            std::lock_guard<std::mutex> dk(decoder_mutex_);
            {
                std::lock_guard<std::mutex> fk(frame_mtx_);
                pending.swap(hx_queue_);
            }
            while (!pending.empty()) {
                DecodedFrame old = std::move(pending.front());
                pending.pop();
                if (decoder_) decoder_->release_frame(old);
            }
            if (decoder_) { decoder_->flush(); decoder_->destroy(); decoder_.reset(); }
            decoder_ = VideoDecoder::create();
            if (decoder_) {
                decoder_->set_frame_callback([this](DecodedFrame& f) { on_decoded(f); });
            }
            decoder_initialized_ = false;   // force re-init on next frame
            codec_name_ = "none";
        }

        // Update NDI recv name if device or output alias changed.
        // rename() destroys and recreates the recv instance with the new name,
        // re-registers it with the advertiser, and reconnects to the source.
        // Skip while actively streaming — renaming mid-stream causes a reconnect
        // churn if the config reload happens immediately after connect_source saves
        // the new source (the alias write triggers rename → reconnect loop).
        if (recv_) {
            std::string new_name = build_recv_name();
            if (recv_->get_recv_name() != new_name) {
                if (connected_.load()) {
                    std::cerr << "[Worker" << ch_num_
                              << "] alias change deferred (streaming): '"
                              << recv_->get_recv_name() << "' -> '"
                              << new_name << "' — disconnect to apply\n";
                } else {
                    recv_->rename(new_name);
                }
            }
        }
    }

    void show_splash(bool source_available) {
        if (drm_ && drm_->is_initialized())
            drm_->show_splash(source_available);
    }

    bool is_streaming() const {
        return drm_ && drm_->is_streaming();
    }

    ResolutionResult set_resolution(uint32_t w, uint32_t h, double refresh_hz, int64_t deadline_monotonic_ms) {
        if (!drm_ || !drm_->is_initialized()) return ResolutionResult::Failed;
        return resolution_policy_.set_manual(
            [&] { return drm_->set_mode(w, h, refresh_hz, deadline_monotonic_ms); }, [&] {
            // Manual resolution change — disable auto mode and persist
            auto& cfg = Config::instance();
            OutputConfig oc = cfg.get_output(ch_num_);
            oc.preferred_mode = std::to_string(w) + "x" + std::to_string(h);
            cfg.set_output(ch_num_, oc);
            std::cout << "[Worker" << ch_num_ << "] Resolution set (manual): "
                      << w << "x" << h << "@" << refresh_hz << "\n";
        }, deadline_monotonic_ms);
    }

    // Reset to auto mode — output will match source resolution/framerate
    ResolutionResult set_auto_resolution(int64_t deadline_monotonic_ms) {
        return resolution_policy_.enable_auto([&] {
            auto& cfg = Config::instance();
            OutputConfig oc = cfg.get_output(ch_num_);
            oc.preferred_mode = "";
            cfg.set_output(ch_num_, oc);
            std::cout << "[Worker" << ch_num_ << "] Resolution set to auto\n";
        }, deadline_monotonic_ms);
    }

    nlohmann::json get_modes() const {
        nlohmann::json j;
        j["modes"] = nlohmann::json::array();
        j["auto_resolution"] = resolution_policy_.enabled();
        j["rotation"] = drm_ ? drm_->rotation() : 0;
        j["supported_rotations"] = drm_ ? drm_->supported_rotations() : std::vector<uint32_t>{0};
        j["current"]["width"]      = drm_ ? drm_->width()      : 0;
        j["current"]["height"]     = drm_ ? drm_->height()     : 0;
        j["current"]["refresh"]    = drm_ ? drm_->refresh()    : 0;
        j["current"]["refresh_hz"] = drm_ ? drm_->refresh_hz() : 0.0;
        if (drm_) {
            for (const auto& m : drm_->list_modes()) {
                nlohmann::json mode;
                mode["width"]      = m.width;
                mode["height"]     = m.height;
                mode["refresh"]    = m.refresh;
                mode["refresh_hz"] = m.refresh_hz;
                mode["name"]       = m.name;
                mode["preferred"]  = m.preferred;
                j["modes"].push_back(mode);
            }
        }
        return j;
    }

    void set_scale_mode(ScaleMode m) {
        scale_mode_ = m;
        if (drm_) drm_->set_scale_mode(m);
        auto& cfg = Config::instance();
        OutputConfig oc = cfg.get_output(ch_num_);
        if (m == ScaleMode::Stretch)   oc.scale_mode = "stretch";
        else if (m == ScaleMode::Crop) oc.scale_mode = "crop";
        else                           oc.scale_mode = "letterbox";
        cfg.set_output(ch_num_, oc);
    }

    bool rotation_busy() const { return drm_ && drm_->rotation_busy(); }

    void apply_saved_rotation() {
        if (!drm_ || !drm_->is_initialized()) return;
        auto& cfg = Config::instance();
        auto output = cfg.get_output(ch_num_);
        if (output.rotation == drm_->rotation()) return;
        if (!drm_->rotation_allowed(output.rotation)) {
            std::cerr << "[Worker" << ch_num_ << "] Saved rotation " << output.rotation
                      << " unsupported; retaining " << drm_->rotation() << " degrees\n";
            output.rotation = drm_->rotation();
            cfg.set_output(ch_num_, output);
        } else if (!drm_->set_rotation(output.rotation)) {
            // A transient format/TEST_ONLY/busy failure must not erase a valid
            // saved preference. Only the renderer policy normalizes settings.
            std::cerr << "[Worker" << ch_num_ << "] Saved rotation " << output.rotation
                      << " not applied yet; keeping the saved preference\n";
        }
    }

    bool set_rotation(uint32_t degrees) {
        if (!drm_) return false;
        if (!drm_->set_rotation(degrees)) return false;
        auto& cfg = Config::instance();
        OutputConfig oc = cfg.get_output(ch_num_);
        oc.rotation = degrees;
        cfg.set_output(ch_num_, oc);
        return true;
    }

    bool compressed_frame_observed() const { return hx_stream_.load(); }

    nlohmann::json get_status() const {
        nlohmann::json s;
        s["ch_num"]       = ch_num_;
        s["output_index"] = ch_num_ - 1;   // 0-based index for API routing
        s["connector"]    = connector_name_;
        {
            auto out_cfg = Config::instance().get_output(ch_num_);
            s["output_alias"] = out_cfg.output_alias.empty()
                ? default_output_alias(connector_name_)
                : out_cfg.output_alias;
        }
        s["recv_name"]    = recv_ ? recv_->get_recv_name() : build_recv_name();
        s["device_name"]  = Config::instance().device.ndi_recv_name;
        s["drm_ready"]    = drm_ready();
        s["compressed_frame_observed"] = compressed_frame_observed();
        s["connected"]    = connected_.load();
        s["audio_enabled"] = recv_ && recv_->audio_enabled();
        s["audio_available"] = audio_ != nullptr || engine_ != nullptr;
        s["audio_underruns"] = audio_ ? audio_->underruns() : 0;
        {
            std::lock_guard<std::mutex> lk(source_mutex_);
            s["source_name"] = source_name_;
            s["source_origin"] = remote_origin_ ? "discovery_server" : "local";
        }
        s["input_width"]  = input_w_.load();
        s["input_height"] = input_h_.load();
        s["fps"]          = fps_.load();
        {
            int total = 0, dropped = 0;
            if (recv_) recv_->get_performance(total, dropped);
            s["total_frames"]   = total;
            s["dropped_frames"] = dropped;
        }
        s["width"]        = drm_ ? drm_->width()      : 0;
        s["height"]       = drm_ ? drm_->height()     : 0;
        s["refresh"]      = drm_ ? drm_->refresh()    : 0;
        s["refresh_hz"]   = drm_ ? drm_->refresh_hz() : 0.0;
        {
            std::lock_guard<std::mutex> lk(decoder_mutex_);
            bool receiving = connected_.load() && last_video_frame_ms_.load() > 0 && codec_name_ != "none";
            s["codec"] = receiving ? codec_name_ : "none";
            if (receiving && codec_name_ == "uncompressed") {
                s["decode_backend"] = "ndi-sdk";
                s["hw_decode"] = false;
            } else {
                bool active = receiving && decoder_ && decoder_initialized_;
                s["decode_backend"] = active ? decoder_->backend_name() : "none";
                s["hw_decode"] = active && decoder_->is_hardware();
            }
        }
        s["decode_fps"]       = decode_fps_.load();
        s["decode_saturated"] = decode_saturated_.load();
        std::string sm = "letterbox";
        if (scale_mode_ == ScaleMode::Stretch) sm = "stretch";
        else if (scale_mode_ == ScaleMode::Crop) sm = "crop";
        s["scale_mode"] = sm;
        s["rotation"]   = drm_ ? drm_->rotation() : 0;
        // Stream type: Standard (SpeedHQ/UYVY) vs HX (H.264/H.265)
        if (recv_) {
            auto st = recv_->stream_type();
            s["stream_type"] = (st == NDIStreamType::HX)       ? "HX"
                             : (st == NDIStreamType::Standard)  ? "Standard"
                             :                                    "unknown";
        } else {
            s["stream_type"] = "unknown";
        }
        return s;
    }

    std::string conn_name() const { return connector_name_; }
    int         ch_num()    const { return ch_num_; }
    bool is_connected()     const { return connected_.load(); }
    double get_fps()        const { return fps_.load(); }

    bool drm_ready() const {
        return drm_ && drm_->is_initialized();
    }

    // Health accessors for IPC health endpoint
    int64_t video_stale_ms() const {
        auto t = last_video_frame_ms_.load();
        return t > 0 ? now_ms() - t : -1;
    }
    int64_t decoded_stale_ms() const {
        auto t = last_decoded_frame_ms_.load();
        return t > 0 ? now_ms() - t : -1;
    }
    int64_t display_stale_ms() const {
        auto t = last_display_commit_ms_.load();
        return t > 0 ? now_ms() - t : -1;
    }
    int64_t recv_stale_ms() const {
        auto t = last_recv_alive_ms_.load();
        return t > 0 ? now_ms() - t : -1;
    }
    int stall_count() const { return stall_count_; }

    std::string health_state() const {
        if (!connected_.load()) return "idle";
        if (stall_count_ >= 60) return "stalled";
        if (stall_count_ > 0) return "degraded";
        return "ok";
    }

    // Redraw the splash after the address or the pending-network line changes.
    void note_network_hint() {
        if (!connected_.load()) splash_requested_ = true;
    }

    void tick() {
        // Render splash asynchronously — set by disconnect_source/forget_source
        // so the IPC thread isn't blocked by the 4K software fill.
        if (splash_requested_.load()) {
            if (drm_ && drm_->is_initialized()) {
                if (drm_->show_splash(false)) {
                    splash_requested_ = false;
                    std::cout << "[Worker" << ch_num_ << "] Splash rendered (async)\n";
                }
                // If show_splash returned false (mutex contention), retry next tick
            } else {
                splash_requested_ = false;
            }
        }

        // If DS routed us to a source via allow_controlling but we have no display,
        // immediately disconnect — we don't want to consume an NDI slot with no output.
        if (connected_.load() && !drm_ready()) {
            std::cout << "[Worker" << ch_num_ << "] No display — dropping DS-assigned connection\n";
            disconnect_source();
        }

        if (engine_)
            engine_->set_master_fallback(ch_num_, drm_ready());
        // Hotplug check
        if (drm_ && !drm_->is_initialized()) {
            if (resolution_policy_.reinitialize([&] { return drm_->check_hotplug(); })) {
                std::cout << "[Worker" << ch_num_ << "] Display connected via hotplug\n";
                apply_saved_rotation();
                drm_->show_splash(connected_.load());
                // NDI receiver was already registered at start() — just connect
                // to the configured source now that the display is ready.
                std::string sname, sip;
                {
                    std::lock_guard<std::mutex> lk(source_mutex_);
                    sname = source_name_;
                    sip   = source_ip_;
                }
                if (!sname.empty() && sname != "None") {
                    connect_source(sname, sip);
                }
            }
        }

        // Copy recv thread heartbeat
        if (recv_)
            last_recv_alive_ms_ = recv_->recv_heartbeat_ms();

        // FPS calculation (~1s)
        auto now = std::chrono::steady_clock::now();
        auto ms  = std::chrono::duration_cast<std::chrono::milliseconds>(
                       now - fps_last_time_).count();
        if (ms >= 1000) {
            if (recv_) {
                int total = 0, dropped = 0;
                recv_->get_performance(total, dropped);
                // Recreated receivers reset counters. Never expose negative FPS.
                const int64_t delta=int64_t(total)-fps_last_total_;
                double fps = (ms > 0 && delta>=0) ? delta * 1000.0 / ms : 0.0;
                // FrameSync duplicates are display frames, not fresh source input.
                if (delta>0) last_video_frame_ms_=now_ms();
                fps_ = fps;
                fps_last_total_ = total;
            }
            // Decoder throughput (frames actually decoded per second).
            int dc = decoded_count_.load();
            decode_fps_ = (ms > 0) ? (dc - decode_last_count_) * 1000.0 / ms : 0.0;
            decode_last_count_ = dc;

            // Best-effort GPU/decoder saturation signal: only meaningful for an
            // HX (compressed) stream that's connected and receiving. If the
            // decoder can't keep pace with the source for a few seconds, the
            // hardware decode engine (or CPU) is saturated. Uncompressed
            // (FrameSync) streams have no decoder, so this never trips for them.
            bool behind = connected_.load() && hx_stream_.load() &&
                          fps_.load() > 5.0 &&
                          decode_fps_.load() < fps_.load() * 0.85;
            if (behind) {
                if (decode_behind_secs_ < 1000) decode_behind_secs_++;
            } else {
                decode_behind_secs_ = 0;
            }
            decode_saturated_ = (decode_behind_secs_ >= 3);

            fps_last_time_ = now;
        }

        // Health check — detect and recover from frozen subsystems
        health_check();

        if (telemetry_ && recv_) {
            int total = 0, dropped = 0;
            recv_->get_performance(total, dropped);
            telemetry_->emit(total, dropped, recv_->video_queue_depth());
        }
    }

private:
    // health_check() only detects and records health state.
    // Corrective actions are taken by the external ndimon-watchdog service
    // which polls /api/health and decides what to do based on watchdog_mode.
    void health_check() {
        if (!connected_.load() || !recv_) {
            stall_count_ = 0;
            return;
        }

        auto t = now_ms();
        auto connected_duration = t - connect_time_ms_.load();

        // Give 10s grace period after connection before checking health
        if (connected_duration < 10000) return;

        bool healthy = true;

        // 1. Recv thread stall: heartbeat older than 5s
        auto recv_age = recv_stale_ms();
        if (recv_age > 5000) {
            healthy = false;
            if (stall_count_ == 10)
                std::cerr << "[Worker" << ch_num_ << "] HEALTH: recv thread stalled ("
                          << recv_age << "ms)\n";
        }

        // 2. No video frames arriving despite being connected
        auto video_age = video_stale_ms();
        if ((video_age < 0 && connected_duration > 10000) || video_age > 5000) {
            healthy = false;
            if (stall_count_ == 20)
                std::cerr << "[Worker" << ch_num_ << "] HEALTH: source video stopped\n";
        }

        // 3. Decoder hang: video arriving but no decoded output for 5s
        auto decoded_age = decoded_stale_ms();
        if (video_age >= 0 && video_age < 2000 && decoded_age > 5000) {
            healthy = false;
            if (stall_count_ == 10)
                std::cerr << "[Worker" << ch_num_ << "] HEALTH: decoder stall detected\n";
        }

        // 4. Display freeze: decoded OR uncompressed frames arriving but no commit
        auto display_age = display_stale_ms();
        bool frames_flowing = (decoded_age >= 0 && decoded_age < 2000) ||
                              (video_age >= 0 && video_age < 2000 && !hx_stream_.load());
        if (frames_flowing && display_age > 3000) {
            healthy = false;
            if (stall_count_ == 6) {
                std::cerr << "[Worker" << ch_num_ << "] HEALTH: display freeze detected\n";
                if (drm_) drm_->reset_flip_pending();
            }
            // HW-1 H6: say what the display looked like, once the freeze is
            // detected and again while it lasts, so a wedge leaves evidence.
            if (drm_ && (stall_count_ == 6 || stall_count_ == 20 || stall_count_ == 60)) {
                size_t queued;
                {
                    std::lock_guard<std::mutex> lk(frame_mtx_);
                    queued = frame_queue_.size();
                }
                std::cerr << "[Worker" << ch_num_ << "] HEALTH: display state (stall "
                          << stall_count_ << ", display_age=" << display_age
                          << "ms, frame_queue=" << queued << "): " << drm_->stall_report() << "\n";
            }
        }

        if (healthy) {
            stall_count_ = 0;
        } else if (stall_count_ < 1000) {
            stall_count_++;
        }
    }

    // Auto-resolution: match output mode to source resolution/framerate
    void try_match_source_mode(uint32_t src_w, uint32_t src_h,
                                int frame_rate_n = 0, int frame_rate_d = 0) {
        if (!drm_ || !drm_->is_initialized()) return;
        const auto key = SourceModeKey::make(src_w, src_h, frame_rate_n, frame_rate_d);
        resolution_policy_.match_source(key, [&] {

            auto modes = drm_->list_modes();
            if (modes.empty()) return false;

            // Compute source refresh rate
            double src_hz = 0.0;
            if (frame_rate_n > 0 && frame_rate_d > 0)
                src_hz = (double)frame_rate_n / (double)frame_rate_d;

            // Find best matching mode:
            // 1. Prefer exact resolution match
            // 2. Among exact matches, prefer closest refresh rate
            // 3. If no exact match, pick closest resolution (prefer >= source)
            const DRMMode* best = nullptr;
            double best_score = 1e9;

            for (const auto& m : modes) {
                double score = 0.0;
                // Resolution distance (prefer exact, then closest >=)
                int dw = (int)m.width  - (int)src_w;
                int dh = (int)m.height - (int)src_h;
                if (dw == 0 && dh == 0) {
                    score = 0.0;  // exact resolution match
                } else if (dw >= 0 && dh >= 0) {
                    score = 1000.0 + dw + dh;  // larger resolution, acceptable
                } else {
                    score = 2000.0 + std::abs(dw) + std::abs(dh);  // smaller, less preferred
                }
                // Refresh rate closeness (weighted less than resolution)
                if (src_hz > 0.0)
                    score += std::abs(m.refresh_hz - src_hz) * 10.0;

                if (score < best_score) {
                    best_score = score;
                    best = &m;
                }
            }

            if (best) {
                // Don't switch if we're already at this mode
                if (best->width == drm_->width() && best->height == drm_->height() &&
                    std::abs(best->refresh_hz - drm_->refresh_hz()) < 0.05 &&
                    drm_->mode_active()) {
                    return true;
                }
                std::cout << "[Worker" << ch_num_ << "] Auto-resolution: source "
                          << src_w << "x" << src_h;
                if (src_hz > 0) std::cout << "@" << src_hz;
                std::cout << " → output " << best->width << "x" << best->height
                          << "@" << best->refresh_hz << "\n";
                return drm_->set_mode(best->width, best->height, best->refresh_hz) == ResolutionResult::Applied;
            }
            return false;
        });
    }

    // Build the NDI receiver name: "DeviceAlias (OutputAlias)"
    std::string build_recv_name() const {
        auto out_cfg = Config::instance().get_output(ch_num_);
        return out_cfg.output_alias.empty()
            ? default_output_alias(connector_name_)
            : out_cfg.output_alias;
    }

    // -------------------------------------------------------------------------
    void on_video(NDIVideoFrame& vf) {
        bool is_compressed = (vf.stride == 0 && vf.size > 0);
        hx_stream_ = is_compressed;   // drives decode-saturation detection

        if (is_compressed) {
            VideoCodec codec = VideoCodec::H264;
            uint32_t fcc = (uint32_t)vf.fourcc;
            if (fcc == NDI_LIB_FOURCC('H','2','6','5') ||
                fcc == NDI_LIB_FOURCC('h','2','6','5') ||
                fcc == NDI_LIB_FOURCC('H','E','V','C')) {
                codec = VideoCodec::H265;
            }

            std::lock_guard<std::mutex> lk(decoder_mutex_);
            if (!decoder_) return;

            if (!decoder_initialized_ || codec != current_codec_) {
                decoder_->destroy();
                current_codec_      = codec;
                codec_name_         = (codec == VideoCodec::H265) ? "H265" : "H264";
                decoder_initialized_ = decoder_->init(codec);
                if (!decoder_initialized_ && decoder_->is_hardware()) {
                    std::cerr << "[Worker" << ch_num_ << "] "
                              << decoder_->backend_name()
                              << " init failed for " << codec_name_
                              << " — falling back to software\n";
                    decoder_->destroy();
                    decoder_.reset();
                    decoder_ = VideoDecoder::create_software();
                    if (decoder_) {
                        decoder_->set_frame_callback(
                            [this](DecodedFrame& f) { on_decoded(f); });
                        decoder_initialized_ = decoder_->init(codec);
                    }
                }
                if (decoder_initialized_) {
                    std::cout << "[Worker" << ch_num_ << "] Codec: " << codec_name_
                              << " " << vf.width << "x" << vf.height
                              << " backend=" << decoder_->backend_name() << "\n";
                } else {
                    std::cerr << "[Worker" << ch_num_ << "] HX decode init failed ("
                              << codec_name_ << ")\n";
                }
            }
            if (decoder_initialized_) {
                input_w_ = vf.width;
                input_h_ = vf.height;
                try_match_source_mode(vf.width, vf.height,
                                       vf.frame_rate_n, vf.frame_rate_d);
                // NDI timestamps are in 100 ns units → microseconds = value / 10.
                int64_t pts_us = vf.timestamp / 10;
                decoder_->decode(vf.data, vf.size, pts_us);
            }
        } else {
            // Uncompressed NDI (SpeedHQ/UYVY/NV12)
            if (!drm_ || !drm_->is_initialized() || !vf.data) return;

            uint32_t drm_fmt = 0;
            switch (ndi_display_format(vf.fourcc)) {
                case NDIDisplayFormat::BGRX: drm_fmt = DRM_FORMAT_XRGB8888; break;
                case NDIDisplayFormat::BGRA: drm_fmt = DRM_FORMAT_ARGB8888; break;
                case NDIDisplayFormat::UYVY: drm_fmt = DRM_FORMAT_UYVY; break;
                case NDIDisplayFormat::NV12: drm_fmt = DRM_FORMAT_NV12; break;
                default:
                    if (warned_video_formats_.insert(static_cast<uint32_t>(vf.fourcc)).second)
                        std::cerr << "[Worker" << ch_num_ << "] Unsupported NDI pixel format "
                                  << static_cast<uint32_t>(vf.fourcc) << "\n";
                    { std::lock_guard<std::mutex> lk(decoder_mutex_); codec_name_ = "unsupported"; }
                    return;
            }

            input_w_ = vf.width;
            input_h_ = vf.height;
            try_match_source_mode(vf.width, vf.height,
                                   vf.frame_rate_n, vf.frame_rate_d);
            {
                std::lock_guard<std::mutex> lk(decoder_mutex_);
                codec_name_ = "uncompressed";
            }

            // Queue application-owned pixels so SDK receiver recreation cannot
            // invalidate a frame still waiting for the display thread.
            if (engine_ && has_audio_) {
                std::lock_guard<std::mutex> lk(frame_mtx_);
                if (!frame_queue_.accept_frame_timestamp(vf.timestamp, vf.framesync)) return;
            }
            RawFrame rf;
            rf.copied_data.assign(vf.data, vf.data + vf.size);
            rf.width = vf.width; rf.height = vf.height;
            rf.stride = vf.stride; rf.drm_format = drm_fmt;
            last_decoded_frame_ms_ = now_ms(); // uncompressed skips HW decoder
            {
                std::lock_guard<std::mutex> lk(frame_mtx_);
                frame_queue_.set_frame_bytes_hint(rf.size(),
                    vf.frame_rate_n > 0 ? vf.frame_rate_n : 60,
                    vf.frame_rate_d > 0 ? vf.frame_rate_d : 1);
                rf.enqueued_us = DropTelemetry::mono_us();
                if (!frame_queue_.push(std::move(rf),rf.enqueued_us) && telemetry_)
                    telemetry_->on_queue_drop();
            }
            frame_cv_.notify_one();
        }
    }

    void on_audio(NDIAudioFrame& af) {
        if (engine_) {
            if (!recv_ || !recv_->audio_enabled() || !af.data || af.channels<1 || af.channels>16) return;
            ndimon::audio::Block block;block.channels=af.channels;block.frames=af.num_samples;
            for(int c=0;c<af.channels;++c)block.plane[c]=reinterpret_cast<float*>(reinterpret_cast<uint8_t*>(af.data)+size_t(c)*af.channel_stride);
            engine_->push_ndi(ch_num_, block, af.sample_rate);
            return;
        }
        if (!audio_ || !recv_ || !recv_->audio_enabled()) return;
        if (af.data) {
            // NDI audio is planar float: all channels in one contiguous buffer.
            // Channel c starts at base + c * channel_stride.
            std::vector<const float*> ch_ptrs(af.channels);
            for (int c = 0; c < af.channels; c++) {
                ch_ptrs[c] = (const float*)((const uint8_t*)af.data
                              + (size_t)c * (size_t)af.channel_stride);
            }
            audio_->write_audio(ch_ptrs.data(), af.channels, af.num_samples, af.channel_stride);
        }
    }

    void on_connection(bool connected, const std::string& src) {
        hx_stream_ = false;
        connected_ = connected;
        if (connected) {
            {
                std::lock_guard<std::mutex> lk(decoder_mutex_);
                codec_name_ = "none";
            }
            connect_time_ms_ = now_ms();
            // Reset health timestamps so stale values don't trigger false alarms
            last_video_frame_ms_   = 0;
            last_decoded_frame_ms_ = 0;
            last_display_commit_ms_ = 0;
            stall_count_ = 0;
            last_source_ = src;
            // Keep source_name_ in sync with the actual connected source.
            // When DS routes a new source, the SDK auto-switches and fires
            // on_connection with the real source — update source_name_ so
            // get_status() returns the correct value (not the stale config value).
            { std::lock_guard<std::mutex> lk(source_mutex_); source_name_ = src; }
            std::cout << "[Worker" << ch_num_ << "] Connected to: " << src << "\n";
            auto& cfg = Config::instance();
            bool tally = cfg.decoder.tally_mode == "TallyOn";
            recv_->set_tally(tally, false);
        } else {
            std::cout << "[Worker" << ch_num_ << "] Disconnected\n";
            if (drm_ && drm_->is_initialized()) {
                drm_->set_streaming(false);  // allow splash to render
                drm_->show_splash(false);
            }
        }

        // Push connection event to the control plane — it owns reconnect scheduling
        if (ipc_) {
            nlohmann::json ev;
            ev["type"]      = "connection";
            ev["output"]    = ch_num_ - 1;
            ev["connected"] = connected;
            ev["source"]    = src;
            ev["drm_ready"] = drm_ready();
            ipc_->push_event(ev);
        }

        // Update status file for ch1 (atomic write — ndimon-control may be reading)
        if (ch_num_ == 1) {
            try {
                nlohmann::json status;
                status["SourceName"] = connected ? src : "";
                status["Status"]     = connected ? "active" : "inactive";
                status["ChNum"]      = ch_num_;
                const char* path = "/run/ndimon/ndimon-dec1-status.json";
                const std::string tmp = std::string(path) + ".tmp";
                {
                    std::ofstream f(tmp);
                    if (f) f << status.dump();
                }
                std::rename(tmp.c_str(), path);
            } catch (...) {}
        }
    }

    void on_decoded(DecodedFrame& f) {
        last_decoded_frame_ms_ = now_ms();
        decoded_count_++;
        // Steal the frame so MPP/V4L2/FFmpeg drain does not release it when
        // this callback returns. Display thread renders then release_frame().
        // decoder_mutex_ before frame_mtx_: reload_config's decode-mode
        // branch uses the same order.
        DecodedFrame held = f;
        f.opaque = nullptr;
        f.fd = -1;
        f.data = nullptr;
        {
            std::lock_guard<std::mutex> dk(decoder_mutex_);
            std::lock_guard<std::mutex> fk(frame_mtx_);
            std::vector<DecodedFrame> dropped;
            while (hx_queue_.size() >= 2) {
                dropped.push_back(std::move(hx_queue_.front()));
                hx_queue_.pop();
            }
            hx_queue_.push(std::move(held));
            for (auto& old : dropped) {
                if (decoder_) decoder_->release_frame(old);
            }
        }
        frame_cv_.notify_one();
    }

    // -------------------------------------------------------------------------
    // Uncompressed frame queue — recv thread enqueues and returns immediately;
    // display_loop() drains the queue on a dedicated thread (avoids blocking
    // the NDI recv thread on vsync / software color conversion).
    // SDK storage is borrowed only during on_video; the queue owns its pixels.
    struct RawFrame {
        // Includes every source plane, including NV12 chroma.
        std::vector<uint8_t> copied_data;

        int width{}, height{}, stride{};
        uint32_t drm_format{};
        int64_t enqueued_us{};  // diagnostics: DropTelemetry::mono_us() at enqueue

        const uint8_t* data() const {
            return copied_data.data();
        }
        size_t size() const {
            return copied_data.size();
        }
    };
    std::set<uint32_t> warned_video_formats_;  // Known unsupported layouts; recv thread only.
    std::mutex              frame_mtx_;
    std::condition_variable frame_cv_;
    DelayedFrameQueue<RawFrame> frame_queue_{3840ull * 2160 * 4};
    std::queue<DecodedFrame> hx_queue_;     // HX DMA/CPU frames for display thread
    std::atomic<bool>       display_running_{false};
    std::thread             display_thr_;

    void show_hx_frame(DecodedFrame& f) {
        if (!drm_ || !drm_->is_initialized()) return;
        if (f.fd >= 0) {
            if (f.prime_explicit) {
                drm_->show_frame_dma_explicit(f.fd, f.drm_format,
                                              f.width, f.height,
                                              f.plane_offset, f.plane_pitch,
                                              f.num_planes, f.drm_modifier);
            } else {
                drm_->show_frame_dma(f.fd, f.drm_format,
                                     f.width, f.height,
                                     f.hor_stride, f.ver_stride);
            }
        } else if (f.data) {
            drm_->show_frame_memory(f.data, f.data_size,
                                    f.width, f.height,
                                    f.hor_stride, f.drm_format);
        }
        last_display_commit_ms_ = now_ms();
    }

    void display_loop() {
        while (display_running_) {
            RawFrame rf;
            DecodedFrame hx;
            bool have_raw = false, have_hx = false;
            {
                std::unique_lock<std::mutex> lk(frame_mtx_);
                auto wait_us = frame_queue_.empty() ? 50000LL :
                    std::max<int64_t>(0,frame_queue_.next_due_us()-DropTelemetry::mono_us());
                frame_cv_.wait_for(lk, std::chrono::microseconds(wait_us), [this] {
                    return (!frame_queue_.empty() && frame_queue_.next_due_us() <= DropTelemetry::mono_us()) || !hx_queue_.empty() || !display_running_;
                });
                if (!display_running_) break;
                if (!hx_queue_.empty()) {
                    hx = std::move(hx_queue_.front());
                    hx_queue_.pop();
                    have_hx = true;
                } else if (!frame_queue_.empty()) {
                    if (auto frame = frame_queue_.pop_due(DropTelemetry::mono_us())) {
                        rf = std::move(*frame);
                        have_raw = true;
                    }
                }
            }
            if (have_hx) {
                show_hx_frame(hx);
                std::lock_guard<std::mutex> lk(decoder_mutex_);
                if (decoder_) decoder_->release_frame(hx);
            } else if (have_raw) {
                if (drm_ && drm_->is_initialized()) {
                    const int64_t t0 = telemetry_ ? DropTelemetry::mono_us() : 0;
                    drm_->show_frame_memory(rf.data(), rf.size(),
                                            rf.width, rf.height, rf.stride, rf.drm_format);
                    last_display_commit_ms_ = now_ms();
                    if (telemetry_)
                        telemetry_->on_render(t0 - rf.enqueued_us, DropTelemetry::mono_us() - t0);
                }
            }
        }
        drain_queued_frames();
    }

    void drain_queued_frames() {
        std::queue<DecodedFrame> hx;
        {
            std::lock_guard<std::mutex> dk(decoder_mutex_);
            {
                std::lock_guard<std::mutex> fk(frame_mtx_);
                frame_queue_.clear();
                hx.swap(hx_queue_);
            }
            while (!hx.empty()) {
                DecodedFrame f = std::move(hx.front());
                hx.pop();
                if (decoder_) decoder_->release_frame(f);
            }
        }
    }

    // -------------------------------------------------------------------------
    std::string connector_name_;
    std::string device_;
    std::string preferred_mode_;
    AutoResolutionPolicy resolution_policy_;
    ScaleMode   scale_mode_;
    bool        has_audio_;
    int         ch_num_;
    int         lease_fd_  = -1;
    std::string source_name_;
    std::string source_ip_;
    bool        remote_origin_ = false;   // source_name_ was set by the Discovery Server
    mutable std::mutex source_mutex_;

    std::unique_ptr<DRMDisplay>   drm_;
    std::unique_ptr<NDIReceiver>  recv_;
    std::unique_ptr<DropTelemetry> telemetry_;  // only when NDIMON_TELEMETRY_CSV is set
    std::unique_ptr<AlsaAudio>    audio_;
    ndimon::audio::AudioEngine* engine_ = nullptr;

    mutable std::mutex decoder_mutex_;
    std::unique_ptr<VideoDecoder> decoder_;
    bool        decoder_initialized_ = false;
    VideoCodec  current_codec_       = VideoCodec::H264;
    std::string codec_name_          = "none";
    std::string decode_mode_         = "auto";   // tracks device.decode_mode for reload

    std::atomic<bool>    connected_{false};
    std::atomic<bool>    splash_requested_{false};  // async splash render (picked up by tick)
    std::atomic<int>     input_w_{0}, input_h_{0};
    std::atomic<double>  fps_{0.0};

    // Decode throughput / saturation tracking
    std::atomic<int>     decoded_count_{0};      // total frames sent to display
    std::atomic<double>  decode_fps_{0.0};
    std::atomic<bool>    hx_stream_{false};      // current stream is compressed (HX)
    std::atomic<bool>    decode_saturated_{false};
    int                  decode_last_count_ = 0;
    int                  decode_behind_secs_ = 0;

    // Health tracking timestamps (monotonic ms)
    std::atomic<int64_t> last_video_frame_ms_{0};
    std::atomic<int64_t> last_decoded_frame_ms_{0};
    std::atomic<int64_t> last_display_commit_ms_{0};
    std::atomic<int64_t> last_recv_alive_ms_{0};
    std::atomic<int64_t> connect_time_ms_{0};
    int                  stall_count_{0};

    IPCServer*        ipc_ = nullptr;
    std::string       last_source_;          // last successfully connected source name

    bool ndi_started_ = false;

    int fps_last_total_ = 0;
    std::chrono::steady_clock::time_point fps_last_time_;

    static std::atomic<bool> audio_in_use_;
};

std::atomic<bool> DisplayWorker::audio_in_use_{false};

// ===========================================================================
// main
// ===========================================================================
int main(int argc, char* argv[]) {
    // Environment first, while the process is single-threaded: the SDK finds its
    // config through NDI_CONFIG_DIR and must never see setenv() after this point.
    pin_ndi_home();
    (void)argc; (void)argv;

    signal(SIGINT,  signal_handler);
    signal(SIGTERM, signal_handler);
    signal(SIGPIPE, SIG_IGN);  // prevent write() to closed subscriber fd from killing process

    // Unbuffered output for journald
    std::cout << std::unitbuf;
    std::cerr << std::unitbuf;

    // Suppress VT console cursor
    {
        FILE* tty = fopen("/dev/tty1", "w");
        if (tty) {
            fputs("\033[?25l\033[?17;0;0c", tty);
            fclose(tty);
        }
    }

    std::cout << "[NDIMon-R] NDI Decoder starting\n";
    std::cout << "[NDIMon-R] Platform: " << PlatformDetect::name() << "\n";

    auto& cfg = Config::instance();
    // Volatile cache only: preserve handed-over domains across the existing
    // transport/engine service restart. A reboot still needs control-plane replay.
    {
        std::ifstream cached(ndisdk::config_dir() + "/decoder-config.json");
        if (cached) {
            try {
                const auto restored = cfg.apply_config(nlohmann::json::parse(cached));
                if (!restored.value("ok", false))
                    std::cerr << "[Config] Cannot restore follower snapshot: " << restored.dump() << "\n";
            } catch (const std::exception& e) {
                std::cerr << "[Config] Cannot restore follower snapshot: " << e.what() << "\n";
            }
        }
    }
    cfg.load();
    cfg.set_runtime_ip(get_primary_ip());

    // Set default NDI alias if not configured: "NDIMON-XXXX"
    if (cfg.device.ndi_recv_name.empty() && !cfg.follows("device")) {
        cfg.device.ndi_recv_name = "NDIMON-" + get_primary_mac_suffix();
        std::cout << "[NDIMon-R] NDI alias (default): " << cfg.device.ndi_recv_name << "\n";
        cfg.save_device();
        // Set OS hostname so NDI discovery shows the right device name.
        // Use fork/exec to avoid command injection via device name.
        {
            pid_t pid = fork();
            if (pid == 0) {
                execlp("hostnamectl", "hostnamectl", "set-hostname",
                       cfg.device.ndi_recv_name.c_str(), nullptr);
                _exit(127);
            } else if (pid > 0) {
                int status = 0;
                waitpid(pid, &status, 0);
            }
        }
        std::cout << "[NDIMon-R] Set hostname to: " << cfg.device.ndi_recv_name << "\n";
    }

    // The selected interface may still be waiting for its DHCP lease at boot.
    // Wait for it (the unit's start timeout is extended meanwhile) instead of
    // failing and being restarted; only refuse when it never comes (NI-18, W3).
    const auto selected_ndi_network = ndisdk::wait_for_selection(
        [] { return ndisdk::network_selection(); },
        std::chrono::seconds(60), std::chrono::milliseconds(250),
        [](std::chrono::milliseconds waited) {
            if (!g_running) return false;
            if (waited.count() == 0)
                std::cout << "[NDIMon-R] Waiting for the selected NDI network to get an address\n";
#ifdef HAVE_SYSTEMD
            if (waited.count() % 5000 == 0)
                sd_notify(0, "EXTEND_TIMEOUT_USEC=15000000\nSTATUS=waiting for the selected NDI network");
#endif
            return true;
        },
        [](std::chrono::milliseconds interval) { std::this_thread::sleep_for(interval); });
    if (!selected_ndi_network.ready()) {
        std::cerr << "[NDIMon-R] Selected NDI network unavailable; refusing automatic fallback\n";
        return 1;
    }
    // Write ~/.ndi/ndi-config.v1.json BEFORE NDIlib_initialize() so the SDK
    // picks up the discovery server address and groups at startup (NDI 6.2+).
    write_ndi_sdk_config(cfg);

    // Init NDI
    if (!NDIReceiver::ndi_init()) {
        std::cerr << "[NDIMon-R] NDI init failed\n";
        return 1;
    }

    // The NDI SDK rewrites ndi-config.v1.json during NDIlib_initialize(), wiping any
    // settings written before init. Re-write discovery and transport preferences
    // before creating receivers.
    write_ndi_sdk_config(cfg);
    std::cout << "[NDIMon-R] Standard NDI SDK receive (SDK-decoded frames)\n";

    // Auto-detect the DRM card that has HDMI/DP connectors.
    // On RPi4, HDMI outputs are on card1 (vc4), not card0 (V3D).
    // On Rockchip boards they are typically on card0 (VOP2).
    std::string drm_device = "/dev/dri/card0";
    for (int ci = 0; ci < 4; ci++) {
        std::string dev = "/dev/dri/card" + std::to_string(ci);
        auto probe = DRMDisplay::enumerate_connectors(dev);
        bool has_output = false;
        for (const auto& c : probe)
            if (c.name.find("HDMI") != std::string::npos ||
                c.name.find("DP")   != std::string::npos)
                has_output = true;
        if (has_output) { drm_device = dev; break; }
    }
    std::cout << "[NDIMon-R] DRM device: " << drm_device << "\n";

    // Enumerate DRM connectors
    auto connectors = DRMDisplay::enumerate_connectors(drm_device);
    std::cout << "[NDIMon-R] Found " << connectors.size() << " DRM connectors\n";
    for (const auto& c : connectors) {
        std::cout << "[NDIMon-R]   " << c.name
                  << (c.connected ? " (connected)" : " (disconnected)")
                  << " " << c.modes.size() << " modes\n";
    }

    // Build a stable connector→ch_num map. Backward-compatible with the original
    // fixed assignment (HDMI-A-1→1, HDMI-A-2→2, DP-1→3) so existing ARM configs
    // keep their dec{N}-settings.json. Any other connector a NUC might expose
    // (DP-2, eDP-1, HDMI-A-3, …) gets the lowest free ch, deterministically by
    // name, so the same hardware always yields the same mapping across reboots.
    auto is_output_conn = [](const std::string& n) {
        return n.find("HDMI") != std::string::npos || n.find("DP") != std::string::npos;
    };
    std::map<std::string, int> conn_ch;
    {
        std::set<int> used;
        auto fixed_ch = [](const std::string& n) -> int {
            if (n.find("HDMI-A-1") != std::string::npos) return 1;
            if (n.find("HDMI-A-2") != std::string::npos) return 2;
            if (n.find("DP-1") != std::string::npos && n.find("eDP") == std::string::npos) return 3;
            return 0;
        };
        // Pass 1: fixed/known connectors.
        for (const auto& c : connectors) {
            if (!is_output_conn(c.name)) continue;
            int ch = fixed_ch(c.name);
            if (ch > 0) { conn_ch[c.name] = ch; used.insert(ch); }
        }
        // Pass 2: unknown connectors, sorted by name → lowest free ch.
        std::vector<std::string> unknown;
        for (const auto& c : connectors)
            if (is_output_conn(c.name) && !conn_ch.count(c.name)) unknown.push_back(c.name);
        std::sort(unknown.begin(), unknown.end());
        for (const auto& n : unknown) {
            int ch = 1;
            while (used.count(ch)) ch++;
            conn_ch[n] = ch; used.insert(ch);
        }
    }

    // Create workers
    // Workers borrow the engine; reverse destruction order also protects exception unwind.
    std::unique_ptr<ndimon::audio::AudioEngine> audio_engine;
    std::vector<std::unique_ptr<DisplayWorker>> workers;

    // Open a DRM master fd and create per-connector leases so each worker
    // gets independent DRM master rights for its CRTC/connector/planes.
    int drm_master_fd = open(drm_device.c_str(), O_RDWR | O_CLOEXEC);
    if (drm_master_fd >= 0 && drmSetMaster(drm_master_fd) != 0) {
        std::cerr << "[NDIMon-R] drmSetMaster failed — lease creation may not work\n";
    }
    std::map<std::string, int> lease_fds;
    if (drm_master_fd >= 0) {
        std::set<uint32_t> used_crtcs;
        for (const auto& conn : connectors) {
            if (conn.name.find("HDMI") == std::string::npos &&
                conn.name.find("DP")   == std::string::npos) continue;
            uint32_t selected_crtc = 0;
            int lfd = DRMDisplay::create_lease(drm_master_fd, conn.name, used_crtcs, &selected_crtc);
            if (lfd >= 0) {
                lease_fds[conn.name] = lfd;
                if (selected_crtc) used_crtcs.insert(selected_crtc);
            }
        }
    }

    DisplayWorker* audio_worker = nullptr;
    std::map<int, DisplayWorker *> audio_workers;
    int primary_audio_output = 1;
    if (cfg.device.audio_engine == "v2") {
        using namespace ndimon::audio;
        audio_engine = std::make_unique<AudioEngine>(AudioEngine::Factories{
            [&](const SourceSettings &s, int period) {
                return std::make_unique<AlsaSource>(s.id, s.alsa_pcm, s.channels, period);
            },
            [&](const SinkSettings &s, bool master, int period) -> std::unique_ptr<AudioSink> {
                auto pcm=s.alsa_pcm;
                if (pcm == "auto") {
                    int output = primary_audio_output;
                    if (s.resource.size() == 6 && s.resource.rfind("hdmi:", 0) == 0 &&
                        s.resource[5] >= '1' && s.resource[5] <= '8')
                        output = s.resource[5] - '0';
                    auto target = audio_workers.find(output);
                    if (target == audio_workers.end())
                        return std::make_unique<UnavailableSink>(s.id, s.channels, master,
                                                                 "HDMI audio output unavailable: " +
                                                                     std::to_string(output));
                    pcm = s.resource.empty()
                              ? AlsaAudio::find_hdmi_device(target->second->connector_name())
                              : connector_pcm(target->second->connector_name());
                    if (pcm.empty())
                        return std::make_unique<UnavailableSink>(
                            s.id, s.channels, master,
                            "audio PCM mapping unavailable for connector " +
                                target->second->connector_name());
                }
                return std::make_unique<AlsaSink>(s.id,pcm,s.channels,period,master);
            },
            [&](const SourceSettings &s, int) -> std::unique_ptr<AudioSource> {
                int output = s.output ? s.output : primary_audio_output;
                auto target = audio_workers.find(output);
                if (target == audio_workers.end())
                    return std::make_unique<UnavailableSource>(s.id, s.channels,
                                                               "NDI audio output unavailable: " +
                                                                   std::to_string(output));
                auto *worker = target->second;
                return std::make_unique<NdiAudioSource>(
                    s.id, s.channels,
                    [worker](int frames, int channels, float *const *planes,
                             int *captured_channels) {
                        return worker->pull_audio(frames, channels, planes, captured_channels);
                    });
            }});
    }
    for (const auto& conn : connectors) {
        // Only HDMI-A-x, DP-x, eDP-x
        if (!is_output_conn(conn.name)) continue;

        // Stable connector→ch_num from the map built above
        int ch = conn_ch.count(conn.name) ? conn_ch[conn.name] : 1;

        auto out_cfg = cfg.get_output(ch);

        ScaleMode sm = ScaleMode::Letterbox;
        if (out_cfg.scale_mode == "stretch") sm = ScaleMode::Stretch;
        else if (out_cfg.scale_mode == "crop") sm = ScaleMode::Crop;

        bool primary_audio = workers.empty();
        int lfd = -1;
        auto it = lease_fds.find(conn.name);
        if (it != lease_fds.end()) lfd = it->second;

        auto w = std::make_unique<DisplayWorker>(
            conn.name, drm_device, out_cfg.preferred_mode,
            sm, primary_audio, ch, lfd);
        if (!audio_worker) {
            audio_worker = w.get();
            primary_audio_output = ch;
        }
        audio_workers.emplace(ch, w.get());
        w->set_audio_engine(audio_engine.get());
        w->start();
        workers.push_back(std::move(w));
    }

    // If no HDMI/DP connectors found, create one for HDMI-A-1 (hotplug)
    if (workers.empty()) {
        std::cout << "[NDIMon-R] No HDMI/DP connectors found, creating fallback worker\n";
        auto out_cfg = cfg.get_output(1);
        auto w = std::make_unique<DisplayWorker>(
            "HDMI-A-1", drm_device, out_cfg.preferred_mode,
            ScaleMode::Letterbox, true, 1);
        if (!audio_worker) {
            audio_worker = w.get();
            primary_audio_output = 1;
        }
        audio_workers.emplace(1, w.get());
        w->set_audio_engine(audio_engine.get());
        w->start();
        workers.push_back(std::move(w));
    }

    if (audio_engine)
        audio_engine->set_primary_output(primary_audio_output);
    auto reload_audio = [&]() -> nlohmann::json {
        int cap=5000;for(auto& w:workers)cap=std::min(cap,w->video_delay_cap_ms());
        if(!audio_engine)return {{"ok",true},{"applied",nlohmann::json::array()},{"restarted",false},{"restart_required",true}};
        auto result = cfg.follows("audio") ? audio_engine->apply(cfg.audio_settings(nullptr), cap)
            : audio_engine->reload("/etc/ndimon-audio-settings.json", cap);
        if (result.ok)
            audio_engine->start();
        for (auto &worker : workers)
            worker->update_audio_capture();
        auto effective=audio_engine->status();
        if(audio_worker && effective.contains("settings"))audio_worker->set_video_delay(effective["settings"].value("video_delay_ms",0));
        return result.to_json();
    };
    reload_audio();
    ndimon::audio::AudioMasterLog audio_master_log;
    if (audio_engine) {
        audio_engine->start();
        audio_master_log.update(audio_engine->status(), std::cout);
    }

    std::cout << "[NDIMon-R] " << workers.size() << " display worker(s) running\n";

    // IPC server
    auto ipc = std::make_unique<IPCServer>();

    // Helper: find worker by output_index (== ch_num - 1).
    // Look up by ch_num rather than vector position: on boards that expose a
    // non-standard connector set (e.g. only HDMI-A-2, or DP before HDMI) the
    // vector index does NOT equal ch_num-1, which would misroute connect/status
    // commands and silently break auto-reconnect for that output.
    auto get_worker = [&](int idx) -> DisplayWorker* {
        if (idx < 0 || idx > 7) {
            std::cerr << "[NDIMon-R] IPC: output index " << idx << " out of range\n";
            return nullptr;
        }
        for (auto& w : workers)
            if (w->ch_num() - 1 == idx) return w.get();
        std::cerr << "[NDIMon-R] IPC: no worker for output index " << idx
                  << " (have " << workers.size() << " workers)\n";
        return nullptr;
    };

    ipc->set_command_callback([&](const IPCCommand& cmd) {
        // The control plane re-asserts the settings with reload_config every 30 s;
        // that one is logged below, and only when the reload changed something.
        if (cmd.action != "reload_config")
            std::cout << "[NDIMon-R] IPC cmd: " << cmd.action
                      << " output=" << cmd.output_index << "\n";

        if (cmd.action == "connect") {
            auto* w = get_worker(cmd.output_index);
            if (w) w->connect_source(cmd.source_name, cmd.source_ip);

        } else if (cmd.action == "disconnect") {
            auto* w = get_worker(cmd.output_index);
            if (w) w->disconnect_source();

        } else if (cmd.action == "forget_source") {
            auto* w = get_worker(cmd.output_index);
            if (w) w->forget_source();

        } else if (cmd.action == "set_tally") {
            for (auto& w : workers)
                w->set_tally(cmd.tally_program, cmd.tally_preview);

        } else if (cmd.action == "reload_config") {
            const std::string before = cfg.follower_snapshot().dump();
            cfg.load();
            if (cfg.follower_snapshot().dump() != before)
                std::cout << "[NDIMon-R] IPC cmd: reload_config output=" << cmd.output_index
                          << " (configuration changed)\n";
            for (auto& w : workers) w->reload_config();
            reload_audio();

        } else if (cmd.action == "set_scale_mode") {
            ScaleMode sm = ScaleMode::Letterbox;
            if (cmd.scale_mode_str == "stretch") sm = ScaleMode::Stretch;
            else if (cmd.scale_mode_str == "crop") sm = ScaleMode::Crop;
            auto* w = get_worker(cmd.output_index);
            if (w) w->set_scale_mode(sm);

        } else if (cmd.action == "set_output_source") {
            auto* w = get_worker(cmd.output_index);
            if (w) w->connect_source(cmd.source_name, cmd.source_ip);

        } else if (cmd.action == "show_splash") {
            cfg.load();  // pick up new splash config written by API
            // Only show splash on outputs that aren't actively streaming
            for (auto& w : workers) {
                if (!w->is_streaming())
                    w->show_splash(cmd.source_available);
            }
        }
    });

    ipc->set_query_callback([&](const IPCCommand& cmd) -> nlohmann::json {
        if (cmd.action == "apply_config") {
            const auto& domains = cmd.config_request.at("config");
            const bool engine_restart = domains.contains("audio") &&
                domains.at("audio").at("engine") != (audio_engine ? "v2" : "v1");
            std::map<int, OutputConfig> previous;
            for (auto& w : workers) previous.emplace(w->ch_num(), cfg.get_output(w->ch_num()));
            auto response = cfg.apply_config(cmd.config_request, [&](const ndimon::audio::AudioSettings& settings) {
                if (!audio_engine || engine_restart) return std::string{};
                int cap = 5000;
                for (auto& w : workers) cap = std::min(cap, w->video_delay_cap_ms());
                const auto result = audio_engine->apply(settings, cap);
                return result.ok ? std::string{} : result.error;
            });
            if (!response.value("ok", false)) return response;
            if (!ndisdk::write_file_atomic(ndisdk::config_dir() + "/decoder-config.json", cfg.follower_snapshot().dump()))
                std::cerr << "[Config] WARNING: cannot cache follower snapshot for service restart\n";
            else chmod((ndisdk::config_dir() + "/decoder-config.json").c_str(), 0600);
            if (engine_restart) response["restart_required"] = true;
            write_ndi_sdk_config(cfg);
            for (auto& w : workers) {
                w->reload_config();
                if (domains.contains("outputs")) {
                    const auto out = cfg.get_output(w->ch_num());
                    const auto& old = previous.at(w->ch_num());
                    if (out.preferred_mode != old.preferred_mode || out.refresh_hz != old.refresh_hz) {
                        if (out.preferred_mode.empty()) w->set_auto_resolution(0);
                        else {
                            unsigned width = 0, height = 0;
                            if (sscanf(out.preferred_mode.c_str(), "%ux%u", &width, &height) == 2)
                                w->set_resolution(width, height, out.refresh_hz, 0);
                        }
                    }
                    if (out.source_name != old.source_name || out.source_ip != old.source_ip) {
                        if (out.source_name.empty()) w->forget_source();
                        else if (!w->on_remote_source(out.source_name, out.source_ip))
                            w->connect_source(out.source_name, out.source_ip);
                    }
                }
                if (!w->is_streaming()) w->show_splash(false);
            }
            if (audio_engine) audio_engine->start();
            return response;
        }
        if (cmd.action == "get_modes") {
            auto* w = get_worker(cmd.output_index);
            return w ? w->get_modes() : nlohmann::json{{"error", "no worker"}};
        }
        if (cmd.action == "set_rotation") {
            auto* w=get_worker(cmd.output_index);
            if (!w) return nlohmann::json{{"ok",false},{"error","no worker"}};
            bool ok=w->set_rotation(cmd.rotation_degrees);
            auto result=w->get_modes();
            result["ok"]=ok;
            if (!ok) result["error"]=w->rotation_busy()?"rotation_busy":"rotation_unsupported";
            return result;
        }
        if (cmd.action == "set_resolution" || cmd.action == "auto_resolution") {
            auto* w = get_worker(cmd.output_index);
            if (!w) return nlohmann::json{{"ok", false}, {"error", "no worker"}};
            // Prefer fractional refresh_hz; fall back to integer refresh
            double hz = cmd.res_refresh_hz > 0.0
                        ? cmd.res_refresh_hz
                        : (double)cmd.res_refresh;
            auto result = cmd.action == "auto_resolution" ? w->set_auto_resolution(cmd.deadline_monotonic_ms)
                : w->set_resolution(cmd.res_width, cmd.res_height, hz, cmd.deadline_monotonic_ms);
            nlohmann::json j = w->get_modes();
            j["ok"] = result == ResolutionResult::Applied;
            if (result != ResolutionResult::Applied)
                j["error"] = result == ResolutionResult::Busy ? "resolution_busy"
                    : result == ResolutionResult::Expired ? "expired" : "resolution_failed";
            return j;
        }
        if (cmd.action == "reload_audio") return reload_audio();
        if (cmd.action == "check_audio") {
            int cap=5000;for(auto& w:workers)cap=std::min(cap,w->video_delay_cap_ms());
            return nlohmann::json{{"ok",true},{"video_delay_cap_ms",cap}};
        }
        if (cmd.action == "get_status_all") {
            nlohmann::json j = nlohmann::json::array();
            for (auto& w : workers) j.push_back(w->get_status());
            return j;
        }
        if (cmd.action == "health") {
            static auto start_time = now_ms();
            nlohmann::json j;
            j["alive"] = true;
            j["revision"] = cfg.applied_revision();
            j["uptime_s"] = (now_ms() - start_time) / 1000;
            j["outputs"] = nlohmann::json::array();
            for (auto& w : workers) {
                nlohmann::json h;
                h["output"]        = w->ch_num() - 1;
                h["connected"]     = w->is_connected();
                h["fps"]           = w->get_fps();
                h["health"]        = w->health_state();
                h["recv_stale_ms"]    = w->recv_stale_ms();
                h["video_stale_ms"]   = w->video_stale_ms();
                h["decoded_stale_ms"] = w->decoded_stale_ms();
                h["display_stale_ms"] = w->display_stale_ms();
                h["stall_count"]      = w->stall_count();
                j["outputs"].push_back(h);
            }
            return j;
        }
        return nlohmann::json{{"ok", false}, {"error", "unknown query"}};
    });

    ipc->set_status_provider([&]() -> nlohmann::json {
        // Return first worker's status + array of all
        nlohmann::json s;
        s["revision"] = cfg.applied_revision();
        if (!workers.empty()) {
            s = workers[0]->get_status();
            s["revision"] = cfg.applied_revision();
            s["platform"] = PlatformDetect::name();
        }
        nlohmann::json all = nlohmann::json::array();
        for (auto& w : workers) all.push_back(w->get_status());
        s["outputs"] = all;
        s["audio"] = audio_engine ? audio_engine->status() : nlohmann::json{{"running",false},{"engine","v1"}};
        if(audio_engine){
            int cap=5000,effective=5000;
            for(auto& w:workers){cap=std::min(cap,w->video_delay_cap_ms());effective=std::min(effective,w->effective_video_delay_ms());}
            s["audio"]["video_delay_cap_ms"]=cap;
            s["audio"]["effective_video_delay_ms"]=effective;
            s["audio"]["video_delay_capped"]=s["audio"].value("video_delay_ms",0)>effective;
        }
        return s;
    });

    ipc->start("/tmp/ndi-decoder.sock");

#ifdef HAVE_SYSTEMD
    sd_notify(0, "READY=1");
    std::cout << "[NDIMon-R] sd_notify READY\n";
#else
    std::cerr << "[NDIMon-R] WARNING: built without libsystemd — sd_notify disabled.\n"
              << "[NDIMon-R] If using Type=notify in systemd, the service WILL timeout.\n"
              << "[NDIMon-R] Install libsystemd-dev and rebuild to fix.\n";
#endif

    // Wire IPC pointer into workers so they can push events
    for (auto& w : workers) w->set_ipc(ipc.get());

    // When a subscriber (ndimon-control) connects, replay a Discovery Server
    // route it may have missed (handles startup race and control restarts)
    ipc->set_subscriber_connected_callback([&]() {
        for (auto& w : workers) {
            w->replay_remote_source();
            w->push_connection_state();   // triggers the supervisor's retry loop if needed
        }
    });

    // Main loop: tick workers (hotplug, fps)
    // Config reload is now driven by explicit reload_config IPC commands from ndimon-control
    std::cout << "[NDIMon-R] Running. Ctrl+C to exit.\n";

    std::string shown_ip = Config::instance().runtime_ip();
    std::string shown_banner;
    int hint_tick = 0;
    while (g_running) {
        if (ndisdk::network_selection().signature() != selected_ndi_network.signature()) {
            std::cerr << "[NDIMon-R] NDI interface/address changed; restarting SDK\n";
            g_running = false;
            break;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(500));
        // The splash reads the address once per draw. Refresh it so a DHCP
        // or restored address shows up without restarting the decoder.
        if (++hint_tick >= 4) {
            hint_tick = 0;
            std::string ip = get_primary_ip();
            std::string banner;
            {
                std::ifstream banner_file("/run/ndimon/net-pending");
                if (banner_file) std::getline(banner_file, banner);
            }
            if (ip != shown_ip || banner != shown_banner) {
                shown_ip = ip;
                shown_banner = banner;
                Config::instance().set_runtime_ip(ip);
                for (auto& w : workers) w->note_network_hint();
            }
        }
        for (auto& w : workers) w->tick();
        if (audio_engine) audio_master_log.update(audio_engine->status(), std::cout);
#ifdef HAVE_SYSTEMD
        sd_notify(0, "WATCHDOG=1");
#endif
    }

    // Cleanup
#ifdef HAVE_SYSTEMD
    sd_notify(0, "STOPPING=1");
#endif
    std::cout << "[NDIMon-R] Shutting down...\n";
    ipc->stop();
    if (audio_engine) audio_engine->stop();
    for (auto& w : workers) w->stop();
    workers.clear();
    if (drm_master_fd >= 0) close(drm_master_fd);
    NDIReceiver::ndi_destroy();
    std::cout << "[NDIMon-R] Done.\n";
    return 0;
}
