#pragma once
#include "AudioSink.h"
#include "AudioSource.h"
namespace ndimon::audio {
class UnavailableSource final : public AudioSource {
    std::string id_, reason_;
    int channels_;

  public:
    UnavailableSource(std::string id, int channels, std::string reason)
        : id_(std::move(id)), reason_(std::move(reason)), channels_(channels) {}
    const std::string &id() const override { return id_; }
    int channels() const override { return channels_; }
    bool ready() const override { return false; }
    bool placeholder() const override { return true; }
    void pull(Block &out) override { out.silence(); }
    nlohmann::json status() const override { return {{"ready", false}, {"error", reason_}}; }
};
class UnavailableSink final : public AudioSink {
    std::string id_, reason_;
    int channels_;
    bool master_;

  public:
    UnavailableSink(std::string id, int channels, bool master, std::string reason)
        : id_(std::move(id)), reason_(std::move(reason)), channels_(channels), master_(master) {}
    const std::string &id() const override { return id_; }
    int channels() const override { return channels_; }
    bool is_master() const override { return master_; }
    bool placeholder() const override { return true; }
    bool wait_period(int) override { return false; }
    void push(const Block &) override {}
    nlohmann::json status() const override { return {{"ready", false}, {"error", reason_}}; }
};
} // namespace ndimon::audio
