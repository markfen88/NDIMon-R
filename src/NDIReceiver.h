#pragma once
#include <string>
#include <thread>
#include <atomic>
#include <functional>
#include <memory>
#include <mutex>
#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <Processing.NDI.Lib.h>
#include <Processing.NDI.RecvAdvertiser.h>

enum class NDIStreamType { Unknown, Standard, HX };

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
    bool framesync = false;
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
    float*   data = nullptr;            // base pointer to planar float audio
    int      channel_stride = 0;        // bytes between channel planes
    NDIlib_audio_frame_v3_t* ndi_frame = nullptr;
};

// Video callback: called from recv thread. Caller must call recv->free_video(frame) after use.
using NDIVideoCallback      = std::function<void(NDIVideoFrame&)>;
using NDIAudioCallback      = std::function<void(NDIAudioFrame&)>;
using NDIConnectionCallback = std::function<void(bool connected, const std::string& source_name)>;
// Remote source callback: fired from the recv thread when the SDK switched the
// receiver to another source on a Discovery Server command (allow_controlling).
// source_name is empty when the controller disconnected the receiver.
using NDIRemoteSourceCallback = std::function<void(const std::string& source_name)>;

// What the recv thread does with an SDK source_change, given the name the SDK
// reports and the source this receiver was last told to use. A change this
// receiver made itself is not a remote one.
struct RemoteSourceChange {
    bool remote = false;
    std::string source;   // "" = disconnected
};
RemoteSourceChange classify_source_change(const std::string& reported, const std::string& current);

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
    std::string current_source() const {
        std::lock_guard<std::mutex> lk(state_mutex_);
        return current_source_;
    }

    // Remember the transport mode (RUDP/TCP/M-TCP/UDP/Multicast) the process was
    // started with. The SDK only reads it at startup, so a different configured
    // value is applied by restarting the process, not by this object.
    void set_transport(const std::string& rxpm);
    std::string get_transport() const { return transport_mode_; }

    // Set discovery server IP (used in find_source and for reconnect)
    void set_discovery_server(const std::string& ip) { discovery_server_ = ip; }
    std::string get_discovery_server() const { return discovery_server_; }

    // Recreate the NDI advertiser with a new discovery server address.
    // Call when DS settings change at runtime. Does not interrupt video connection.
    void reload_discovery(const std::string& new_ds);

    // Whether to register with the Discovery Server, whether it may route this
    // receiver, and the receiver input name it shows. A change re-registers the
    // receiver with the advertiser only; the video connection is unaffected.
    void set_advertising(bool advertise, bool allow_control, const std::string& input_name);
    bool remote_control_allowed() const { return allow_control_.load(); }
    // How often a Discovery Server that did not answer is tried again (tests).
    void set_advertiser_retry(std::chrono::milliseconds interval) { advertiser_retry_ = interval; }

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

    // Stream type detected from first frame (Unknown until first frame)
    NDIStreamType stream_type() const { return stream_type_; }

    // Enable/disable audio
    void set_audio_enabled(bool enable) { audio_enabled_ = enable; }
    void set_audio_pull_mode(bool enabled) { audio_pull_mode_ = enabled; }
    int pull_audio(int frames, int channels, float* const* planes, int* captured_channels = nullptr);
    bool audio_enabled() const { return audio_enabled_.load(); }

    // Set metadata XML to send to the source/DS on every connection.
    // Replaces any previously queued metadata. Safe to call at any time.
    void set_connect_metadata(const std::string& xml) { connect_metadata_ = xml; }

    // Tally
    void set_tally(bool on_program, bool on_preview);

    void set_video_callback(NDIVideoCallback cb)         { video_cb_   = std::move(cb); }
    // Diagnostics: called with the NDI timestamp of every FrameSync video pull.
    void set_pull_callback(std::function<void(int64_t)> cb) { pull_cb_ = std::move(cb); }
    int video_queue_depth();
    void set_audio_callback(NDIAudioCallback cb)         { audio_cb_   = std::move(cb); }
    void set_connection_callback(NDIConnectionCallback cb) { conn_cb_  = std::move(cb); }
    void set_remote_source_callback(NDIRemoteSourceCallback cb) { remote_cb_ = std::move(cb); }

    void free_video(NDIVideoFrame& f);
    void free_video(NDIlib_video_frame_v2_t* frame);   // free raw SDK frame (cross-thread safe)
    void free_audio(NDIAudioFrame& f);

    // Performance stats
    void get_performance(int& total_frames, int& dropped_frames);

    // Recv thread heartbeat (monotonic ms, for health monitoring)
    int64_t recv_heartbeat_ms() const { return recv_heartbeat_ms_.load(); }

private:
    void recv_thread();
    // Compare the SDK's source with the one this receiver was told to use;
    // `event` is true for an SDK source_change frame (logged even when equal).
    void on_source_change(bool event = true);
    // Registration with the Discovery Server. Callers hold control_mutex_.
    void register_receiver();
    void unregister_receiver();
    void destroy_advertiser();
    // advertiser_create returns NULL while no Discovery Server answers (e.g. at
    // boot before the network is up). This thread keeps trying until it does.
    void advertiser_thread();
    void wake_advertiser();
    // Destroy the current recv instance and create a fresh one (new name),
    // re-register with the advertiser, and reconnect to whatever source was
    // active. Used by rename().
    void recreate_recv_preserving_source();
    bool create_recv();                               // create without source (discovery only)
    void destroy_recv();                              // only called on full shutdown
    void stop_thread();                               // stop recv thread, keep recv_ alive
    struct SourceLookupTiming { int64_t create_ms{}, wait_ms{}, destroy_ms{}; };
    std::string find_source_url(const std::string& name, SourceLookupTiming& timing);

    // Serialize hotplug/control operations separately from short SDK reads.
    // Never hold handle_mutex_ while joining the receive thread or invoking callbacks.
    std::recursive_mutex control_mutex_;
    mutable std::mutex handle_mutex_;
    NDIlib_recv_instance_t recv_           = nullptr;
    NDIlib_recv_advertiser_instance_t advertiser_ = nullptr;
    NDIlib_framesync_instance_t framesync_ = nullptr;
    std::thread recv_thread_;
    std::atomic<bool> running_{false};
    std::atomic<bool> audio_enabled_{true};
    std::atomic<bool> audio_pull_mode_{false}, audio_connected_{false};
    std::mutex framesync_mutex_;
    std::atomic<NDIStreamType> stream_type_{NDIStreamType::Unknown};

    // Protects current_source_ / current_ip_ — written by recv_thread on
    // routing/source-change metadata and by connect()/disconnect() from main.
    mutable std::mutex state_mutex_;
    std::string current_source_;
    std::string current_ip_;
    std::string transport_mode_{"TCP"};
    std::string discovery_server_;
    std::string recv_name_{"NDI Decoder"};
    std::string connect_metadata_;   // sent to source/DS on every connection (SDK-managed)

    // Advertiser registration settings (guarded by control_mutex_ except the atomics).
    std::atomic<bool> advertise_{true}, allow_control_{true};
    std::string input_name_;
    bool registered_ = false;
    std::thread advertiser_thread_;
    std::mutex advertiser_wait_mutex_;
    std::condition_variable advertiser_wait_;
    bool advertiser_stop_ = false, advertiser_wake_ = false;
    std::chrono::milliseconds advertiser_retry_{10000};

    NDIVideoCallback      video_cb_;
    std::function<void(int64_t)> pull_cb_;
    NDIAudioCallback      audio_cb_;
    NDIConnectionCallback conn_cb_;
    NDIRemoteSourceCallback remote_cb_;

    std::atomic<int64_t> recv_heartbeat_ms_{0};

    bool first_frame_logged_ = false;  // log FourCC/size of very first frame for diagnostics

    std::chrono::steady_clock::time_point fs_audio_last_{};
    bool fs_audio_have_{false};

    static bool ndi_initialized_;
};
