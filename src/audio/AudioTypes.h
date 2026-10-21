#pragma once
#include <algorithm>
#include <cstdint>
namespace ndimon::audio {
constexpr int kMaxChannels = 16;
constexpr int kRate = 48000;
struct Block {
    int channels = 0, frames = 0;
    float *plane[kMaxChannels] = {};
    void silence() {
        for (int c = 0; c < channels; ++c)
            std::fill_n(plane[c], frames, 0.0f);
    }
};
struct Counters {
    uint64_t frames = 0;
    uint32_t underruns = 0, overruns = 0, xruns = 0;
};
} // namespace ndimon::audio
