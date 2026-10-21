#include "DelayLine.h"
namespace ndimon::audio {
DelayLine::DelayLine(int channels, int maximum)
    : channels_(channels), size_(maximum + 1), ring_(channels * size_, 0) {}
void DelayLine::set_delay_frames(int frames) {
    frames = std::clamp(frames, 0, size_ - 1);
    if (frames == delay_)
        return;
    if (!processed_) {
        old_delay_ = delay_ = frames;
        fade_ = kFadeFrames;
        return;
    }
    old_delay_ = delay_;
    delay_ = frames;
    fade_ = 0;
}
void DelayLine::process(const Block &in, Block &out) {
    if (in.frames > 0)
        processed_ = true;
    for (int n = 0; n < in.frames; ++n) {
        float t = std::min(1.0f, float(fade_) / kFadeFrames);
        for (int c = 0; c < channels_; ++c) {
            auto *r = ring_.data() + c * size_;
            r[pos_] = in.plane[c][n];
            out.plane[c][n] = (1 - t) * r[(pos_ + size_ - old_delay_) % size_] +
                              t * r[(pos_ + size_ - delay_) % size_];
        }
        pos_ = (pos_ + 1) % size_;
        if (fade_ < kFadeFrames)
            ++fade_;
    }
}
} // namespace ndimon::audio
