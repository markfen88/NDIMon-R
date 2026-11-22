#include "AlsaSink.h"
#include "../AlsaAudio.h"
#include "PcmLease.h"
#include "AudioTiming.h"
#include <cstdlib>
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
      storage_(ch * 4 * p), target_(playback_drift_target_frames(p, p)) {
    block_.channels = ch;
    block_.frames = 4 * p;
    for (int c = 0; c < ch; ++c)
        block_.plane[c] = storage_.data() + c * 4 * p;
}
void AlsaSink::push(const Block &in) {
    if (!pcm_.ready())
        return;
    int delay = pcm_.delay();
    fill_ = delay;
    if (master_) {
        latency_ = delay;
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
    // A (re)started stream begins with an arbitrary queue. Slewing it to the
    // target at the loop's few hundred ppm took up to a minute and overshot by
    // several milliseconds, so the latency to the device differed after every
    // start. Align it in one step once the stream runs (its delay then
    // includes the plugin lead), after an xrun, and after a jump the loop
    // cannot absorb quickly.
    if (pcm_.xruns() != seen_xruns_) {
        seen_xruns_ = pcm_.xruns();
        aligned_ = false;
    }
    if (aligned_ && std::abs(delay + drift_.fill() - target_) > 4 * period_)
        aligned_ = false;
    if (!aligned_ && pcm_.running()) {
        int pad = std::min(drift_.align(delay), available);
        if (pad > 0) {
            block_.frames = pad;
            block_.silence();
            int n = pcm_.write_planar(block_);
            frames_ += n;
            delay += n;
            available -= n;
        }
        aligned_ = true;
        ++aligns_;
    }
    latency_ = delay + drift_.fill();
    if (available > 0) {
        block_.frames = drift_.drain(block_.plane, available, delay);
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
            {"latency_frames", latency_.load()},
            {"target_frames", master_ ? 0 : target_},
            {"aligns", aligns_.load()},
            {"ppm", drift_.ppm()},
            {"xruns", pcm_.xruns()},
            {"short_writes", short_writes_.load()},
            {"overruns", drift_.counters().overruns},
            {"frames", frames_.load()}};
}
} // namespace ndimon::audio
