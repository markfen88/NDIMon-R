#pragma once
#include "AlsaPcm.h"
#include "AudioSource.h"
#include "DriftCompensator.h"
namespace ndimon::audio {
class AlsaSource : public AudioSource {
  public:
    AlsaSource(std::string id, std::string pcm_name, int channels, int period_frames);
    const std::string &id() const override { return id_; }
    int channels() const override { return channels_; }
    bool ready() const override { return pcm_.ready(); }
    void pull(Block &) override;
    nlohmann::json status() const override;

  private:
    std::string id_;
    int channels_, capacity_;
    AlsaPcm pcm_;
    DriftCompensator drift_;
    std::vector<float> storage_;
    Block scratch_;
    bool primed_ = false;
};
} // namespace ndimon::audio
