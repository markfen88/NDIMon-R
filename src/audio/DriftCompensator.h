#pragma once
#include "AudioTypes.h"
#include <atomic>
#include <vector>
struct SwrContext;
namespace ndimon::audio {
struct DriftParams {
    int channels;
    int rate = 48000;
    int target_fill_frames = 1024;
    int capacity_frames = 8192;
    double kp = 4e-6, ki = 2e-8;
    int max_ppm = 3000;
};
class DriftCompensator {
  public:
    explicit DriftCompensator(DriftParams params);
    ~DriftCompensator();
    void push(const float *const *planes, int frames);
    bool pull(Block &out);
    int drain(float *const *out, int max_frames, int measured_fill);
    double ppm() const { return ppm_; }
    int fill() const {
        return int(write_.load(std::memory_order_acquire) - read_.load(std::memory_order_acquire));
    }
    Counters counters() const { return {frames_.load(), underruns_.load(), overruns_.load(), 0}; }
    void reset();
    void discard() {
        read_.store(write_.load(std::memory_order_acquire), std::memory_order_release);
    }

  private:
    void control(int error, int distance, bool playback);
    int convert(float *const *out, int frames);
    DriftParams params_;
    SwrContext *swr_ = nullptr;
    std::vector<float> fifo_, scratch_;
    std::atomic<uint64_t> read_{0}, write_{0};
    double integral_ = 0;
    std::atomic<double> ppm_{0};
    std::atomic<uint64_t> frames_{0};
    std::atomic<uint32_t> underruns_{0}, overruns_{0};
};
} // namespace ndimon::audio
