#include "Config.h"
#include "VideoDecoder.h"
#include "NDIReceiver.h"
#include "DRMDisplay.h"
#include "AlsaAudio.h"
#include "IPCServer.h"
#include "PlatformDetect.h"

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
#include <fcntl.h>
#include <sys/stat.h>
#include <ifaddrs.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <drm_fourcc.h>
#include <xf86drm.h>
#include <Processing.NDI.Lib.h>

static std::atomic<bool> g_running{true};

// Write ~/.ndi/ndi-config.v1.json so the NDI SDK uses the discovery server
// and correct groups for BOTH finding sources AND registering this receiver.
// Must be called before NDIlib_initialize().
static void write_ndi_sdk_config(const Config& cfg) {
    const char* home = getenv("HOME");
    if (!home || !home[0]) home = "/root";
    std::string ndi_dir = std::string(home) + "/.ndi";
    mkdir(ndi_dir.c_str(), 0755);
    std::string path = ndi_dir + "/ndi-config.v1.json";

    std::string groups = cfg.ndi_group.groups.empty() ? "public" : cfg.ndi_group.groups;
    std::string discovery;
    if (cfg.finder.discovery_server_mode == "NDIDisServEn" &&
        !cfg.finder.discovery_server_ip.empty()) {
        discovery = cfg.finder.discovery_server_ip;
    }

    // Build JSON manually to avoid pulling nlohmann into this helper.
    // codec.h264/h265.passthrough=true forces the NDI SDK to deliver HX streams
    // as compressed H.264/H.265 bitstream rather than decoding them internally.
    // This is critical on Noble where libavcodec.so.61 (FFmpeg 7) is not available;
    // without passthrough the SDK renders a "Video Decoder not Found" error frame.
    std::string json =
        "{\n"
        "  \"ndi\": {\n"
        "    \"groups\": {\n"
        "      \"recv\": \"" + groups + "\",\n"
        "      \"send\": \"" + groups + "\"\n"
        "    },\n"
        "    \"networks\": {\n"
        "      \"ips\": \"" + cfg.off_subnet_ips + "\",\n"
        "      \"discovery\": \"" + discovery + "\"\n"
        "    },\n"
        "    \"codec\": {\n"
        "      \"h264\": { \"passthrough\": true },\n"
        "      \"h265\": { \"passthrough\": true }\n"
        "    }\n"
        "  }\n"
        "}\n";

    std::ofstream f(path);
    if (f) { f << json; std::cout << "[NDIMon-R] NDI config written: " << path << "\n"; }
    else   { std::cerr << "[NDIMon-R] WARNING: could not write " << path << "\n"; }
}

// Returns the first non-loopback IPv4 address, or empty string.
static std::string get_primary_ip() {
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
    // Try common interface names in order of preference
    static const char* ifaces[] = {
        "enP4p65s0", "eth0", "end0", "eno1", "enp0s3", nullptr
    };
    for (int i = 0; ifaces[i]; ++i) {
        std::string path = std::string("/sys/class/net/") + ifaces[i] + "/address";
        std::ifstream f(path);
        if (!f) continue;
        std::string mac;
        std::getline(f, mac);
        // Remove colons: "aa:bb:cc:dd:EE:FF" -> "aabbccddeeff"
        std::string raw;
        for (char c : mac) if (c != ':') raw += c;
        if (raw.size() >= 4) {
            // uppercase last 4 chars
            std::string suffix = raw.substr(raw.size() - 4);
            for (auto& c : suffix) c = (char)toupper((unsigned char)c);
            return suffix;
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
        , scale_mode_(scale)
        , has_audio_(has_audio)
        , ch_num_(ch_num)
        , lease_fd_(lease_fd)
    {}

    void set_ipc(IPCServer* ipc) { ipc_ = ipc; }

    // Replay the last buffered routing event to all subscribers (startup race safety)
    void replay_routing_event() {
        if (!ipc_) return;
        std::lock_guard<std::mutex> lk(routing_event_mutex_);
        if (!last_routing_event_.is_null()) {
            ipc_->push_event(last_routing_event_);
        }
    }

    // Push current connection state to newly connected subscriber so Node.js
    // can start a retry loop immediately if the saved source is unavailable
    void push_connection_state() {
        if (!ipc_) return;
        nlohmann::json ev;
        ev["type"]      = "connection";
        ev["output"]    = ch_num_ - 1;
        ev["connected"] = connected_.load();
        ev["source"]    = connected_.load() ? last_source_ : "";
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
            drm_->show_splash(false);
            std::cout << "[Worker" << ch_num_ << "] DRM ready: "
                      << drm_->width() << "x" << drm_->height()
                      << "@" << drm_->refresh() << "\n";
        }

        // Create video decoder
        {
            std::lock_guard<std::mutex> lk(decoder_mutex_);
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
        if (has_audio_ && !audio_in_use_.exchange(true)) {
            auto audio = std::make_unique<AlsaAudio>();
            if (audio->init("default", 48000, 2)) {
                audio_ = std::move(audio);
                std::cout << "[Worker" << ch_num_ << "] ALSA audio initialized\n";
            } else {
                std::cerr << "[Worker" << ch_num_ << "] ALSA init failed\n";
                audio_in_use_ = false;
            }
        }

        // Create NDI receiver
        recv_ = std::make_unique<NDIReceiver>();

        recv_->set_video_callback([this](NDIVideoFrame& vf) {
            on_video(vf);
        });
        recv_->set_audio_callback([this](NDIAudioFrame& af) {
            on_audio(af);
        });
        recv_->set_connection_callback([this](bool connected, const std::string& src) {
            on_connection(connected, src);
        });
        recv_->set_routing_callback([this](const std::string& name, const std::string& url) {
            // Discovery server assigned a new source — push event to Node.js,
            // which owns all routing decisions. Buffer for subscriber reconnect replay.
            if (!ipc_) return;
            nlohmann::json ev;
            ev["type"]   = "routing";
            ev["source"] = name;
            ev["url"]    = url;
            ev["output"] = ch_num_ - 1;
            {
                std::lock_guard<std::mutex> lk(routing_event_mutex_);
                last_routing_event_ = ev;
            }
            ipc_->push_event(ev);
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

        if (cfg.finder.discovery_server_mode == "NDIDisServEn" &&
            !cfg.finder.discovery_server_ip.empty()) {
            recv_->set_discovery_server(cfg.finder.discovery_server_ip);
        }

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
        // Stop display thread first so it doesn't race with drm_ teardown
        display_running_ = false;
        frame_cv_.notify_all();
        if (display_thr_.joinable()) display_thr_.join();
        { std::lock_guard<std::mutex> lk(frame_mtx_); while (!frame_queue_.empty()) frame_queue_.pop(); }

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
        {
            std::lock_guard<std::mutex> lk(source_mutex_);
            source_name_ = name;
            source_ip_   = ip;
        }

        // Build properties metadata to send to the source/DS on connection.
        // Informs the DS of the device identity, current routing assignment,
        // and audio/video/general configuration.
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

            // Routing ack — tells the DS which source this receiver is showing
            xml += "<ndi_routing>"
                   "<source name=\"" + name + "\" url=\"" + ip + "\"/>"
                   "</ndi_routing>";

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
    }

    void disconnect_source() {
        if (recv_) recv_->disconnect();
        if (drm_) drm_->set_streaming(false);  // allow splash to render
        if (drm_ && drm_->is_initialized()) drm_->show_splash(false);
        // Do NOT clear source from persistent config so the device can
        // auto-reconnect after a reboot or transient signal loss.
    }

    void forget_source() {
        if (recv_) recv_->disconnect();
        if (drm_) drm_->set_streaming(false);  // allow splash to render
        if (drm_ && drm_->is_initialized()) drm_->show_splash(false);
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
        if (recv_) recv_->set_audio_enabled(en);
    }

    void set_discovery_server(const std::string& ip) {
        if (recv_) recv_->set_discovery_server(ip);
    }

    void reload_config() {
        auto& cfg = Config::instance();
        auto out_cfg = cfg.get_output(ch_num_);

        // Update scale mode
        ScaleMode sm = ScaleMode::Letterbox;
        if (out_cfg.scale_mode == "stretch") sm = ScaleMode::Stretch;
        else if (out_cfg.scale_mode == "crop") sm = ScaleMode::Crop;
        if (sm != scale_mode_) {
            scale_mode_ = sm;
            if (drm_) drm_->set_scale_mode(sm);
        }

        // OSD
        if (drm_) {
            drm_->set_osd_enabled(cfg.osd.enabled);
            drm_->set_osd_text(cfg.osd.text);
        }

        // Discovery server
        if (recv_) {
            std::string ds;
            if (cfg.finder.discovery_server_mode == "NDIDisServEn" &&
                !cfg.finder.discovery_server_ip.empty())
                ds = cfg.finder.discovery_server_ip;
            recv_->set_discovery_server(ds);
        }

        // Audio enable/disable (ch1 legacy)
        if (ch_num_ == 1) {
            recv_->set_audio_enabled(cfg.decoder.ndi_audio == "NDIAudioEn");
        }

        // Update NDI recv name if device or output alias changed.
        // rename() destroys and recreates the recv instance with the new name,
        // re-registers it with the advertiser, and reconnects to the source.
        // Skip while actively streaming — renaming mid-stream causes a reconnect
        // churn if the config reload happens immediately after connect_source saves
        // the new source (the alias write triggers rename → reconnect loop).
        if (recv_ && !connected_.load()) {
            std::string new_name = build_recv_name();
            if (recv_->get_recv_name() != new_name)
                recv_->rename(new_name);
        }
    }

    void show_splash(bool source_available) {
        if (drm_ && drm_->is_initialized())
            drm_->show_splash(source_available);
    }

    bool is_streaming() const {
        return drm_ && drm_->is_streaming();
    }

    bool set_resolution(uint32_t w, uint32_t h, double refresh_hz) {
        if (!drm_ || !drm_->is_initialized()) return false;
        bool ok = drm_->set_mode(w, h, refresh_hz);
        if (ok) {
            auto& cfg = Config::instance();
            OutputConfig oc = cfg.get_output(ch_num_);
            oc.preferred_mode = std::to_string(w) + "x" + std::to_string(h);
            cfg.set_output(ch_num_, oc);
        }
        return ok;
    }

    nlohmann::json get_modes() const {
        nlohmann::json j;
        j["modes"] = nlohmann::json::array();
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
        s["connected"]    = connected_.load();
        {
            std::lock_guard<std::mutex> lk(source_mutex_);
            s["source_name"] = source_name_;
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
            s["codec"]    = codec_name_;
        }
        std::string sm = "letterbox";
        if (scale_mode_ == ScaleMode::Stretch) sm = "stretch";
        else if (scale_mode_ == ScaleMode::Crop) sm = "crop";
        s["scale_mode"] = sm;
        return s;
    }

    std::string conn_name() const { return connector_name_; }
    int         ch_num()    const { return ch_num_; }

    bool drm_ready() const {
        return drm_ && drm_->is_initialized();
    }

    void tick() {
        // Hotplug check
        if (drm_ && !drm_->is_initialized()) {
            if (drm_->check_hotplug()) {
                std::cout << "[Worker" << ch_num_ << "] Display connected via hotplug\n";
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

        // FPS calculation (~1s)
        auto now = std::chrono::steady_clock::now();
        auto ms  = std::chrono::duration_cast<std::chrono::milliseconds>(
                       now - fps_last_time_).count();
        if (ms >= 1000) {
            if (recv_) {
                int total = 0, dropped = 0;
                recv_->get_performance(total, dropped);
                double fps = (ms > 0) ? (total - fps_last_total_) * 1000.0 / ms : 0.0;
                fps_ = fps;
                fps_last_total_ = total;
            }
            fps_last_time_ = now;
        }
    }

private:
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
                if (decoder_initialized_) {
                    std::cout << "[Worker" << ch_num_ << "] Codec: " << codec_name_
                              << " " << vf.width << "x" << vf.height << "\n";
                }
            }
            if (decoder_initialized_) {
                input_w_ = vf.width;
                input_h_ = vf.height;
                int64_t pts_us = vf.timestamp / 100;
                decoder_->decode(vf.data, vf.size, pts_us);
            }
        } else {
            // Uncompressed NDI (SpeedHQ/UYVY/NV12)
            if (!drm_ || !drm_->is_initialized() || !vf.data) return;

            uint32_t drm_fmt = DRM_FORMAT_YUYV;
            uint32_t fcc = (uint32_t)vf.fourcc;
            if (fcc == NDI_LIB_FOURCC('U','Y','V','Y'))
                drm_fmt = DRM_FORMAT_UYVY;
            else if (fcc == NDI_LIB_FOURCC('N','V','1','2'))
                drm_fmt = DRM_FORMAT_NV12;

            input_w_ = vf.width;
            input_h_ = vf.height;
            {
                std::lock_guard<std::mutex> lk(decoder_mutex_);
                codec_name_ = "uncompressed";
            }

            // Copy frame to owned buffer and hand off to the display thread.
            // This returns immediately — the recv thread is never blocked on vsync
            // or software color conversion, keeping the NDI queue drained.
            RawFrame rf;
            rf.data.assign(vf.data, vf.data + vf.size);
            rf.width = vf.width; rf.height = vf.height;
            rf.stride = vf.stride; rf.drm_format = drm_fmt;
            {
                std::lock_guard<std::mutex> lk(frame_mtx_);
                // Drop oldest frame if display thread can't keep up
                while (frame_queue_.size() >= 2) frame_queue_.pop();
                frame_queue_.push(std::move(rf));
            }
            frame_cv_.notify_one();
        }
    }

    void on_audio(NDIAudioFrame& af) {
        auto& cfg = Config::instance();
        if (!audio_) return;
        if (ch_num_ == 1 && cfg.decoder.ndi_audio == "NDIAudioDis") return;
        if (af.channel_data && af.channel_data[0]) {
            // NDI audio is planar float: all channels in one buffer.
            // channel_data[0] is the base; channel c starts at base + c * channel_stride.
            // DO NOT index af.channel_data[c] for c>0 — it reads past the single p_data pointer.
            const float* base = af.channel_data[0];
            std::vector<const float*> ch_ptrs(af.channels);
            for (int c = 0; c < af.channels; c++) {
                ch_ptrs[c] = (const float*)((const uint8_t*)base
                              + (size_t)c * (size_t)af.channel_stride);
            }
            audio_->write_audio(ch_ptrs.data(), af.channels, af.num_samples, af.channel_stride);
        }
    }

    void on_connection(bool connected, const std::string& src) {
        connected_ = connected;
        if (connected) {
            last_source_ = src;
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

        // Push connection event to Node.js — it owns reconnect scheduling
        if (ipc_) {
            nlohmann::json ev;
            ev["type"]      = "connection";
            ev["output"]    = ch_num_ - 1;
            ev["connected"] = connected;
            ev["source"]    = src;
            ipc_->push_event(ev);
        }

        // Update status file for ch1 (BirdDog compatibility)
        if (ch_num_ == 1) {
            try {
                nlohmann::json status;
                status["SourceName"] = connected ? src : "";
                status["Status"]     = connected ? "active" : "inactive";
                status["ChNum"]      = ch_num_;
                std::ofstream f("/etc/ndimon-dec1-status.json");
                f << status.dump();
            } catch (...) {}
        }
    }

    void on_decoded(DecodedFrame& f) {
        // NOTE: called from decode() → drain_frames() → frame_cb_(),
        // which means decoder_mutex_ is already held by on_video().
        // Do NOT attempt to re-lock decoder_mutex_ here — it will deadlock.
        if (drm_ && drm_->is_initialized()) {
            // Prefer DMA-BUF fd path for MPP frames — kernel-managed memory,
            // no virtual address access needed, safe for RGA/DRM import.
            if (f.fd >= 0) {
                drm_->show_frame_dma(f.fd, f.drm_format,
                                     f.width, f.height,
                                     f.hor_stride, f.ver_stride);
            } else if (f.data) {
                drm_->show_frame_memory(f.data, f.data_size,
                                        f.width, f.height,
                                        f.hor_stride, f.drm_format);
            }
        }
        // release_frame clears f.opaque so MppDecoder::drain_frames()
        // skips its own release. decoder_ is guaranteed non-null here
        // because we're called from within decoder_->decode().
        decoder_->release_frame(f);
    }

    // -------------------------------------------------------------------------
    // Raw uncompressed frame queue — recv thread copies and returns immediately;
    // display_loop() drains the queue on a dedicated thread (avoids blocking
    // the NDI recv thread on vsync / software color conversion).
    struct RawFrame {
        std::vector<uint8_t> data;
        int width{}, height{}, stride{};
        uint32_t drm_format{};
    };
    std::mutex              frame_mtx_;
    std::condition_variable frame_cv_;
    std::queue<RawFrame>    frame_queue_;   // bounded to 2 frames (drop oldest)
    std::atomic<bool>       display_running_{false};
    std::thread             display_thr_;

    void display_loop() {
        while (display_running_) {
            RawFrame rf;
            {
                std::unique_lock<std::mutex> lk(frame_mtx_);
                frame_cv_.wait_for(lk, std::chrono::milliseconds(50), [this] {
                    return !frame_queue_.empty() || !display_running_;
                });
                if (!display_running_) break;
                if (frame_queue_.empty()) continue;
                rf = std::move(frame_queue_.front());
                frame_queue_.pop();
            }
            if (drm_ && drm_->is_initialized())
                drm_->show_frame_memory(rf.data.data(), rf.data.size(),
                                        rf.width, rf.height, rf.stride, rf.drm_format);
        }
    }

    // -------------------------------------------------------------------------
    std::string connector_name_;
    std::string device_;
    std::string preferred_mode_;
    ScaleMode   scale_mode_;
    bool        has_audio_;
    int         ch_num_;
    int         lease_fd_  = -1;
    std::string source_name_;
    std::string source_ip_;
    mutable std::mutex source_mutex_;

    std::unique_ptr<DRMDisplay>   drm_;
    std::unique_ptr<NDIReceiver>  recv_;
    std::unique_ptr<AlsaAudio>    audio_;

    mutable std::mutex decoder_mutex_;
    std::unique_ptr<VideoDecoder> decoder_;
    bool        decoder_initialized_ = false;
    VideoCodec  current_codec_       = VideoCodec::H264;
    std::string codec_name_          = "none";

    std::atomic<bool>    connected_{false};
    std::atomic<int>     input_w_{0}, input_h_{0};
    std::atomic<double>  fps_{0.0};

    IPCServer*        ipc_ = nullptr;
    nlohmann::json    last_routing_event_;   // buffered for subscriber reconnect
    mutable std::mutex routing_event_mutex_;
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
    cfg.load();
    cfg.device.device_ip = get_primary_ip();

    // Set default NDI alias if not configured: "NDIMON-XXXX"
    if (cfg.device.ndi_recv_name.empty()) {
        cfg.device.ndi_recv_name = "NDIMON-" + get_primary_mac_suffix();
        std::cout << "[NDIMon-R] NDI alias (default): " << cfg.device.ndi_recv_name << "\n";
        cfg.save();
        // Set OS hostname so NDI discovery shows the right device name
        std::string cmd = "hostnamectl set-hostname \"" + cfg.device.ndi_recv_name + "\" 2>/dev/null";
        (void)system(cmd.c_str());
        std::cout << "[NDIMon-R] Set hostname to: " << cfg.device.ndi_recv_name << "\n";
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
    // extra keys we wrote before init. Re-write now so the codec passthrough settings
    // are present when the recv instances are created (SDK reads them at recv creation).
    write_ndi_sdk_config(cfg);

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

    // Create workers
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

    for (const auto& conn : connectors) {
        // Only HDMI-A-x and DP-x
        if (conn.name.find("HDMI") == std::string::npos &&
            conn.name.find("DP")   == std::string::npos) continue;

        // Map connector to ch_num
        int ch = 1;
        if (conn.name.find("HDMI-A-2") != std::string::npos) ch = 2;
        else if (conn.name.find("DP-1") != std::string::npos) ch = 3;
        else if (conn.name.find("HDMI-A-1") != std::string::npos) ch = 1;

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
        w->start();
        workers.push_back(std::move(w));
    }

    std::cout << "[NDIMon-R] " << workers.size() << " display worker(s) running\n";

    // IPC server
    auto ipc = std::make_unique<IPCServer>();

    // Helper: find worker by output_index
    auto get_worker = [&](int idx) -> DisplayWorker* {
        if (idx < 0 || idx >= (int)workers.size()) return workers[0].get();
        return workers[idx].get();
    };

    ipc->set_command_callback([&](const IPCCommand& cmd) {
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
            cfg.load();
            for (auto& w : workers) w->reload_config();

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
        if (cmd.action == "get_modes") {
            auto* w = get_worker(cmd.output_index);
            return w ? w->get_modes() : nlohmann::json{{"error", "no worker"}};
        }
        if (cmd.action == "set_resolution") {
            auto* w = get_worker(cmd.output_index);
            if (!w) return nlohmann::json{{"ok", false}, {"error", "no worker"}};
            // Prefer fractional refresh_hz; fall back to integer refresh
            double hz = cmd.res_refresh_hz > 0.0
                        ? cmd.res_refresh_hz
                        : (double)cmd.res_refresh;
            bool ok = w->set_resolution(cmd.res_width, cmd.res_height, hz);
            nlohmann::json j = w->get_modes();
            j["ok"] = ok;
            return j;
        }
        if (cmd.action == "get_status_all") {
            nlohmann::json j = nlohmann::json::array();
            for (auto& w : workers) j.push_back(w->get_status());
            return j;
        }
        return nlohmann::json{{"ok", false}, {"error", "unknown query"}};
    });

    ipc->set_status_provider([&]() -> nlohmann::json {
        // Return first worker's status + array of all
        nlohmann::json s;
        if (!workers.empty()) {
            s = workers[0]->get_status();
            s["platform"] = PlatformDetect::name();
        }
        nlohmann::json all = nlohmann::json::array();
        for (auto& w : workers) all.push_back(w->get_status());
        s["outputs"] = all;
        return s;
    });

    ipc->start("/tmp/ndi-decoder.sock");

    // Wire IPC pointer into workers so they can push events
    for (auto& w : workers) w->set_ipc(ipc.get());

    // When a subscriber (Node.js) connects, replay buffered routing events
    // so it can recover current state (handles startup race and Node.js restarts)
    ipc->set_subscriber_connected_callback([&]() {
        for (auto& w : workers) {
            w->replay_routing_event();
            w->push_connection_state();   // triggers Node.js retry loop if needed
        }
    });

    // Main loop: tick workers (hotplug, fps)
    // Config reload is now driven by explicit reload_config IPC commands from Node.js
    std::cout << "[NDIMon-R] Running. Ctrl+C to exit.\n";

    while (g_running) {
        std::this_thread::sleep_for(std::chrono::milliseconds(500));
        for (auto& w : workers) w->tick();
    }

    // Cleanup
    std::cout << "[NDIMon-R] Shutting down...\n";
    ipc->stop();
    for (auto& w : workers) w->stop();
    workers.clear();
    if (drm_master_fd >= 0) close(drm_master_fd);
    NDIReceiver::ndi_destroy();
    std::cout << "[NDIMon-R] Done.\n";
    return 0;
}
