#pragma once
#include "AudioSource.h"
#include "DriftCompensator.h"
#include "MixMatrix.h"
#include <functional>
namespace ndimon::audio {
class NdiAudioSource : public AudioSource {
  public:
    using Puller = std::function<int(int, int, float *const *, int *)>;
    NdiAudioSource(std::string id, int channels, Puller puller);
    const std::string &id() const override { return id_; }
    int channels() const override { return channels_; }
    bool ready() const override { return true; }
    void pull(Block &) override;
    void push_fifo(const float *const *planes, int frames) override;
    void discard_fallback() override { fifo_.discard(); }
    void set_fallback_valid(bool valid) override {
        fallback_valid_.store(valid, std::memory_order_release);
        if (!valid)
            discard_pending_.store(true, std::memory_order_release);
    }
    nlohmann::json status() const override;

  private:
    std::string id_;
    int channels_;
    Puller puller_;
    DriftCompensator fifo_;
    std::vector<float> native_storage_;
    Block native_;
    std::vector<MixInput> native_inputs_;
    std::atomic<bool> using_fifo_{true};
    std::atomic<bool> fallback_valid_{true}, discard_pending_{false};
    std::atomic<uint64_t> frames_{0};
    std::atomic<uint32_t> underruns_{0};
};
} // namespace ndimon::audio
