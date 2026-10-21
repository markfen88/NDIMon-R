#pragma once
#include "AudioTypes.h"
#include <nlohmann/json.hpp>
#include <string>
namespace ndimon::audio {
class AudioSink {
  public:
    virtual ~AudioSink() = default;
    virtual const std::string &id() const = 0;
    virtual int channels() const = 0;
    virtual bool is_master() const = 0;
    virtual bool placeholder() const { return false; }
    virtual bool wait_period(int timeout_ms) = 0;
    virtual void push(const Block &in) = 0;
    virtual nlohmann::json status() const = 0;
};
} // namespace ndimon::audio
