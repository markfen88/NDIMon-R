#pragma once
#include "AudioSink.h"
#include <ctime>
namespace ndimon::audio {
class TimerMaster : public AudioSink {
  public:
    explicit TimerMaster(int frames = 512) : period_ns_(int64_t(frames) * 1000000000 / kRate) {}
    const std::string &id() const override { return id_; }
    int channels() const override { return 2; }
    bool is_master() const override { return true; }
    bool wait_period(int) override;
    void push(const Block &) override {}
    nlohmann::json status() const override {
        return {{"ready", true}, {"timer", true}, {"xruns", 0}};
    }

  private:
    std::string id_ = "timer";
    int64_t period_ns_, deadline_ = 0;
};
} // namespace ndimon::audio
