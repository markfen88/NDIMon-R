#include "AlsaSink.h"
#include "../AlsaAudio.h"
#include "PcmLease.h"
#include "AudioTiming.h"
namespace ndimon::audio {
AlsaSink::AlsaSink(std::string id, std::string name, int ch, int p, bool master)
    : id_(std::move(id)),
      pcm_name_(name == "auto" ? AlsaAudio::find_hdmi_device() : std::move(name)), channels_(ch),
      period_(p), master_(master), lease_(PcmLease::acquire(pcm_name_, id_)),
      // Target three periods: delay() of the Tessera plugin includes a lead of
      // about one period, and the engine writes one period at a time, so a
      // two-period target left under a period queued and underran between writes.
      pcm_(pcm_name_, ch, p, false, master),
      drift_({ch, kRate, playback_drift_target_frames(p, p), 8 * p}),
      storage_(ch * 4 * p) {
    block_.channels = ch;
    block_.frames = 4 * p;
    for (int c = 0; c < ch; ++c)
        block_.plane[c] = storage_.data() + c * 4 * p;
}
void AlsaSink::push(const Block &in) {
    if (!pcm_.ready())
        return;
    fill_ = pcm_.delay();
    if (master_) {
        int n = pcm_.write_planar(in);
        frames_ += n;
        if (n < in.frames)
            ++short_writes_;
        return;
    }
    const float *planes[kMaxChannels]{};
    for (int c = 0; c < channels_; ++c)
        planes[c] = in.plane[c];
    drift_.push(planes, in.frames);
    int available = std::min(pcm_.avail(), 4 * period_);
    if (available > 0) {
        block_.frames = drift_.drain(block_.plane, available, pcm_.delay());
        int n = pcm_.write_planar(block_);
        frames_ += n;
        if (n < block_.frames)
            ++short_writes_;
    }
}
nlohmann::json AlsaSink::status() const {
    return {{"ready", pcm_.ready()},
            {"alsa_pcm", pcm_name_},
            {"channels", channels_},
            {"error_code", pcm_.open_error()},
            {"fill_frames", fill_.load()},
            {"ppm", drift_.ppm()},
            {"xruns", pcm_.xruns()},
            {"short_writes", short_writes_.load()},
            {"overruns", drift_.counters().overruns},
            {"frames", frames_.load()}};
}
} // namespace ndimon::audio
