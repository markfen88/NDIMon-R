#pragma once
#include "AudioTypes.h"
#include <vector>
namespace ndimon::audio {
class DelayLine {
  public:
    static constexpr int kFadeFrames = 480;
    DelayLine(int channels, int max_delay_frames);
    void set_delay_frames(int frames);
    void process(const Block &in, Block &out);
    int delay_frames() const { return delay_; }

  private:
    int channels_, size_, pos_ = 0, delay_ = 0, old_delay_ = 0, fade_ = kFadeFrames;
    std::vector<float> ring_;
    bool processed_ = false;
};
} // namespace ndimon::audio
