#pragma once
#include <string>
#include <functional>
#include <mutex>
#include <unordered_map>
#include <nlohmann/json.hpp>

struct DecoderConfig {
    int ch_num = 1;
    std::string ndi_audio = "NDIAudioEn";    // NDIAudioEn | NDIAudioDis
    std::string screensaver_mode = "BlackSS"; // SplashSS | BlackSS | CaptureSS
    std::string tally_mode = "TallyOff";      // TallyOn | TallyOff | VideoMode
    std::string color_space = "YUV";          // YUV | RGB
    std::string source_selection = "NDI";
};

struct TransportConfig {
    std::string rxpm = "TCP";  // TCP | UDP | Multicast | M-TCP | RUDP
};

struct FinderConfig {
    std::string discovery_server_mode = "NDIDisServDis"; // NDIDisServEn | NDIDisServDis
    std::string discovery_server_ip = "";
};

struct DeviceConfig {
    std::string mode = "decode";         // encode | decode
    std::string video_output = "hdmi";
    std::string device_name = "NDI Decoder";
    std::string host_name = "";
    std::string device_ip = "";          // populated at runtime, not persisted
    std::string ndi_recv_name = "";      // NDI alias shown in discovery tools (empty = auto)
    std::string decode_mode = "auto";    // auto | hardware | software (HX decode path)
};

struct NDIGroupConfig {
    std::string groups = "public";
};

// Per-output configuration (one per HDMI/DP connector)
struct OutputConfig {
    std::string preferred_mode;  // e.g. "1920x1080" or ""
    std::string scale_mode;      // "letterbox" | "stretch" | "crop"
    std::string source_name;
    std::string source_ip;
    std::string output_alias;    // custom output label (empty = use connector name)
    uint32_t    rotation = 0;    // display rotation: 0, 90, 180, 270 degrees
};

// Splash screen / screensaver appearance
struct SplashConfig {
    // Background fill colors (CSS-style "#RRGGBB")
    std::string bg_idle    = "#0D1B2A";  // when no source
    std::string bg_live    = "#0D2B1A";  // when source is available

    // Accent/border colors
    std::string accent_idle = "#4488CC";
    std::string accent_live = "#22FF88";

    // Logo image (absolute path to PNG/JPEG on device, empty = no logo)
    std::string logo_path  = "";
    float logo_x_pct = 50.0f;   // centre X as % of display width  [0–100]
    float logo_y_pct = 50.0f;   // centre Y as % of display height [0–100]
    float logo_w_pct = 50.0f;   // logo width as % of display width [5–80]

    // Overlay text rendered on screen
    std::string text_idle  = "No Signal";
    std::string text_live  = "Signal Available";
    float text_height_pct = 4.0f;   // font size as % of screen height [1–20]

    // Element visibility toggles
    bool show_box          = false;
    bool show_signal_text  = true;
    bool show_device_name  = true;
    bool show_device_url   = true;
    bool show_sources_available = true;
};

// On-screen display — overlaid on live video frames (disabled by default)
struct OsdConfig {
    bool        enabled = false;
    std::string text    = "";   // static string displayed top-centre
};

// Optional latency-tuning knobs. All defaults preserve current behaviour, so an
// existing install is unchanged until the user opts in via the web UI.
struct TuningConfig {
    int  display_queue_depth      = 2;     // uncompressed display queue (1..4); 1 = lowest latency
    bool decode_low_latency       = false; // force single-thread/low-delay software decode
    bool vaapi_low_delay          = false; // AV_CODEC_FLAG_LOW_DELAY on the VAAPI decoder
    bool framesync_bypass         = false; // Standard NDI: skip FrameSync (lower latency, weaker A/V sync)
    bool realtime_threads         = false; // SCHED_FIFO on recv + display threads
    bool cpu_performance_governor = false; // best-effort set the CPU governor to "performance" at start
    int  audio_periods            = 4;     // ALSA periods (2..4); fewer = lower audio latency
    int  audio_period_frames      = 1024;  // ALSA period size in frames
};

class Config {
public:
    static Config& instance();

    void load();

    // Save only device-level settings (mode, video output, NDI alias).
    // All other config files are owned by the Node.js API.
    void save_device();

    // Per-output config (ch_num is 1-based: 1=HDMI-A-1, 2=HDMI-A-2, 3=DP-1)
    OutputConfig get_output(int ch_num) const;
    void set_output(int ch_num, const OutputConfig& out);

    DecoderConfig   decoder;
    TransportConfig transport;
    FinderConfig    finder;
    DeviceConfig    device;
    NDIGroupConfig  ndi_group;
    SplashConfig    splash;
    OsdConfig       osd;
    TuningConfig    tuning;

    // Source connection (ch1 / legacy)
    std::string connected_source_name;
    std::string connected_source_ip;

    // Off-subnet IPs (comma-separated)
    std::string off_subnet_ips;

    // Signal callbacks for config changes
    using ChangeCallback = std::function<void()>;
    void on_source_change(ChangeCallback cb) { source_change_cb_ = std::move(cb); }
    void on_transport_change(ChangeCallback cb) { transport_change_cb_ = std::move(cb); }
    void notify_source_change() { if (source_change_cb_) source_change_cb_(); }
    void notify_transport_change() { if (transport_change_cb_) transport_change_cb_(); }

private:
    Config() = default;
    ChangeCallback source_change_cb_;
    ChangeCallback transport_change_cb_;

    static nlohmann::json read_json(const std::string& path);
    static void write_json(const std::string& path, const nlohmann::json& j);

    // Returns config file path for given ch_num
    static std::string settings_path(int ch_num);

    // Guards the in-memory config structs (decoder/transport/finder/device/...)
    // against concurrent load() from one thread and reads from others.
    // Per-output cache uses the same mutex.
    mutable std::mutex mutex_;
    mutable std::unordered_map<int, OutputConfig> output_cache_;
};
