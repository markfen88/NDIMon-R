#include "NDIReceiver.h"
#include <algorithm>
#include "NDIVideoLayout.h"
#include <cstddef>
#include <Processing.NDI.Lib.h>
#include <iostream>
#include <thread>
#include <chrono>
#include <cstring>
#include <atomic>
#include <algorithm>

bool NDIReceiver::ndi_initialized_ = false;

bool NDIReceiver::ndi_init() {
    if (ndi_initialized_) return true;
    if (!NDIlib_initialize()) {
        std::cerr << "[NDIRecv] NDIlib_initialize failed\n";
        return false;
    }
    ndi_initialized_ = true;
    std::cout << "[NDIRecv] NDI SDK initialized\n";
    return true;
}

void NDIReceiver::ndi_destroy() {
    if (ndi_initialized_) {
        NDIlib_destroy();
        ndi_initialized_ = false;
    }
}

RemoteSourceChange classify_source_change(const std::string& reported, const std::string& current) {
    // connect()/disconnect() record the source before the SDK reports it, so
    // only a name this receiver did not ask for came from a controller.
    if (reported == current) return {};
    return {true, reported};
}

NDIReceiver::NDIReceiver() = default;

NDIReceiver::~NDIReceiver() {
    stop_thread();
    destroy_recv();
}

void NDIReceiver::register_receiver() {
    if (!advertiser_ || !recv_ || registered_ || !advertise_) return;
    NDIlib_recv_advertiser_add_receiver(advertiser_, recv_, allow_control_.load(),
                                        /*allow_monitoring=*/true,
                                        input_name_.empty() ? nullptr : input_name_.c_str());
    registered_ = true;
    std::cout << "[NDIRecv] Registered as receiver with discovery server: " << discovery_server_
              << " (input='" << input_name_ << "', control="
              << (allow_control_ ? "allowed" : "off") << ")\n";
}

void NDIReceiver::unregister_receiver() {
    if (advertiser_ && recv_ && registered_)
        NDIlib_recv_advertiser_del_receiver(advertiser_, recv_);
    registered_ = false;
}

void NDIReceiver::destroy_advertiser() {
    unregister_receiver();
    if (advertiser_) {
        NDIlib_recv_advertiser_destroy(advertiser_);
        advertiser_ = nullptr;
    }
}

void NDIReceiver::advertiser_thread() {
    bool failing = false;
    for (;;) {
        std::string url;
        {
            std::lock_guard<std::recursive_mutex> control_lock(control_mutex_);
            if (advertise_ && !discovery_server_.empty() && !advertiser_) url = discovery_server_;
        }
        if (!url.empty()) {
            // Outside the lock: create waits for the Discovery Server.
            NDIlib_recv_advertiser_create_t adv = {};
            adv.p_url_address = url.c_str();
            NDIlib_recv_advertiser_instance_t created = NDIlib_recv_advertiser_create(&adv);
            std::lock_guard<std::recursive_mutex> control_lock(control_mutex_);
            if (created && advertise_ && !advertiser_ && url == discovery_server_) {
                advertiser_ = created;
                register_receiver();
                failing = false;
            } else if (created) {
                NDIlib_recv_advertiser_destroy(created);
            } else if (!failing) {
                failing = true;
                std::cerr << "[NDIRecv] WARNING: recv_advertiser_create failed for " << url
                          << " (no Discovery Server answers); retrying\n";
            }
        }
        std::unique_lock<std::mutex> lk(advertiser_wait_mutex_);
        advertiser_wait_.wait_for(lk, advertiser_retry_, [&] { return advertiser_stop_ || advertiser_wake_; });
        if (advertiser_stop_) return;
        advertiser_wake_ = false;
    }
}

void NDIReceiver::wake_advertiser() {
    {
        std::lock_guard<std::mutex> lk(advertiser_wait_mutex_);
        advertiser_wake_ = true;
    }
    advertiser_wait_.notify_all();
}

void NDIReceiver::set_advertising(bool advertise, bool allow_control, const std::string& input_name) {
    std::lock_guard<std::recursive_mutex> control_lock(control_mutex_);
    if (advertise == advertise_ && allow_control == allow_control_ && input_name == input_name_) return;
    unregister_receiver();
    advertise_ = advertise;
    allow_control_ = allow_control;
    input_name_ = input_name;
    if (!advertise_) destroy_advertiser();
    else register_receiver();
    wake_advertiser();
}

std::string NDIReceiver::find_source_url(const std::string& name, SourceLookupTiming& timing) {
    if (name.empty()) return {};
    const auto started = std::chrono::steady_clock::now();
    const auto elapsed_ms = [&] {
        return std::chrono::duration_cast<std::chrono::milliseconds>(
            std::chrono::steady_clock::now() - started).count();
    };
    NDIlib_find_create_t find_create = {};
    find_create.show_local_sources = true;
    if (!discovery_server_.empty())
        find_create.p_extra_ips = discovery_server_.c_str();

    const auto create_started = std::chrono::steady_clock::now();
    NDIlib_find_instance_t finder = NDIlib_find_create_v2(&find_create);
    const auto wait_started = std::chrono::steady_clock::now();
    timing.create_ms = std::chrono::duration_cast<std::chrono::milliseconds>(wait_started - create_started).count();
    if (!finder) return {};

    std::string url;
    // Leave headroom for the opaque SDK connect step within the API's
    // two-second IPC timeout. This bounds waiting, not SDK calls or joins.
    const auto deadline = started + std::chrono::milliseconds(500);
    for (;;) {
        uint32_t count = 0;
        const NDIlib_source_t* sources = NDIlib_find_get_current_sources(finder, &count);
        for (uint32_t j = 0; j < count; j++) {
            if (sources[j].p_ndi_name && name == sources[j].p_ndi_name) {
                if (sources[j].p_url_address)
                    url = sources[j].p_url_address;  // copy string before destroy
                break;
            }
        }
        if (!url.empty()) break;
        const auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(
            deadline - std::chrono::steady_clock::now()).count();
        if (remaining <= 0) break;
        NDIlib_find_wait_for_sources(finder, static_cast<uint32_t>(std::min<int64_t>(100, remaining)));
    }

    const auto destroy_started = std::chrono::steady_clock::now();
    timing.wait_ms = std::chrono::duration_cast<std::chrono::milliseconds>(destroy_started - wait_started).count();
    NDIlib_find_destroy(finder);
    timing.destroy_ms = std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now() - destroy_started).count();
    std::cout << "[NDIRecv] Source lookup scan " << (url.empty() ? "miss" : "hit")
              << " (" << elapsed_ms() << " ms)\n";
    return url;
}

// Create the recv instance without connecting to any source.
// This registers the device with the NDI discovery server as a receiver,
// making it permanently visible even when not connected to a source.
bool NDIReceiver::create_recv() {
    std::lock_guard<std::mutex> handle_lock(handle_mutex_);
    if (recv_) return true;  // already created
    NDIlib_recv_create_v3_t create = {};
    create.color_format       = NDIlib_recv_color_format_fastest;
    create.bandwidth          = NDIlib_recv_bandwidth_highest;
    create.allow_video_fields = true;   // fastest implies true; be explicit
    create.p_ndi_recv_name    = recv_name_.c_str();

    recv_ = NDIlib_recv_create_v3(&create);
    if (!recv_) {
        std::cerr << "[NDIRecv] NDIlib_recv_create_v3 failed\n";
        return false;
    }
    return true;
}

void NDIReceiver::recreate_recv_preserving_source() {
    std::lock_guard<std::recursive_mutex> control_lock(control_mutex_);
    // Remember current source so we can reconnect after the swap
    std::string src, ip;
    {
        std::lock_guard<std::mutex> lk(state_mutex_);
        src = current_source_;
        ip  = current_ip_;
    }

    stop_thread();

    // FrameSync is bound to the old recv — destroy it before the recv.
    {
        std::lock_guard<std::mutex> fs_lock(framesync_mutex_);
        if (framesync_) {
            audio_connected_=false;
            NDIlib_framesync_destroy(framesync_);
            framesync_ = nullptr;
        }
    }

    // Remove old recv from advertiser (keep advertiser alive)
    unregister_receiver();

    // Destroy old recv instance
    {
        std::lock_guard<std::mutex> handle_lock(handle_mutex_);
        if (recv_) {
            NDIlib_recv_destroy(recv_);
            recv_ = nullptr;
        }
    }

    // Create fresh recv (picks up the new name and/or new ndi-config transport)
    create_recv();

    // Re-register new recv with the existing advertiser
    register_receiver();

    // Reconnect to previous source if there was one; otherwise restart the
    // recv thread idle so the Discovery Server can still route it.
    if (!src.empty() && src != "None") {
        connect(src, ip);
    } else if (recv_) {
        running_ = true;
        recv_thread_ = std::thread(&NDIReceiver::recv_thread, this);
    }
}

void NDIReceiver::rename(const std::string& new_name) {
    std::lock_guard<std::recursive_mutex> control_lock(control_mutex_);
    if (new_name == recv_name_) return;
    recv_name_ = new_name;
    std::cout << "[NDIRecv] Renaming receiver to: " << new_name << "\n";
    recreate_recv_preserving_source();
}

bool NDIReceiver::init_recv() {
    std::lock_guard<std::recursive_mutex> control_lock(control_mutex_);
    if (!create_recv()) return false;

    // Register with the NDI discovery server as a receiver so this device
    // appears under "receivers" in NDI discovery tools. The advertiser thread
    // creates the advertiser once a configured Discovery Server answers.
    if (!advertiser_thread_.joinable())
        advertiser_thread_ = std::thread(&NDIReceiver::advertiser_thread, this);

    // Start recv + audio threads immediately so the DS can route the receiver
    // at any time, even before a source is selected (idle/fresh-start scenario).
    std::cout << "[NDIRecv] init_recv complete (ds='" << discovery_server_ << "')\n";
    running_ = true;
    recv_thread_ = std::thread(&NDIReceiver::recv_thread, this);
    return true;
}

void NDIReceiver::stop_thread() {
    // Always set running_ = false regardless of current state so that a
    // thread that self-exited (e.g., on disconnect) is properly joined.
    {
        std::lock_guard<std::mutex> fs_lock(framesync_mutex_);
        audio_connected_=false;
        running_=false;
    }
    if (recv_thread_.joinable()) recv_thread_.join();
}

void NDIReceiver::destroy_recv() {
    {
        std::lock_guard<std::mutex> lk(advertiser_wait_mutex_);
        advertiser_stop_ = true;
    }
    advertiser_wait_.notify_all();
    if (advertiser_thread_.joinable()) advertiser_thread_.join();
    // FrameSync must be destroyed before recv
    {
        std::lock_guard<std::mutex> fs_lock(framesync_mutex_);
        if (framesync_) {
            audio_connected_=false;
            NDIlib_framesync_destroy(framesync_);
            framesync_ = nullptr;
        }
    }
    {
        std::lock_guard<std::recursive_mutex> control_lock(control_mutex_);
        destroy_advertiser();
    }
    {
        std::lock_guard<std::mutex> handle_lock(handle_mutex_);
        if (recv_) {
            NDIlib_recv_destroy(recv_);
            recv_ = nullptr;
        }
    }
}

bool NDIReceiver::connect(const std::string& source_name, const std::string& source_ip) {
    const auto command_started = std::chrono::steady_clock::now();
    std::lock_guard<std::recursive_mutex> control_lock(control_mutex_);
    const auto stop_started = std::chrono::steady_clock::now();
    stop_thread();  // stop polling thread; keep recv_ alive for discovery
    const auto stop_finished = std::chrono::steady_clock::now();

    if (source_name.empty() || source_name == "None") {
        {
            std::lock_guard<std::mutex> lk(state_mutex_);
            current_source_.clear();
            current_ip_.clear();
        }
        // Stay connected to nothing — device remains visible as a receiver
        if (recv_) NDIlib_recv_connect(recv_, nullptr);
        if (conn_cb_) conn_cb_(false, "");
        // Keep threads running so the DS can still route the receiver
        running_ = true;
    recv_thread_ = std::thread(&NDIReceiver::recv_thread, this);
        return true;
    }

    std::cout << "[NDIRecv] Connect to NDI source: " << source_name << "\n";
    {
        std::lock_guard<std::mutex> lk(state_mutex_);
        current_source_ = source_name;
        current_ip_     = source_ip;
    }
    stream_type_    = NDIStreamType::Unknown;

    // Destroy old framesync — will be recreated if new source is standard
    const auto framesync_started = std::chrono::steady_clock::now();
    {
        std::lock_guard<std::mutex> fs_lock(framesync_mutex_);
        if (framesync_) {
            audio_connected_=false;
            NDIlib_framesync_destroy(framesync_);
            framesync_ = nullptr;
        }
    }
    const auto framesync_finished = std::chrono::steady_clock::now();

    // Ensure recv instance exists (for discovery registration)
    const auto create_started = std::chrono::steady_clock::now();
    if (!create_recv()) return false;
    const auto create_finished = std::chrono::steady_clock::now();

    // Resolve source URL — the strings backing the NDIlib_source_t pointers
    // must stay alive until NDIlib_recv_connect returns, so we use locals.
    std::string found_url;
    SourceLookupTiming lookup_timing;
    NDIlib_source_t src = {};
    src.p_ndi_name = source_name.c_str();
    if (!source_ip.empty()) {
        src.p_url_address = source_ip.c_str();
        std::cout << "[NDIRecv] Using IP: " << source_ip << "\n";
    } else {
        std::cout << "[NDIRecv] No IP provided, scanning via NDI Find...\n";
        found_url = find_source_url(source_name, lookup_timing);
        if (!found_url.empty()) {
            src.p_url_address = found_url.c_str();
            std::cout << "[NDIRecv] Found URL: " << found_url << "\n";
        } else {
            std::cout << "[NDIRecv] Source not found by scan, connecting by name only\n";
        }
    }

    // Register connection metadata BEFORE connecting so the SDK sends it in
    // the initial handshake rather than racing with async connection setup.
    // The Discovery Server learns the source from the SDK's own monitoring;
    // <ndi_routing> is the SDK's routing vocabulary and is never sent upstream.
    const auto metadata_started = std::chrono::steady_clock::now();
    NDIlib_recv_clear_connection_metadata(recv_);
    if (!connect_metadata_.empty()) {
        NDIlib_metadata_frame_t meta = {};
        meta.p_data = const_cast<char*>(connect_metadata_.c_str());
        NDIlib_recv_add_connection_metadata(recv_, &meta);
    }

    const auto connect_started = std::chrono::steady_clock::now();
    NDIlib_recv_connect(recv_, &src);
    const auto connect_finished = std::chrono::steady_clock::now();
    const auto milliseconds = [](auto begin, auto end) {
        return std::chrono::duration_cast<std::chrono::milliseconds>(end - begin).count();
    };
    std::cout << "[NDIRecv] Connect timing path=" << (source_ip.empty() ? "scan" : "ip")
              << " stop=" << milliseconds(stop_started, stop_finished)
              << " framesync_destroy=" << milliseconds(framesync_started, framesync_finished)
              << " recv_create=" << milliseconds(create_started, create_finished)
              << " find_create=" << lookup_timing.create_ms
              << " find_wait=" << lookup_timing.wait_ms
              << " find_destroy=" << lookup_timing.destroy_ms
              << " metadata=" << milliseconds(metadata_started, connect_started)
              << " recv_connect=" << milliseconds(connect_started, connect_finished)
              << " total=" << milliseconds(command_started, connect_finished) << " ms\n";

    running_ = true;
    recv_thread_ = std::thread(&NDIReceiver::recv_thread, this);
    return true;
}

void NDIReceiver::disconnect() {
    std::lock_guard<std::recursive_mutex> control_lock(control_mutex_);
    stop_thread();
    // Destroy FrameSync so the restarted recv_thread doesn't keep pulling
    // frames from the old session (which blocks frame_mutex_ and prevents splash).
    {
        std::lock_guard<std::mutex> fs_lock(framesync_mutex_);
        if (framesync_) {
            audio_connected_=false;
            NDIlib_framesync_destroy(framesync_);
            framesync_ = nullptr;
        }
    }
    // Keep recv_ alive — device stays visible as a receiver in discovery
    if (recv_) NDIlib_recv_connect(recv_, nullptr);
    {
        std::lock_guard<std::mutex> lk(state_mutex_);
        current_source_.clear();
        current_ip_.clear();
    }
    // Keep threads running so the DS can still route the receiver at any time
    running_ = true;
    recv_thread_ = std::thread(&NDIReceiver::recv_thread, this);
}

void NDIReceiver::reload_discovery(const std::string& new_ds) {
    std::lock_guard<std::recursive_mutex> control_lock(control_mutex_);
    if (new_ds == discovery_server_) return;
    discovery_server_ = new_ds;
    std::cout << "[NDIRecv] Reloading discovery server: '" << new_ds << "'\n";

    // Tear down the old advertiser; the advertiser thread creates one for the
    // new address (video connection is unaffected).
    destroy_advertiser();
    wake_advertiser();
}

bool NDIReceiver::is_connected() const {
    std::lock_guard<std::mutex> handle_lock(handle_mutex_);
    if (!recv_) return false;
    return NDIlib_recv_get_no_connections(recv_) > 0;
}

void NDIReceiver::set_transport(const std::string& rxpm) {
    if (rxpm == transport_mode_) return;
    transport_mode_ = rxpm;
    std::cout << "[NDIRecv] Transport mode set to: " << rxpm << "\n";

    // NDI transport is negotiated by the SDK from ndi-config.v1.json, which it
    // reads only when the process starts (recreating the recv does not re-read
    // it). write_ndi_sdk_config() in main.cpp writes the file and the worker
    // restarts the process when this value differs from the configured one;
    // here we only remember what this process started with.
}

void NDIReceiver::set_tally(bool on_program, bool on_preview) {
    std::lock_guard<std::mutex> handle_lock(handle_mutex_);
    if (!recv_) return;
    NDIlib_tally_t tally = {};
    tally.on_program = on_program;
    tally.on_preview = on_preview;
    NDIlib_recv_set_tally(recv_, &tally);
}

void NDIReceiver::free_video(NDIVideoFrame& f) {
    std::lock_guard<std::mutex> handle_lock(handle_mutex_);
    if (recv_ && f.ndi_frame) {
        NDIlib_recv_free_video_v2(recv_, f.ndi_frame);
        f.ndi_frame = nullptr;
    }
}

void NDIReceiver::free_video(NDIlib_video_frame_v2_t* frame) {
    std::lock_guard<std::mutex> handle_lock(handle_mutex_);
    if (recv_ && frame)
        NDIlib_recv_free_video_v2(recv_, frame);
}

void NDIReceiver::free_audio(NDIAudioFrame& f) {
    std::lock_guard<std::mutex> handle_lock(handle_mutex_);
    if (recv_ && f.ndi_frame) {
        NDIlib_recv_free_audio_v3(recv_, f.ndi_frame);
        f.ndi_frame = nullptr;
    }
}

int NDIReceiver::video_queue_depth() {
    std::lock_guard<std::mutex> handle_lock(handle_mutex_);
    if (!recv_) return -1;
    NDIlib_recv_queue_t queue = {};
    NDIlib_recv_get_queue(recv_, &queue);
    return queue.video_frames;
}

void NDIReceiver::get_performance(int& total_frames, int& dropped_frames) {
    NDIlib_recv_queue_t queue = {};
    {
        std::lock_guard<std::mutex> handle_lock(handle_mutex_);
        if (!recv_) { total_frames = dropped_frames = 0; return; }
        NDIlib_recv_performance_t total = {}, dropped = {};
        NDIlib_recv_get_performance(recv_, &total, &dropped);
        total_frames = (int)total.video_frames;
        dropped_frames = (int)dropped.video_frames;
        NDIlib_recv_get_queue(recv_, &queue);
    }
    if (queue.video_frames > 5)
        std::cerr << "[NDIRecv] WARNING: video queue depth " << queue.video_frames
                  << " — receiver is falling behind\n";
}

// Detect whether a FourCC is a compressed (HX) codec
static bool is_compressed_fourcc(NDIlib_FourCC_video_type_e fourcc) {
    uint32_t fcc = (uint32_t)fourcc;
    return fcc == NDI_LIB_FOURCC('H','2','6','4') ||
           fcc == NDI_LIB_FOURCC('H','2','6','5') ||
           fcc == NDI_LIB_FOURCC('h','2','6','4') ||
           fcc == NDI_LIB_FOURCC('h','2','6','5') ||
           fcc == NDI_LIB_FOURCC('H','E','V','C') ||
           fcc == NDI_LIB_FOURCC('A','V','C','1');
}

// Build an NDIVideoFrame from a raw SDK video frame
static NDIVideoFrame make_video_frame(NDIlib_video_frame_v2_t& raw) {
    NDIVideoFrame vf;
    vf.width        = raw.xres;
    vf.height       = raw.yres;
    vf.frame_rate_n = raw.frame_rate_N;
    vf.frame_rate_d = raw.frame_rate_D;
    vf.fourcc       = raw.FourCC;
    vf.data         = raw.p_data;
    vf.timestamp    = raw.timestamp;
    vf.frame_format = raw.frame_format_type;
    vf.ndi_frame    = &raw;

    if (is_compressed_fourcc(raw.FourCC)) {
        vf.size   = raw.data_size_in_bytes;
        vf.stride = 0;
    } else {
        const auto layout = ndi_video_layout(raw.FourCC, raw.xres, raw.yres,
                                             raw.line_stride_in_bytes);
        vf.stride = layout.stride;
        vf.size = layout.bytes;
    }
    return vf;
}

void NDIReceiver::recv_thread() {
    std::cout << "[NDIRecv] START recv thread\n";

    bool was_connected = false;
    auto probe_start   = std::chrono::steady_clock::now();
    std::string src_snap;
    {
        std::lock_guard<std::mutex> lk(state_mutex_);
        src_snap = current_source_;
    }
    bool probe_active  = !src_snap.empty() && src_snap != "None";

    // A new thread is a new connection (connect() already destroyed any
    // FrameSync). Its first video frame decides the stream type, even when the
    // SDK delivers it before this loop has seen the connection count change; a
    // flag left over from the previous connection skipped the FrameSync setup
    // for exactly that frame (NI-18).
    first_frame_logged_ = false;
    stream_type_ = NDIStreamType::Unknown;
    auto connected_at = std::chrono::steady_clock::now();
    bool no_video_reported = false;
    // A source change frame that was missed is caught by comparing the SDK's
    // source every half second. Not at the start of a thread: connect() has
    // just switched the source and the SDK may still name the previous one.
    const auto thread_started = std::chrono::steady_clock::now();
    auto last_source_check = thread_started;

    try {
    while (running_) {
        recv_heartbeat_ms_ = std::chrono::duration_cast<std::chrono::milliseconds>(
            std::chrono::steady_clock::now().time_since_epoch()).count();

        if (!recv_) {
            std::this_thread::sleep_for(std::chrono::milliseconds(100));
            continue;
        }

        // ---- Connection status management ----
        int connections = NDIlib_recv_get_no_connections(recv_);
        audio_connected_ = running_ && connections > 0;
        if (connections > 0 && !was_connected) {
            was_connected = true;
            probe_active  = false;
            connected_at  = std::chrono::steady_clock::now();
            // Stream detection is not re-armed here: the thread start and the
            // disconnect below do that. A frame may already have been captured
            // (and a FrameSync created) for this very connection.
            {
                std::lock_guard<std::mutex> lk(state_mutex_);
                src_snap = current_source_;
            }
            std::cout << "[NDIRecv] Connected to " << src_snap << "\n";
            if (conn_cb_) conn_cb_(true, src_snap);
        } else if (connections == 0 && was_connected) {
            was_connected = false;
            {
                std::lock_guard<std::mutex> lk(state_mutex_);
                src_snap = current_source_;
            }
            probe_active  = !src_snap.empty() && src_snap != "None";
            probe_start   = std::chrono::steady_clock::now();
            {
                std::lock_guard<std::mutex> fs_lock(framesync_mutex_);
                if (framesync_) {
                    audio_connected_=false;
                    NDIlib_framesync_destroy(framesync_);
                    framesync_ = nullptr;
                }
            }
            // The next connection may carry another stream type: detect again.
            first_frame_logged_ = false;
            stream_type_ = NDIStreamType::Unknown;
            no_video_reported = false;
            // The receiver stays on its source: the SDK reconnects by itself when
            // it returns. Dropping it here threw away a Discovery Server route.
            if (running_) {
                std::cout << "[NDIRecv] Disconnected from " << src_snap << "\n";
                if (conn_cb_) conn_cb_(false, src_snap);
            }
        } else if (!was_connected && probe_active && running_) {
            using clock = std::chrono::steady_clock;
            auto elapsed = std::chrono::duration_cast<std::chrono::seconds>(
                clock::now() - probe_start).count();
            if (elapsed >= 5) {
                std::cout << "[NDIRecv] Source unavailable: " << src_snap << "\n";
                if (conn_cb_) conn_cb_(false, src_snap);
                probe_start = clock::now();
            }
        }

        {
            const auto now = std::chrono::steady_clock::now();
            if (now - thread_started >= std::chrono::seconds(2) &&
                now - last_source_check >= std::chrono::milliseconds(500)) {
                last_source_check = now;
                on_source_change(false);
            }
        }

        // ==================================================================
        // DUAL-MODE CAPTURE
        // Standard NDI: FrameSync pull model (better A/V sync, display timing)
        // HX NDI:       capture_v3 push model (every compressed frame matters)
        // ==================================================================

        if (framesync_) {
            // ---- STANDARD MODE: FrameSync pull ----
            // FrameSync always returns immediately with the best available frame.
            // For video: returns latest frame (duplicates if needed for display sync).
            // For audio: returns exactly the requested samples (inserts silence if needed).

            // Pull video
            NDIlib_video_frame_v2_t video_frame{};
            NDIlib_framesync_capture_video(framesync_, &video_frame,
                                           NDIlib_frame_format_type_progressive);
            if (video_frame.p_data && pull_cb_) pull_cb_(video_frame.timestamp);
            if (video_frame.p_data && video_cb_) {
                if (!first_frame_logged_) {
                    first_frame_logged_ = true;
                    char fcc[5] = {};
                    memcpy(fcc, &video_frame.FourCC, 4);
                    std::cout << "[NDIRecv] First frame (Standard/FrameSync): "
                              << video_frame.xres << "x" << video_frame.yres
                              << " FourCC='" << fcc << "'"
                              << " stride=" << video_frame.line_stride_in_bytes << "\n";
                }
                NDIVideoFrame vf = make_video_frame(video_frame);
                vf.ndi_frame = nullptr;  // framesync frames use separate free
                vf.framesync = true;
                if (vf.size > 0) video_cb_(vf);
            }
            NDIlib_framesync_free_video(framesync_, &video_frame);

            // Pull audio sized to elapsed time so we don't over-request vs the
            // ~8 ms loop. NDI FrameSync should be driven from a local timebase;
            // ALSA write blocking plus this sample count keep A/V near realtime.
            if (!audio_pull_mode_ && audio_enabled_ && audio_cb_) {
                auto now = std::chrono::steady_clock::now();
                int samples = 1024;
                if (fs_audio_have_) {
                    auto us = std::chrono::duration_cast<std::chrono::microseconds>(
                        now - fs_audio_last_).count();
                    samples = static_cast<int>(48000LL * us / 1000000LL);
                }
                if (samples >= 160) {
                    if (samples > 2048) samples = 2048;
                    NDIlib_audio_frame_v2_t audio_frame{};
                    NDIlib_framesync_capture_audio(framesync_, &audio_frame,
                                                   48000, 0, samples);
                    if (audio_frame.p_data) {
                        NDIAudioFrame af;
                        af.sample_rate    = audio_frame.sample_rate;
                        af.channels       = audio_frame.no_channels;
                        af.num_samples    = audio_frame.no_samples;
                        af.channel_stride = audio_frame.channel_stride_in_bytes;
                        af.data           = audio_frame.p_data;
                        af.ndi_frame      = nullptr;
                        audio_cb_(af);
                    }
                    NDIlib_framesync_free_audio(framesync_, &audio_frame);
                    fs_audio_last_ = now;
                    fs_audio_have_ = true;
                }
            }

            // FrameSync doesn't deliver metadata — poll recv for metadata/status
            NDIlib_metadata_frame_t metadata{};
            NDIlib_frame_type_e mt = NDIlib_recv_capture_v3(
                recv_, nullptr, nullptr, &metadata, 0);
            if (mt == NDIlib_frame_type_metadata && metadata.p_data) {
                std::cout << "[NDIRecv] Metadata: " << std::string(metadata.p_data).substr(0, 120) << "\n";
                NDIlib_recv_free_metadata(recv_, &metadata);
            } else if (mt == NDIlib_frame_type_source_change) {
                on_source_change();
            }

            // Small sleep to pace the pull loop (~60Hz)
            std::this_thread::sleep_for(std::chrono::milliseconds(8));

        } else {
            // ---- HX MODE / DETECTION: capture_v3 push ----
            NDIlib_video_frame_v2_t video_frame{};
            NDIlib_audio_frame_v3_t audio_frame_storage{};
            NDIlib_audio_frame_v3_t* audio_ptr = audio_enabled_ ? &audio_frame_storage : nullptr;
            NDIlib_metadata_frame_t metadata{};

            // Take a waiting video frame before any audio. A capture that asks
            // for both returned audio first for as long as audio was queued
            // (measured on the SDK, 6.3.2), and every audio frame ends in a
            // blocking ALSA write that runs at real time: after a stalled
            // callback (the mode change for the first frame) the backlog never
            // drained and video was starved for good (NI-18).
            NDIlib_frame_type_e frame_type = NDIlib_recv_capture_v3(
                recv_, &video_frame, nullptr, &metadata, 0);
            if (frame_type == NDIlib_frame_type_none)
                frame_type = NDIlib_recv_capture_v3(
                    recv_, &video_frame, audio_ptr, &metadata, 50);

            if (stream_type_ == NDIStreamType::Unknown && was_connected && !no_video_reported &&
                std::chrono::steady_clock::now() - connected_at >= std::chrono::seconds(3)) {
                NDIlib_recv_queue_t queue = {};
                NDIlib_recv_get_queue(recv_, &queue);
                std::cout << "[NDIRecv] No video frame 3 s after the connection (queued video="
                          << queue.video_frames << " audio=" << queue.audio_frames
                          << "); audio-only source?\n";
                no_video_reported = true;
            }

            switch (frame_type) {
                case NDIlib_frame_type_video: {
                    bool compressed = is_compressed_fourcc(video_frame.FourCC);

                    // Log first frame and detect stream type
                    if (!first_frame_logged_) {
                        first_frame_logged_ = true;
                        char fcc[5] = {};
                        uint32_t f = (uint32_t)video_frame.FourCC;
                        fcc[0] = (char)(f & 0xff);
                        fcc[1] = (char)((f >> 8) & 0xff);
                        fcc[2] = (char)((f >> 16) & 0xff);
                        fcc[3] = (char)((f >> 24) & 0xff);

                        if (compressed) {
                            stream_type_ = NDIStreamType::HX;
                            std::cout << "[NDIRecv] First frame (HX): "
                                      << video_frame.xres << "x" << video_frame.yres
                                      << " FourCC='" << fcc << "'"
                                      << " size=" << video_frame.data_size_in_bytes << "\n";
                        } else {
                            stream_type_ = NDIStreamType::Standard;
                            std::cout << "[NDIRecv] First frame (Standard): "
                                      << video_frame.xres << "x" << video_frame.yres
                                      << " FourCC='" << fcc << "'"
                                      << " stride=" << video_frame.line_stride_in_bytes << "\n";
                            // Create FrameSync for standard streams — provides
                            // display-sync frame timing and A/V sync with automatic
                            // frame duplication/dropping and silence insertion.
                            {
                                std::lock_guard<std::mutex> fs_lock(framesync_mutex_);
                                framesync_ = NDIlib_framesync_create(recv_);
                            }
                            if (framesync_) {
                                first_frame_logged_ = false;  // reset so FrameSync logs its FourCC
                                fs_audio_have_ = false;
                                std::cout << "[NDIRecv] FrameSync enabled for Standard NDI\n";
                                // Free the current frame and let FrameSync take over
                                NDIlib_recv_free_video_v2(recv_, &video_frame);
                                break;
                            }
                            std::cerr << "[NDIRecv] FrameSync creation failed, using capture_v3\n";
                        }
                    }

                    // HDR warning
                    if (video_frame.FourCC == NDIlib_FourCC_video_type_P216 ||
                        video_frame.FourCC == NDIlib_FourCC_video_type_PA16) {
                        std::cerr << "[NDIRecv] WARNING: HDR source detected (FourCC="
                                  << (video_frame.FourCC == NDIlib_FourCC_video_type_P216
                                      ? "P216" : "PA16")
                                  << "). color_format_fastest does not down-map HDR to SDR.\n";
                    }

                    if (video_cb_) {
                        NDIVideoFrame vf = make_video_frame(video_frame);
                        if (vf.size > 0) video_cb_(vf);
                        if (vf.ndi_frame)
                            NDIlib_recv_free_video_v2(recv_, &video_frame);
                    } else {
                        NDIlib_recv_free_video_v2(recv_, &video_frame);
                    }
                    break;
                }

                case NDIlib_frame_type_audio: {
                    if (audio_ptr && audio_cb_) {
                        NDIAudioFrame af;
                        af.sample_rate    = audio_ptr->sample_rate;
                        af.channels       = audio_ptr->no_channels;
                        af.num_samples    = audio_ptr->no_samples;
                        af.channel_stride = audio_ptr->channel_stride_in_bytes;
                        af.data           = (float*)audio_ptr->p_data;
                        af.ndi_frame      = audio_ptr;
                        audio_cb_(af);
                        if (af.ndi_frame)
                            NDIlib_recv_free_audio_v3(recv_, audio_ptr);
                    } else if (audio_ptr) {
                        NDIlib_recv_free_audio_v3(recv_, audio_ptr);
                    }
                    break;
                }

                case NDIlib_frame_type_metadata: {
                    if (metadata.p_data)
                        std::cout << "[NDIRecv] Metadata: " << std::string(metadata.p_data).substr(0, 120) << "\n";
                    NDIlib_recv_free_metadata(recv_, &metadata);
                    break;
                }

                case NDIlib_frame_type_status_change:
                    break;

                case NDIlib_frame_type_source_change:
                    on_source_change();
                    break;

                case NDIlib_frame_type_error:
                    std::cerr << "[NDIRecv] Frame error\n";
                    std::this_thread::sleep_for(std::chrono::milliseconds(100));
                    break;

                case NDIlib_frame_type_none:
                default:
                    break;
            }
        } // end dual-mode capture
    } // while (running_)
    } catch (const std::exception& e) {
        std::cerr << "[NDIRecv] Thread exception: " << e.what() << "\n";
    } catch (...) {
        std::cerr << "[NDIRecv] Thread unknown exception\n";
    }

    std::cout << "[NDIRecv] STOPPED recv thread\n";
}

// The SDK switched the receive instance itself: on a Discovery Server connect
// command (allow_controlling) or after our own connect(). Never answer with a
// connect; the control plane records the new source.
void NDIReceiver::on_source_change(bool event) {
    // The name is the source the SDK is assigned to, NULL only after a
    // controller disconnect (a lost sender keeps its name). The return value
    // only says whether it changed since the last call, so it is not used:
    // the RK3588 once delivered a disconnect frame whose change was missed.
    const char* p_src = nullptr;
    NDIlib_recv_get_source_name(recv_, &p_src, 0);
    const std::string reported = p_src ? p_src : "";
    if (p_src) NDIlib_recv_free_string(recv_, p_src);
    RemoteSourceChange change;
    {
        std::lock_guard<std::mutex> lk(state_mutex_);
        change = classify_source_change(reported, current_source_);
        if (change.remote) {
            // A routed source is known by name only; an old endpoint would
            // send the next reconnect back to the previous source.
            current_source_ = change.source;
            current_ip_.clear();
        }
    }
    if (event || change.remote)
        std::cout << "[NDIRecv] Source change: '" << reported << "'"
                  << (change.remote ? " (Discovery Server)" : "") << "\n";
    if (change.remote && remote_cb_) remote_cb_(change.source);
}

int NDIReceiver::pull_audio(int frames, int channels, float* const* planes, int* captured_channels) {
    if(captured_channels)*captured_channels=0;
    if (!audio_enabled_ || !audio_pull_mode_ || !audio_connected_ || frames<=0 || channels<1 || channels>16) return 0;
    std::unique_lock<std::mutex> lock(framesync_mutex_,std::try_to_lock);
    if (!lock.owns_lock() || !framesync_ || !running_ || !audio_connected_) return 0;
    NDIlib_audio_frame_v2_t f{};
    NDIlib_framesync_capture_audio(framesync_,&f,48000,0,frames);
    int count=f.p_data && f.no_channels>0 && f.no_channels<=16 ? std::clamp(f.no_samples,0,frames) : 0;
    if(captured_channels && count>0)*captured_channels=std::min(channels,f.no_channels);
    for(int c=0;c<channels;++c) {
        if(c<f.no_channels && count>0) {
            const auto* p=reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(f.p_data)+size_t(c)*f.channel_stride_in_bytes);
            std::copy_n(p,count,planes[c]);
        } else std::fill_n(planes[c],count,0.f);
        std::fill(planes[c]+count,planes[c]+frames,0.f);
    }
    NDIlib_framesync_free_audio(framesync_,&f);
    return count;
}
