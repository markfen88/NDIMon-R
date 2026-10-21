// Splash backdrop setting shared by Config and the renderer.
#pragma once

#include <string>

namespace ndimon {

enum class Backdrop { Gradient, Flat };

// Exact match only, mirroring the API's validation; anything else is the default.
inline Backdrop backdrop_from_string(const std::string& s) {
    return s == "flat" ? Backdrop::Flat : Backdrop::Gradient;
}

} // namespace ndimon
