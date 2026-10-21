#pragma once
#include "AudioTypes.h"
#include <vector>
namespace ndimon::audio {
struct MixInput {
    const Block *block;
    float gain;
    float start_gain = -1;
    int fade_offset = 480;
};
void mix(const std::vector<MixInput> &inputs, Block &out, bool clamp_output = true);
float db_to_gain(float db);
} // namespace ndimon::audio
