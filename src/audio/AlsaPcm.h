#pragma once
#include "AudioTypes.h"
#include <alsa/asoundlib.h>
#include <atomic>
#include <string>
#include <vector>
namespace ndimon::audio {
class AlsaPcm {
  public:
    AlsaPcm(const std::string &name, int channels, int period, bool capture, bool master);
    ~AlsaPcm();
    bool ready() const { return pcm_ != nullptr && healthy_.load(std::memory_order_acquire); }
    int open_error() const { return runtime_error_.load() ? runtime_error_.load() : open_error_; }
    int avail();
    int delay();
    bool recover(int err);
    bool wait(int ms);
    int write_planar(const Block &);
    int read_planar(Block &, int max_frames);
    uint32_t xruns() const { return xruns_; }

  private:
    snd_pcm_t *pcm_ = nullptr;
    int channels_, capacity_;
    bool capture_;
    int open_error_ = 0;
    std::atomic<bool> healthy_{true};
    std::atomic<int> runtime_error_{0};
    snd_pcm_format_t format_ = SND_PCM_FORMAT_UNKNOWN;
    std::vector<float> floats_;
    std::vector<int32_t> i32_;
    std::vector<int16_t> i16_;
    std::atomic<uint32_t> xruns_{0};
};
} // namespace ndimon::audio
