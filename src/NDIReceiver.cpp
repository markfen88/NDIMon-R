#include "NDIReceiver.h"
#include <cstddef>
#include <Processing.NDI.Lib.h>
#include <iostream>
#include <thread>
#include <chrono>
#include <cstring>
#include <atomic>
#include <regex>

// Extract a named XML attribute value from a simple single-element XML string.
// e.g. xml_attr("<source name=\"foo\" url=\"bar\"/>", "name") -> "foo"
static std::string xml_attr(const std::string& xml, const std::string& attr) {
    // Match: attr="value" or attr='value'
    std::regex re(attr + R"(=[\"']([^\"']*)[\"'])");
    std::smatch m;
    if (std::regex_search(xml, m, re)) return m[1].str();
    return {};
}

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

NDIReceiver::NDIReceiver() = default;

NDIReceiver::~NDIReceiver() {
    stop_thread();
    destroy_recv();
}

std::string NDIReceiver::find_source_url(const std::string& name) {
    if (name.empty()) return {};

    NDIlib_find_create_t find_create = {};
    find_create.show_local_sources = true;
    if (!discovery_server_.empty())
        find_create.p_extra_ips = discovery_server_.c_str();

    NDIlib_find_instance_t finder = NDIlib_find_create_v2(&find_create);
    if (!finder) return {};

    std::string url;
    // Wait up to 3 seconds for sources
    for (int i = 0; i < 6; i++) {
        NDIlib_find_wait_for_sources(finder, 500);
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
    }

    NDIlib_find_destroy(finder);
    return url;
}

// Create the recv instance without connecting to any source.
// This registers the device with the NDI discovery server as a receiver,
// making it permanently visible even when not connected to a source.
bool NDIReceiver::create_recv() {
    if (recv_) return true;  // already created
    NDIlib_recv_create_v3_t create = {};
    create.color_format       = NDIlib_recv_color_format_BGRX_BGRA;
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

void NDIReceiver::rename(const std::string& new_name) {
    if (new_name == recv_name_) return;
    recv_name_ = new_name;
    std::cout << "[NDIRecv] Renaming receiver to: " << new_name << "\n";

    // Remember current source so we can reconnect after the swap
    std::string src = current_source_;
    std::string ip  = current_ip_;

    stop_thread();

    // Remove old recv from advertiser (keep advertiser alive)
    if (advertiser_ && recv_)
        NDIlib_recv_advertiser_del_receiver(advertiser_, recv_);

    // Destroy old recv instance
    if (recv_) {
        NDIlib_recv_destroy(recv_);
        recv_ = nullptr;
    }

    // Create fresh recv with new name
    create_recv();

    // Re-register new recv with the existing advertiser
    if (advertiser_ && recv_)
        NDIlib_recv_advertiser_add_receiver(advertiser_, recv_,
                                            /*allow_controlling=*/true,
                                            /*allow_monitoring=*/true,
                                            /*group=*/nullptr);

    // Reconnect to previous source if there was one
    if (!src.empty() && src != "None")
        connect(src, ip);
}

bool NDIReceiver::init_recv() {
    if (!create_recv()) return false;

    // Register with the NDI discovery server as a receiver so this device
    // appears under "receivers" in NDI discovery tools.
    // Only possible if a discovery server is configured.
    if (!discovery_server_.empty() && !advertiser_) {
        NDIlib_recv_advertiser_create_t adv = {};
        adv.p_url_address = discovery_server_.c_str();
        advertiser_ = NDIlib_recv_advertiser_create(&adv);
        if (advertiser_) {
            NDIlib_recv_advertiser_add_receiver(advertiser_, recv_,
                                                /*allow_controlling=*/true,
                                                /*allow_monitoring=*/true,
                                                /*group=*/nullptr);
            std::cout << "[NDIRecv] Registered as receiver with discovery server: "
                      << discovery_server_ << "\n";
        } else {
            std::cerr << "[NDIRecv] WARNING: recv_advertiser_create failed "
                         "(device won't appear as receiver in discovery)\n";
        }
    }

    // Start recv + audio threads immediately so DS can send routing metadata at
    // any time, even before a source is selected (idle/fresh-start scenario).
    std::cout << "[NDIRecv] init_recv complete (advertiser="
              << (advertiser_ ? "active" : "none")
              << ", ds='" << discovery_server_ << "')\n";
    running_ = true;
    recv_thread_ = std::thread(&NDIReceiver::recv_thread, this);
    return true;
}

void NDIReceiver::stop_thread() {
    // Always set running_ = false regardless of current state so that a
    // thread that self-exited (e.g., on disconnect) is properly joined.
    running_ = false;
    if (recv_thread_.joinable()) recv_thread_.join();
}

void NDIReceiver::destroy_recv() {
    // FrameSync must be destroyed before recv
    if (framesync_) {
        NDIlib_framesync_destroy(framesync_);
        framesync_ = nullptr;
    }
    if (advertiser_) {
        if (recv_) NDIlib_recv_advertiser_del_receiver(advertiser_, recv_);
        NDIlib_recv_advertiser_destroy(advertiser_);
        advertiser_ = nullptr;
    }
    if (recv_) {
        NDIlib_recv_destroy(recv_);
        recv_ = nullptr;
    }
}

bool NDIReceiver::connect(const std::string& source_name, const std::string& source_ip) {
    // If currently connected and switching to None, inform the source/DS before disconnect
    if ((source_name.empty() || source_name == "None") && recv_ &&
        NDIlib_recv_get_no_connections(recv_) > 0) {
        static const char none_routing[] =
            "<ndi_routing><source name=\"\" url=\"\"/></ndi_routing>";
        NDIlib_metadata_frame_t none_meta = {};
        none_meta.p_data = const_cast<char*>(none_routing);
        NDIlib_recv_send_metadata(recv_, &none_meta);
        std::cout << "[NDIRecv] Sent routing-to-None to DS\n";
    }
    stop_thread();  // stop polling thread; keep recv_ alive for discovery

    if (source_name.empty() || source_name == "None") {
        current_source_.clear();
        // Stay connected to nothing — device remains visible as a receiver
        if (recv_) NDIlib_recv_connect(recv_, nullptr);
        if (conn_cb_) conn_cb_(false, "");
        // Keep threads running so DS can still send routing metadata
        running_ = true;
    recv_thread_ = std::thread(&NDIReceiver::recv_thread, this);
        return true;
    }

    std::cout << "[NDIRecv] Connect to NDI source: " << source_name << "\n";
    current_source_ = source_name;
    current_ip_     = source_ip;
    stream_type_    = NDIStreamType::Unknown;

    // Destroy old framesync — will be recreated if new source is standard
    if (framesync_) {
        NDIlib_framesync_destroy(framesync_);
        framesync_ = nullptr;
    }

    // Ensure recv instance exists (for discovery registration)
    if (!create_recv()) return false;

    // Resolve source URL — the strings backing the NDIlib_source_t pointers
    // must stay alive until NDIlib_recv_connect returns, so we use locals.
    std::string found_url;
    NDIlib_source_t src = {};
    src.p_ndi_name = source_name.c_str();
    if (!source_ip.empty()) {
        src.p_url_address = source_ip.c_str();
        std::cout << "[NDIRecv] Using IP: " << source_ip << "\n";
    } else {
        std::cout << "[NDIRecv] No IP provided, scanning via NDI Find...\n";
        found_url = find_source_url(source_name);
        if (!found_url.empty()) {
            src.p_url_address = found_url.c_str();
            std::cout << "[NDIRecv] Found URL: " << found_url << "\n";
        } else {
            std::cout << "[NDIRecv] Source not found by scan, connecting by name only\n";
        }
    }

    // Register connection metadata BEFORE connecting so the SDK sends it in
    // the initial handshake rather than racing with async connection setup.
    // Send routing as a standalone frame (single XML root element) so the
    // source/DS can parse it unambiguously — the full connect_metadata_ may
    // contain multiple concatenated XML roots which many parsers reject.
    NDIlib_recv_clear_connection_metadata(recv_);
    std::string routing_xml = "<ndi_routing><source name=\"" + source_name
                            + "\" url=\"" + source_ip + "\"/></ndi_routing>";
    {
        NDIlib_metadata_frame_t meta = {};
        meta.p_data = const_cast<char*>(routing_xml.c_str());
        NDIlib_recv_add_connection_metadata(recv_, &meta);
    }
    if (!connect_metadata_.empty()) {
        NDIlib_metadata_frame_t meta = {};
        meta.p_data = const_cast<char*>(connect_metadata_.c_str());
        NDIlib_recv_add_connection_metadata(recv_, &meta);
    }

    NDIlib_recv_connect(recv_, &src);

    running_ = true;
    recv_thread_ = std::thread(&NDIReceiver::recv_thread, this);
    return true;
}

void NDIReceiver::disconnect() {
    // Inform source/DS that this receiver is no longer routing to any source
    if (recv_ && NDIlib_recv_get_no_connections(recv_) > 0) {
        static const char none_routing[] =
            "<ndi_routing><source name=\"\" url=\"\"/></ndi_routing>";
        NDIlib_metadata_frame_t none_meta = {};
        none_meta.p_data = const_cast<char*>(none_routing);
        NDIlib_recv_send_metadata(recv_, &none_meta);
        std::cout << "[NDIRecv] Sent routing-to-None to DS\n";
    }
    stop_thread();
    // Keep recv_ alive — device stays visible as a receiver in discovery
    if (recv_) NDIlib_recv_connect(recv_, nullptr);
    current_source_.clear();
    // Keep threads running so DS can still send routing metadata at any time
    running_ = true;
    recv_thread_ = std::thread(&NDIReceiver::recv_thread, this);
}

void NDIReceiver::reload_discovery(const std::string& new_ds) {
    if (new_ds == discovery_server_) return;
    discovery_server_ = new_ds;
    std::cout << "[NDIRecv] Reloading discovery server: '" << new_ds << "'\n";

    // Tear down old advertiser
    if (advertiser_) {
        if (recv_) NDIlib_recv_advertiser_del_receiver(advertiser_, recv_);
        NDIlib_recv_advertiser_destroy(advertiser_);
        advertiser_ = nullptr;
    }

    // Create new advertiser with updated DS address (video connection is unaffected)
    if (!new_ds.empty() && recv_) {
        NDIlib_recv_advertiser_create_t adv = {};
        adv.p_url_address = new_ds.c_str();
        advertiser_ = NDIlib_recv_advertiser_create(&adv);
        if (advertiser_) {
            NDIlib_recv_advertiser_add_receiver(advertiser_, recv_,
                                                /*allow_controlling=*/true,
                                                /*allow_monitoring=*/true,
                                                /*group=*/nullptr);
            std::cout << "[NDIRecv] Re-registered with new discovery server: " << new_ds << "\n";
        } else {
            std::cerr << "[NDIRecv] WARNING: recv_advertiser_create failed for " << new_ds << "\n";
        }
    }
}

bool NDIReceiver::is_connected() const {
    if (!recv_) return false;
    return NDIlib_recv_get_no_connections(recv_) > 0;
}

void NDIReceiver::set_transport(const std::string& rxpm) {
    if (rxpm == transport_mode_) return;
    transport_mode_ = rxpm;
    std::cout << "[NDIRecv] Transport mode set to: " << rxpm << "\n";

    // NDI transport is negotiated by the SDK based on ndi-config.v1.json settings.
    // RUDP is the default in NDI 5+; disabling it forces TCP/UDP fallback.
    // Multicast is sender-controlled but we can express a preference via metadata.
    // The config file is re-read when a new recv instance is created, so a
    // reconnect (rename → destroy/create recv) applies the change.
    // The actual config write is done by write_ndi_sdk_config() in main.cpp
    // when reload_config is called — we just store the preference here.
}

void NDIReceiver::set_tally(bool on_program, bool on_preview) {
    if (!recv_) return;
    NDIlib_tally_t tally = {};
    tally.on_program = on_program;
    tally.on_preview = on_preview;
    NDIlib_recv_set_tally(recv_, &tally);
}

void NDIReceiver::free_video(NDIVideoFrame& f) {
    if (recv_ && f.ndi_frame) {
        NDIlib_recv_free_video_v2(recv_, f.ndi_frame);
        f.ndi_frame = nullptr;
    }
}

void NDIReceiver::free_video(NDIlib_video_frame_v2_t* frame) {
    if (recv_ && frame)
        NDIlib_recv_free_video_v2(recv_, frame);
}

void NDIReceiver::free_audio(NDIAudioFrame& f) {
    if (recv_ && f.ndi_frame) {
        NDIlib_recv_free_audio_v3(recv_, f.ndi_frame);
        f.ndi_frame = nullptr;
    }
}

void NDIReceiver::get_performance(int& total_frames, int& dropped_frames) {
    if (!recv_) { total_frames = dropped_frames = 0; return; }
    NDIlib_recv_performance_t total = {}, dropped = {};
    NDIlib_recv_get_performance(recv_, &total, &dropped);
    total_frames   = (int)total.video_frames;
    dropped_frames = (int)dropped.video_frames;

    // Monitor queue depth; log if video frames are backing up
    NDIlib_recv_queue_t queue = {};
    NDIlib_recv_get_queue(recv_, &queue);
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
        vf.stride = raw.line_stride_in_bytes;
        vf.size   = raw.line_stride_in_bytes * raw.yres;
    }
    return vf;
}

void NDIReceiver::recv_thread() {
    std::cout << "[NDIRecv] START recv thread\n";

    bool was_connected = false;
    auto probe_start   = std::chrono::steady_clock::now();
    bool probe_active  = !current_source_.empty() && current_source_ != "None";
    auto last_meta_send = std::chrono::steady_clock::now();

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
        if (connections > 0 && !was_connected) {
            was_connected = true;
            probe_active  = false;
            first_frame_logged_ = false;
            stream_type_ = NDIStreamType::Unknown;
            // Destroy old framesync — new source may be different type
            if (framesync_) {
                NDIlib_framesync_destroy(framesync_);
                framesync_ = nullptr;
            }
            std::cout << "[NDIRecv] Connected to " << current_source_ << "\n";
            if (conn_cb_) conn_cb_(true, current_source_);
            if (!current_source_.empty() && current_source_ != "None") {
                std::string routing = "<ndi_routing><source name=\"" + current_source_
                                    + "\" url=\"" + current_ip_ + "\"/></ndi_routing>";
                NDIlib_metadata_frame_t meta = {};
                meta.p_data = const_cast<char*>(routing.c_str());
                NDIlib_recv_send_metadata(recv_, &meta);
                std::cout << "[NDIRecv] Routing ACK sent on connect\n";
            }
            last_meta_send = std::chrono::steady_clock::now();
        } else if (connections == 0 && was_connected) {
            was_connected = false;
            probe_active  = !current_source_.empty() && current_source_ != "None";
            probe_start   = std::chrono::steady_clock::now();
            if (framesync_) {
                NDIlib_framesync_destroy(framesync_);
                framesync_ = nullptr;
            }
            stream_type_ = NDIStreamType::Unknown;
            if (running_) {
                std::cout << "[NDIRecv] Disconnected from " << current_source_ << "\n";
                if (conn_cb_) conn_cb_(false, current_source_);
                if (recv_) NDIlib_recv_connect(recv_, nullptr);
            }
        } else if (!was_connected && probe_active && running_) {
            using clock = std::chrono::steady_clock;
            auto elapsed = std::chrono::duration_cast<std::chrono::seconds>(
                clock::now() - probe_start).count();
            if (elapsed >= 5) {
                std::cout << "[NDIRecv] Source unavailable: " << current_source_ << "\n";
                if (conn_cb_) conn_cb_(false, current_source_);
                probe_start = clock::now();
            }
        }

        // ---- Periodic routing ACK ----
        if (was_connected && running_ &&
            !current_source_.empty() && current_source_ != "None") {
            using clock = std::chrono::steady_clock;
            auto elapsed = std::chrono::duration_cast<std::chrono::seconds>(
                clock::now() - last_meta_send).count();
            if (elapsed >= 30) {
                // Routing ACK
                std::string routing = "<ndi_routing><source name=\"" + current_source_
                                    + "\" url=\"" + current_ip_ + "\"/></ndi_routing>";
                NDIlib_metadata_frame_t meta = {};
                meta.p_data = const_cast<char*>(routing.c_str());
                NDIlib_recv_send_metadata(recv_, &meta);

                // Device status telemetry for DS
                if (status_meta_cb_) {
                    std::string status_xml = status_meta_cb_();
                    if (!status_xml.empty()) {
                        NDIlib_metadata_frame_t smeta = {};
                        smeta.p_data = const_cast<char*>(status_xml.c_str());
                        NDIlib_recv_send_metadata(recv_, &smeta);
                    }
                }

                last_meta_send = clock::now();
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
                video_cb_(vf);
            }
            NDIlib_framesync_free_video(framesync_, &video_frame);

            // Pull audio at native channel count (48kHz, 1024 samples = ~21ms)
            // Requesting 0 channels = native format; downmix to stereo in ALSA sink
            if (audio_enabled_ && audio_cb_) {
                NDIlib_audio_frame_v2_t audio_frame{};
                NDIlib_framesync_capture_audio(framesync_, &audio_frame,
                                               48000, 0, 1024);
                if (audio_frame.p_data) {
                    NDIAudioFrame af;
                    af.sample_rate    = audio_frame.sample_rate;
                    af.channels       = audio_frame.no_channels;
                    af.num_samples    = audio_frame.no_samples;
                    af.channel_stride = audio_frame.channel_stride_in_bytes;
                    af.data           = audio_frame.p_data;
                    af.ndi_frame      = nullptr;  // framesync frames use separate free
                    audio_cb_(af);
                }
                NDIlib_framesync_free_audio(framesync_, &audio_frame);
            }

            // FrameSync doesn't deliver metadata — poll recv for metadata/status
            NDIlib_metadata_frame_t metadata{};
            NDIlib_frame_type_e mt = NDIlib_recv_capture_v3(
                recv_, nullptr, nullptr, &metadata, 0);
            if (mt == NDIlib_frame_type_metadata && metadata.p_data) {
                std::string xml(metadata.p_data);
                if (xml.find("ndi_routing") != std::string::npos) {
                    std::string name = xml_attr(xml, "name");
                    std::string url  = xml_attr(xml, "url");
                    std::cout << "[NDIRecv] DS routing: source='"
                              << name << "' url='" << url << "'\n";
                    current_source_ = (name.empty() || name == "None") ? "" : name;
                    current_ip_     = url;
                    if (routing_cb_) routing_cb_(name, url);
                } else {
                    std::cout << "[NDIRecv] Metadata: " << xml.substr(0, 120) << "\n";
                }
                NDIlib_recv_free_metadata(recv_, &metadata);
            } else if (mt == NDIlib_frame_type_source_change) {
                const char* p_src = nullptr;
                NDIlib_recv_get_source_name(recv_, &p_src, 0);
                std::string new_src = p_src ? p_src : "";
                if (p_src) NDIlib_recv_free_string(recv_, p_src);
                if (!new_src.empty()) current_source_ = new_src;
            }

            // Small sleep to pace the pull loop (~60Hz)
            std::this_thread::sleep_for(std::chrono::milliseconds(8));

        } else {
            // ---- HX MODE / DETECTION: capture_v3 push ----
            NDIlib_video_frame_v2_t video_frame{};
            NDIlib_audio_frame_v3_t audio_frame_storage{};
            NDIlib_audio_frame_v3_t* audio_ptr = audio_enabled_ ? &audio_frame_storage : nullptr;
            NDIlib_metadata_frame_t metadata{};

            NDIlib_frame_type_e frame_type = NDIlib_recv_capture_v3(
                recv_, &video_frame, audio_ptr, &metadata, 50);

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
                            framesync_ = NDIlib_framesync_create(recv_);
                            if (framesync_) {
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
                        video_cb_(vf);
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
                    if (metadata.p_data) {
                        std::string xml(metadata.p_data);
                        if (xml.find("ndi_routing") != std::string::npos) {
                            std::string name = xml_attr(xml, "name");
                            std::string url  = xml_attr(xml, "url");
                            std::cout << "[NDIRecv] DS routing: source='"
                                      << name << "' url='" << url << "'\n";
                            current_source_ = (name.empty() || name == "None") ? "" : name;
                            current_ip_     = url;
                            if (routing_cb_) routing_cb_(name, url);
                        } else {
                            std::cout << "[NDIRecv] Metadata: " << xml.substr(0, 120) << "\n";
                        }
                    }
                    NDIlib_recv_free_metadata(recv_, &metadata);
                    break;
                }

                case NDIlib_frame_type_status_change:
                    break;

                case NDIlib_frame_type_source_change: {
                    const char* p_src = nullptr;
                    NDIlib_recv_get_source_name(recv_, &p_src, 0);
                    std::string new_src = p_src ? p_src : "";
                    if (p_src) NDIlib_recv_free_string(recv_, p_src);
                    std::cout << "[NDIRecv] Source change: '" << new_src << "'\n";
                    if (!new_src.empty()) current_source_ = new_src;
                    break;
                }

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
