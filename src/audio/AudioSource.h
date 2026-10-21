#pragma once
#include "AudioTypes.h"
#include <nlohmann/json.hpp>
#include <string>
namespace ndimon::audio {
class AudioSource {
  public:
    virtual ~AudioSource() = default;
    virtual const std::string &id() const = 0;
    virtual int channels() const = 0;
    virtual bool ready() const = 0;
    virtual bool placeholder() const { return false; }
    virtual void pull(Block &out) = 0;
    virtual void push_fifo(const float *const *, int) {
    } // Optional push input (NDI capture fallback).
    virtual void set_fallback_valid(bool) {}
    virtual void discard_fallback() {}
    virtual nlohmann::json status() const = 0;
};
} // namespace ndimon::audio
