#include "VideoDecoder.h"
#include "PlatformDetect.h"
#include "Config.h"
#include <iostream>

#ifdef HAVE_MPP
#include "MppDecoder.h"
#endif

#ifdef HAVE_V4L2
#include "V4L2Decoder.h"
#endif

#ifdef HAVE_VAAPI
#include "VAAPIDecoder.h"
#endif

#ifdef HAVE_FFMPEG
#include "SoftwareDecoder.h"
#endif

std::unique_ptr<VideoDecoder> VideoDecoder::create() {
    Platform p = PlatformDetect::detect();
    // Device-level decode preference: auto | hardware | software.
    std::string mode = Config::instance().device.decode_mode;
    if (mode != "hardware" && mode != "software") mode = "auto";
    const bool allow_hw = (mode != "software");

    std::cout << "[VideoDecoder] Platform: " << PlatformDetect::name()
              << "  decode_mode=" << mode << "\n";

#ifdef HAVE_MPP
    if (allow_hw && p == Platform::Rockchip) {
        std::cout << "[VideoDecoder] Using Rockchip MPP hardware decoder\n";
        return std::make_unique<MppDecoder>();
    }
#endif

#ifdef HAVE_V4L2
    if (allow_hw && (p == Platform::RaspberryPi)) {
        std::cout << "[VideoDecoder] Using V4L2 M2M hardware decoder\n";
        return std::make_unique<V4L2Decoder>();
    }
#endif

#ifdef HAVE_VAAPI
    if (allow_hw && PlatformDetect::is_x86() && PlatformDetect::has_render_node()) {
        std::cout << "[VideoDecoder] Using VAAPI hardware decoder ("
                  << PlatformDetect::cpu_vendor() << ")\n";
        return std::make_unique<VAAPIDecoder>();
    }
#endif

    // Best-effort: if hardware was explicitly requested but none is available,
    // we still fall back to software so the appliance keeps working — the status
    // backend ("software") makes the fallback visible in the UI.
    if (mode == "hardware")
        std::cerr << "[VideoDecoder] decode_mode=hardware but no hardware decoder "
                     "available — falling back to software\n";

#ifdef HAVE_FFMPEG
    {
        std::cout << "[VideoDecoder] Using software decoder (FFmpeg)\n";
        auto sw = std::make_unique<SoftwareDecoder>();
        // On x86, software decode is a primary path (the NDI SDK has no GPU
        // decode on Linux) and 4K needs the cores → throughput threading.
        // On ARM the software path is a rare fallback → keep low-latency.
        // The decode_low_latency tuning flag forces single-thread/low-delay
        // everywhere (trades 4K throughput for minimum latency).
        bool low_latency = !PlatformDetect::is_x86() ||
                           Config::instance().tuning.decode_low_latency;
        sw->set_low_latency(low_latency);
        return sw;
    }
#endif

    std::cerr << "[VideoDecoder] No decoder available for this platform!\n";
    return nullptr;
}
