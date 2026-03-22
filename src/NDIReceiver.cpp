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

NDIlib_source_t NDIReceiver::find_source(const std::string& name, const std::string& ip) {
    NDIlib_source_t src = {};
    if (name.empty()) return src;

    // If an IP is directly provided, use it
    if (!ip.empty()) {
        src.p_ndi_name = name.c_str();
        src.p_url_address = ip.c_str();
        return src;
    }

    // Use NDI Find to locate the source
    NDIlib_find_create_t find_create = {};
    find_create.show_local_sources = true;
    // Pass discovery server so finder can reach sources on the discovery server
    if (!discovery_server_.empty())
        find_create.p_extra_ips = discovery_server_.c_str();

    NDIlib_find_instance_t finder = NDIlib_find_create_v2(&find_create);
    if (!finder) return src;

    // Wait up to 3 seconds for sources
    for (int i = 0; i < 6; i++) {
        NDIlib_find_wait_for_sources(finder, 500);
        uint32_t count = 0;
        const NDIlib_source_t* sources = NDIlib_find_get_current_sources(finder, &count);
        for (uint32_t j = 0; j < count; j++) {
            if (sources[j].p_ndi_name && name == sources[j].p_ndi_name) {
                src = sources[j];
                break;
            }
        }
        if (src.p_ndi_name) break;
    }

    NDIlib_find_destroy(finder);
    return src;
}

// Create the recv instance without connecting to any source.
// This registers the device with the NDI discovery server as a receiver,
// making it permanently visible even when not connected to a source.
bool NDIReceiver::create_recv() {
    if (recv_) return true;  // already created
    NDIlib_recv_create_v3_t create = {};
    create.color_format       = NDIlib_recv_color_format_fastest;
    create.bandwidth          = NDIlib_recv_bandwidth_highest;
    create.allow_video_fields = false;
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

    // Ensure recv instance exists (for discovery registration)
    if (!create_recv()) return false;

    NDIlib_source_t src = {};
    if (!source_ip.empty()) {
        src.p_ndi_name    = source_name.c_str();
        src.p_url_address = source_ip.c_str();
        std::cout << "[NDIRecv] Using IP: " << source_ip << "\n";
    } else {
        std::cout << "[NDIRecv] No IP provided, scanning via NDI Find...\n";
        src = find_source(source_name, "");
        if (!src.p_ndi_name) {
            std::cout << "[NDIRecv] Source not found by scan, connecting by name only\n";
            src.p_ndi_name = source_name.c_str();
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

bool NDIReceiver::is_connected() const {
    if (!recv_) return false;
    return NDIlib_recv_get_no_connections(recv_) > 0;
}

void NDIReceiver::set_transport(const std::string& rxpm) {
    transport_mode_ = rxpm;
    // TODO: reconnect with new transport settings if connected
    // NDI transport is controlled via recv create settings / metadata
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

void NDIReceiver::recv_thread() {
    std::cout << "[NDIRecv] START NDIRecvVideoHX() Thread\n";

    bool was_connected = false;
    auto probe_start   = std::chrono::steady_clock::now();
    bool probe_active  = !current_source_.empty() && current_source_ != "None";
    auto last_meta_send = std::chrono::steady_clock::now();

    try {
    while (running_) {
        if (!recv_) {
            std::this_thread::sleep_for(std::chrono::milliseconds(100));
            continue;
        }

        // Check connection status
        int connections = NDIlib_recv_get_no_connections(recv_);
        if (connections > 0 && !was_connected) {
            was_connected = true;
            probe_active  = false;  // connected — disable probe
            std::cout << "[NDIRecv] Connected to " << current_source_ << "\n";
            if (conn_cb_) conn_cb_(true, current_source_);
            // Send routing ACK immediately so DS sees the assignment right away.
            // NDIlib_recv_add_connection_metadata fires on the handshake, but
            // sending again here covers any async-connect race and ensures the
            // DS is updated the moment the connection is confirmed alive.
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
            // Arm probe for the reconnect phase
            probe_active  = !current_source_.empty() && current_source_ != "None";
            probe_start   = std::chrono::steady_clock::now();
            // Guard: if running_ was set to false by stop_thread(), this is an
            // intentional reconnect — don't fire a spurious disconnect callback.
            if (running_) {
                std::cout << "[NDIRecv] Disconnected from " << current_source_ << "\n";
                if (conn_cb_) conn_cb_(false, current_source_);
                // Cancel the SDK's internal reconnect by pointing it at nothing.
                if (recv_) NDIlib_recv_connect(recv_, nullptr);
            }
            // Keep running — DS needs the recv thread alive to send routing metadata.
            // Node.js (via IPC events) handles reconnect scheduling.
        } else if (!was_connected && probe_active && running_) {
            // Source configured but never connected — probe timeout
            using clock = std::chrono::steady_clock;
            auto elapsed = std::chrono::duration_cast<std::chrono::seconds>(
                clock::now() - probe_start).count();
            if (elapsed >= 5) {
                std::cout << "[NDIRecv] Source unavailable: " << current_source_ << "\n";
                if (conn_cb_) conn_cb_(false, current_source_);
                probe_start = clock::now();  // re-arm: fires again if still unavailable
            }
        }

        // Periodic routing ACK resend: keeps DS in sync after DS restarts.
        // Use routing-only XML (single root element) — parsers reliably handle it.
        if (was_connected && running_ &&
            !current_source_.empty() && current_source_ != "None") {
            using clock = std::chrono::steady_clock;
            auto elapsed = std::chrono::duration_cast<std::chrono::seconds>(
                clock::now() - last_meta_send).count();
            if (elapsed >= 30) {
                std::string routing = "<ndi_routing><source name=\"" + current_source_
                                    + "\" url=\"" + current_ip_ + "\"/></ndi_routing>";
                NDIlib_metadata_frame_t meta = {};
                meta.p_data = const_cast<char*>(routing.c_str());
                NDIlib_recv_send_metadata(recv_, &meta);
                last_meta_send = clock::now();
                std::cout << "[NDIRecv] Routing ACK refreshed for DS\n";
            }
        }

        NDIlib_video_frame_v2_t* video_frame = new NDIlib_video_frame_v2_t();
        NDIlib_audio_frame_v3_t* audio_frame = nullptr;
        if (audio_enabled_) {
            audio_frame = new NDIlib_audio_frame_v3_t();
        }
        NDIlib_metadata_frame_t metadata = {};

        NDIlib_frame_type_e frame_type = NDIlib_recv_capture_v3(
            recv_,
            video_frame,
            audio_frame,
            &metadata,
            50  // 50ms timeout
        );

        switch (frame_type) {
            case NDIlib_frame_type_video: {
                // Warn on HDR FourCC: color_format_fastest does not down-map HDR to SDR.
                if (video_frame->FourCC == NDIlib_FourCC_video_type_P216 ||
                    video_frame->FourCC == NDIlib_FourCC_video_type_PA16) {
                    std::cerr << "[NDIRecv] WARNING: HDR source detected (FourCC="
                              << (video_frame->FourCC == NDIlib_FourCC_video_type_P216
                                  ? "P216" : "PA16")
                              << "). color_format_fastest does not down-map HDR to SDR"
                                 " — display will appear black. Use color_format_best"
                                 " for HDR support.\n";
                }

                if (video_cb_) {
                    NDIVideoFrame vf;
                    vf.width      = video_frame->xres;
                    vf.height     = video_frame->yres;
                    vf.frame_rate_n = video_frame->frame_rate_N;
                    vf.frame_rate_d = video_frame->frame_rate_D;
                    vf.fourcc     = video_frame->FourCC;
                    vf.data       = video_frame->p_data;
                    vf.timestamp  = video_frame->timestamp;
                    vf.frame_format = video_frame->frame_format_type;
                    vf.ndi_frame  = video_frame;

                    // Compressed type: data_size_in_bytes is valid
                    // Uncompressed: line_stride_in_bytes is valid
                    bool is_compressed = (
                        video_frame->FourCC == NDI_LIB_FOURCC('H','2','6','4') ||
                        video_frame->FourCC == NDI_LIB_FOURCC('H','2','6','5') ||
                        video_frame->FourCC == NDI_LIB_FOURCC('h','2','6','4') ||
                        video_frame->FourCC == NDI_LIB_FOURCC('h','2','6','5') ||
                        video_frame->FourCC == NDI_LIB_FOURCC('H','E','V','C') ||
                        video_frame->FourCC == NDI_LIB_FOURCC('A','V','C','1')
                    );

                    if (is_compressed) {
                        vf.size   = video_frame->data_size_in_bytes;
                        vf.stride = 0;
                    } else {
                        vf.stride = video_frame->line_stride_in_bytes;
                        vf.size   = video_frame->line_stride_in_bytes * video_frame->yres;
                    }

                    video_cb_(vf);
                    // Caller must call free_video() after use
                    // If callback didn't take ownership, free now
                    if (vf.ndi_frame) {
                        NDIlib_recv_free_video_v2(recv_, video_frame);
                        delete video_frame;
                        video_frame = nullptr;
                    }
                } else {
                    NDIlib_recv_free_video_v2(recv_, video_frame);
                    delete video_frame;
                    video_frame = nullptr;
                }
                if (audio_frame) { delete audio_frame; audio_frame = nullptr; }
                break;
            }

            case NDIlib_frame_type_audio: {
                if (audio_frame && audio_cb_) {
                    NDIAudioFrame af;
                    af.sample_rate     = audio_frame->sample_rate;
                    af.channels        = audio_frame->no_channels;
                    af.num_samples     = audio_frame->no_samples;
                    af.channel_stride  = audio_frame->channel_stride_in_bytes;
                    af.channel_data    = (float**)&audio_frame->p_data;
                    af.ndi_frame       = audio_frame;
                    audio_cb_(af);
                    if (af.ndi_frame) {
                        NDIlib_recv_free_audio_v3(recv_, audio_frame);
                        delete audio_frame;
                        audio_frame = nullptr;
                    }
                } else if (audio_frame) {
                    NDIlib_recv_free_audio_v3(recv_, audio_frame);
                    delete audio_frame;
                    audio_frame = nullptr;
                }
                if (video_frame) { delete video_frame; video_frame = nullptr; }
                break;
            }

            case NDIlib_frame_type_metadata: {
                if (metadata.p_data) {
                    std::string xml(metadata.p_data);
                    // Routing command from discovery server: assign a source to this receiver.
                    // Format: <ndi_routing><source name="..." url="..."/></ndi_routing>
                    // With allow_controlling=true the SDK handles the actual connection switch.
                    // routing_cb_ is purely informational (pushes IPC event for UI display).
                    // Do NOT call connect_source() from here — that causes a feedback loop:
                    //   DS routing → connect → routing ACK → DS routing → connect → crash (SEGV).
                    if (xml.find("ndi_routing") != std::string::npos) {
                        std::string name = xml_attr(xml, "name");
                        std::string url  = xml_attr(xml, "url");
                        std::cout << "[NDIRecv] DS routing: source='"
                                  << name << "' url='" << url << "'\n";
                        // Update internal state so status queries reflect DS assignment
                        current_source_ = (name.empty() || name == "None") ? "" : name;
                        current_ip_     = url;
                        // Notify Node.js for UI display (informational only — no reconnect)
                        if (routing_cb_) routing_cb_(name, url);
                    } else {
                        // Log unknown metadata for debugging
                        std::cout << "[NDIRecv] Metadata: " << xml.substr(0, 120) << "\n";
                    }
                }
                NDIlib_recv_free_metadata(recv_, &metadata);
                if (video_frame) { delete video_frame; video_frame = nullptr; }
                if (audio_frame) { delete audio_frame; audio_frame = nullptr; }
                break;
            }

            case NDIlib_frame_type_status_change:
                if (video_frame) { delete video_frame; video_frame = nullptr; }
                if (audio_frame) { delete audio_frame; audio_frame = nullptr; }
                break;

            case NDIlib_frame_type_source_change: {
                // SDK notifies us that the connected source changed — either the DS
                // reassigned us (allow_controlling=true) or the source renamed itself.
                // Just sync internal state; do NOT fire routing_cb_ — that would trigger
                // a connect_source() call which races with the SDK's own switch and
                // creates a stop_thread()/join() crash loop.
                const char* p_src = nullptr;
                NDIlib_recv_get_source_name(recv_, &p_src, 0);
                std::string new_src = p_src ? p_src : "";
                if (p_src) NDIlib_recv_free_string(recv_, p_src);
                std::cout << "[NDIRecv] Source change: '" << new_src << "'\n";
                if (!new_src.empty()) current_source_ = new_src;
                if (video_frame) { delete video_frame; video_frame = nullptr; }
                if (audio_frame) { delete audio_frame; audio_frame = nullptr; }
                break;
            }

            case NDIlib_frame_type_error:
                std::cerr << "[NDIRecv] NDIRecvVideoHX frame error\n";
                if (video_frame) { delete video_frame; video_frame = nullptr; }
                if (audio_frame) { delete audio_frame; audio_frame = nullptr; }
                std::this_thread::sleep_for(std::chrono::milliseconds(100));
                break;

            case NDIlib_frame_type_none:
            default:
                if (video_frame) { delete video_frame; video_frame = nullptr; }
                if (audio_frame) { delete audio_frame; audio_frame = nullptr; }
                break;
        }
    } // while (running_)
    } catch (const std::exception& e) {
      std::cerr << "[NDIRecv] Thread exception: " << e.what() << "\n";
  } catch (...) {
      std::cerr << "[NDIRecv] Thread unknown exception\n";
  }

    std::cout << "[NDIRecv] STOPPED NDIRecvRunningHX Thread\n";
}
