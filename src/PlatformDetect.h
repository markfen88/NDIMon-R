#pragma once
#include <string>

enum class Platform { Rockchip, RaspberryPi, Generic };

class PlatformDetect {
public:
    static Platform detect();
    static std::string name();
};
