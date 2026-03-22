#include "VideoDecoder.h"
#include "PlatformDetect.h"
#include <iostream>

#ifdef HAVE_MPP
#include "MppDecoder.h"
#endif

#ifdef HAVE_V4L2
#include "V4L2Decoder.h"
#endif

#ifdef HAVE_FFMPEG
#include "SoftwareDecoder.h"
#endif

std::unique_ptr<VideoDecoder> VideoDecoder::create() {
    Platform p = PlatformDetect::detect();
    std::cout << "[VideoDecoder] Platform: " << PlatformDetect::name() << "\n";

#ifdef HAVE_MPP
    if (p == Platform::Rockchip) {
        std::cout << "[VideoDecoder] Using Rockchip MPP hardware decoder\n";
        return std::make_unique<MppDecoder>();
    }
#endif

#ifdef HAVE_V4L2
    if (p == Platform::RaspberryPi || p == Platform::Rockchip) {
        std::cout << "[VideoDecoder] Using V4L2 M2M hardware decoder\n";
        return std::make_unique<V4L2Decoder>();
    }
#endif

#ifdef HAVE_FFMPEG
    std::cout << "[VideoDecoder] Using software decoder (FFmpeg)\n";
    return std::make_unique<SoftwareDecoder>();
#endif

    std::cerr << "[VideoDecoder] No decoder available for this platform!\n";
    return nullptr;
}
