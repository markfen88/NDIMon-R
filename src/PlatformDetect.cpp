#include "PlatformDetect.h"
#include <fstream>
#include <sstream>
#include <cstdio>
#include <dirent.h>
#include <cstring>

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

bool PlatformDetect::is_x86() {
#if defined(__x86_64__) || defined(_M_X64) || defined(__amd64__)
    return true;
#else
    return false;
#endif
}

std::string PlatformDetect::cpu_vendor() {
    if (!is_x86()) return "";
    std::ifstream f("/proc/cpuinfo");
    std::string line;
    while (std::getline(f, line)) {
        if (line.rfind("vendor_id", 0) == 0) {
            if (line.find("GenuineIntel") != std::string::npos) return "Intel";
            if (line.find("AuthenticAMD") != std::string::npos) return "AMD";
            return "x86";
        }
    }
    return "x86";
}

bool PlatformDetect::has_render_node() {
    DIR* d = opendir("/dev/dri");
    if (!d) return false;
    bool found = false;
    struct dirent* e;
    while ((e = readdir(d)) != nullptr) {
        if (strncmp(e->d_name, "renderD", 7) == 0) { found = true; break; }
    }
    closedir(d);
    return found;
}

std::string PlatformDetect::name() {
    switch (detect()) {
        case Platform::Rockchip:    return "Rockchip";
        case Platform::RaspberryPi: return "Raspberry Pi";
        default: break;
    }
    if (is_x86()) {
        std::string v = cpu_vendor();
        return v.empty() ? "x86-64" : (v + " x86-64");
    }
    return "Generic";
}
