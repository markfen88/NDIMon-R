#include "PlatformDetect.h"
#include <fstream>
#include <sstream>
#include <cstdio>

Platform PlatformDetect::detect() {
    // Check /proc/device-tree/compatible (most reliable on embedded Linux)
    {
        std::ifstream f("/proc/device-tree/compatible");
        if (f.is_open()) {
            std::ostringstream ss;
            ss << f.rdbuf();
            std::string content = ss.str();
            if (content.find("rockchip") != std::string::npos)
                return Platform::Rockchip;
            if (content.find("raspberrypi") != std::string::npos ||
                content.find("brcm,bcm2") != std::string::npos)
                return Platform::RaspberryPi;
        }
    }

    // Fallback: check for Rockchip MPP library
    {
        std::ifstream mpp("/usr/lib/aarch64-linux-gnu/librockchip_mpp.so.1");
        if (mpp.is_open()) return Platform::Rockchip;
    }
    {
        std::ifstream mpp("/usr/lib/arm-linux-gnueabihf/librockchip_mpp.so.1");
        if (mpp.is_open()) return Platform::Rockchip;
    }

    // Fallback: check for Raspberry Pi V4L2 codec device
    {
        std::ifstream v4l("/dev/video10");
        if (v4l.is_open()) return Platform::RaspberryPi;
    }

    return Platform::Generic;
}

std::string PlatformDetect::name() {
    switch (detect()) {
        case Platform::Rockchip:    return "Rockchip";
        case Platform::RaspberryPi: return "Raspberry Pi";
        default:                    return "Generic";
    }
}
