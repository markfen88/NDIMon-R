#pragma once
#include <string>

enum class Platform { Rockchip, RaspberryPi, Generic };

class PlatformDetect {
public:
    static Platform detect();
    static std::string name();

    // True on x86-64 hosts (build-time arch).
    static bool is_x86();

    // CPU vendor string for x86: "Intel", "AMD", or "" on ARM/unknown.
    static std::string cpu_vendor();

    // True if a usable DRM render node exists (/dev/dri/renderD*), the
    // prerequisite for VAAPI hardware decode. Cheap probe; the VAAPIDecoder
    // does the authoritative capability check at init.
    static bool has_render_node();
};
