#pragma once
#include "AlsaPcm.h"
#include "AudioSink.h"
#include "DriftCompensator.h"
#include <memory>
namespace ndimon::audio {
class AlsaSink : public AudioSink {
  public:
    AlsaSink(std::string id, std::string pcm_name, int channels = 2, int period_frames = 512,
             bool master = true);
    const std::string &id() const override { return id_; }
    int channels() const override { return channels_; }
    bool is_master() const override { return master_; }
    bool wait_period(int ms) override { return pcm_.wait(ms); }
    void push(const Block &) override;
    nlohmann::json status() const override;

  private:
    std::string id_, pcm_name_;
    int channels_, period_;
    bool master_;
    std::shared_ptr<void> lease_;
    AlsaPcm pcm_;
    DriftCompensator drift_;
    std::vector<float> storage_;
    Block block_;
    std::atomic<uint64_t> frames_{0};
    std::atomic<uint32_t> short_writes_{0};
    std::atomic<int> fill_{0};
};
} // namespace ndimon::audio
