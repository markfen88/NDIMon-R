#pragma once

#include <cstdint>

// The renderer currently produces linear buffers on a fixed landscape canvas.
// Driver enum bits describe hardware features, not format-safe render paths.
inline uint32_t safe_rotation_value(uint32_t degrees, uint64_t advertised,
                                    bool unsafe_linear_plane) {
    if (degrees == 0) return 1;
    // Quarter turns require a swapped logical canvas throughout the renderer.
    if (degrees != 180 || unsafe_linear_plane) return 0;
    const uint32_t value = (advertised & 4) ? 4 : 0x31;
    return (advertised & value) == value ? value : 0;
}
