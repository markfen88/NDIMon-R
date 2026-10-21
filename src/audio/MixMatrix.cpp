#include "MixMatrix.h"
#include <cmath>
namespace ndimon::audio {
float db_to_gain(float db) { return db <= -60 ? 0 : std::pow(10.0f, db / 20); }
void mix(const std::vector<MixInput> &inputs, Block &out, bool clamp_output) {
    out.silence();
    for (const auto &input : inputs) {
        const auto &b = *input.block;
        for (int c = 0; c < out.channels; ++c)
            for (int n = 0; n < out.frames; ++n) {
                float x = 0;
                if (n >= b.frames || b.channels == 0)
                    continue;
                if (b.channels >= 6 && out.channels == 2)
                    x = b.plane[c][n] + .707f * (b.plane[2][n] + b.plane[4 + c][n]) +
                        .5f * b.plane[3][n];
                else if (b.channels == 1)
                    x = b.plane[0][n];
                else if (c < b.channels)
                    x = b.plane[c][n];
                float t = std::min(1.f, float(input.fade_offset + n) / 480);
                float g = input.start_gain < 0
                              ? input.gain
                              : input.start_gain + (input.gain - input.start_gain) * t;
                out.plane[c][n] += g * x;
            }
    }
    if (clamp_output)
        for (int c = 0; c < out.channels; ++c)
            for (int n = 0; n < out.frames; ++n)
                out.plane[c][n] = std::clamp(out.plane[c][n], -1.0f, 1.0f);
}
} // namespace ndimon::audio
