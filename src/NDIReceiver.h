#pragma once
#include <string>
#include <thread>
#include <atomic>
#include <functional>
#include <memory>
#include <cstddef>
#include <cstdint>
#include <Processing.NDI.Lib.h>
#include <Processing.NDI.RecvAdvertiser.h>

enum class NDIStreamType { Standard, HX };

struct NDIVideoFrame {
    int            width  = 0;
    int            height = 0;
    int            frame_rate_n = 0;
    int            frame_rate_d = 0;
    NDIlib_FourCC_video_type_e fourcc = NDIlib_FourCC_video_type_UYVY;
    uint8_t*       data    = nullptr;
    int            stride  = 0;    // line stride (uncompressed) or 0
    int            size    = 0;    // data size (compressed) or 0
    int64_t        timestamp = 0;
    NDIlib_frame_format_type_e frame_format = NDIlib_frame_format_type_progressive;
    // Tally
    bool tally_on_program = false;
    bool tally_on_preview = false;
    // Original frame ptr for free
    NDIlib_video_frame_v2_t* ndi_frame = nullptr;
};

struct NDIAudioFrame {
    int      sample_rate = 0;
    int      channels    = 0;
    int      num_samples = 0;
    float**  channel_data = nullptr;
    int      channel_stride = 0;
    NDIlib_audio_frame_v3_t* ndi_frame = nullptr;
};

// Video callback: called from recv thread. Caller must call recv->free_video(frame) after use.
using NDIVideoCallback      = std::function<void(NDIVideoFrame&)>;
using NDIAudioCallback      = std::function<void(NDIAudioFrame&)>;
using NDIConnectionCallback = std::function<void(bool connected, const std::string& source_name)>;
// Routing callback: fired when the discovery server assigns a new source.
// source_name is empty string to disconnect.
using NDIRoutingCallback    = std::function<void(const std::string& source_name, const std::string& source_url)>;

class NDIReceiver {
public:
    NDIReceiver();
    ~NDIReceiver();

    static bool ndi_init();
    static void ndi_destroy();

    // Connect to a named source (empty = disconnect)
    bool connect(const std::string& source_name, const std::string& source_ip = "");
    void disconnect();

    bool is_connected() const;
    std::string current_source() const { return current_source_; }

    // Set transport mode (TCP/UDP/Multicast)
    void set_transport(const std::string& rxpm);

    // Set discovery server IP (used in find_source and for reconnect)
    void set_discovery_server(const std::string& ip) { discovery_server_ = ip; }

    // Set the NDI receiver name visible in discovery tools.
    // Call rename() instead if the recv instance is already running.
    void set_recv_name(const std::string& name) { recv_name_ = name; }
    std::string get_recv_name() const { return recv_name_; }

    // Change the name live: destroys the old recv instance (keeping the
    // advertiser), creates a fresh one with the new name, re-registers it,
    // and reconnects to whatever source was active.
    void rename(const std::string& new_name);

    // Create the recv instance for discovery registration without connecting.
    // Call once at startup (before connect()) so the device is always visible
    // as a receiver in the NDI discovery server.
    bool init_recv();

    // Enable/disable audio
    void set_audio_enabled(bool enable) { audio_enabled_ = enable; }

    // Set metadata XML to send to the source/DS on every connection.
    // Replaces any previously queued metadata. Safe to call at any time.
    void set_connect_metadata(const std::string& xml) { connect_metadata_ = xml; }

    // Tally
    void set_tally(bool on_program, bool on_preview);

    void set_video_callback(NDIVideoCallback cb)         { video_cb_   = std::move(cb); }
    void set_audio_callback(NDIAudioCallback cb)         { audio_cb_   = std::move(cb); }
    void set_connection_callback(NDIConnectionCallback cb) { conn_cb_  = std::move(cb); }
    void set_routing_callback(NDIRoutingCallback cb)     { routing_cb_ = std::move(cb); }

    void free_video(NDIVideoFrame& f);
    void free_audio(NDIAudioFrame& f);

    // Performance stats
    void get_performance(int& total_frames, int& dropped_frames);

private:
    void recv_thread();
    bool create_recv();                               // create without source (discovery only)
    void destroy_recv();                              // only called on full shutdown
    void stop_thread();                               // stop recv thread, keep recv_ alive
    NDIlib_source_t find_source(const std::string& name, const std::string& ip);

    NDIlib_recv_instance_t recv_           = nullptr;
    NDIlib_recv_advertiser_instance_t advertiser_ = nullptr;
    std::thread recv_thread_;
    std::atomic<bool> running_{false};
    std::atomic<bool> audio_enabled_{true};

    std::string current_source_;
    std::string current_ip_;
    std::string transport_mode_{"TCP"};
    std::string discovery_server_;
    std::string recv_name_{"NDI Decoder"};
    std::string connect_metadata_;   // sent to source/DS on every connection (SDK-managed)

    NDIVideoCallback      video_cb_;
    NDIAudioCallback      audio_cb_;
    NDIConnectionCallback conn_cb_;
    NDIRoutingCallback    routing_cb_;

    static bool ndi_initialized_;
};
