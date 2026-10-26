#include "AlsaSource.h"
namespace ndimon::audio {
AlsaSource::AlsaSource(std::string id, std::string name, int ch, int p)
    : id_(std::move(id)), channels_(ch), capacity_(8 * p), pcm_(name, ch, p, true, false),
      // Capture's Tessera lag controls when samples become available; it is
      // not included in this FIFO's fill. pull() reads available samples once
      // per engine period, and the PI target is measured AFTER consumption.
      // Two periods here are real queued audio, unlike AlsaSink's delay()
      // measurement, which includes the plugin's playback lead.
      drift_({ch, kRate, 2 * p, 8 * p}), storage_(ch * capacity_) {
    scratch_.channels = ch;
    scratch_.frames = capacity_;
    for (int c = 0; c < ch; ++c)
        scratch_.plane[c] = storage_.data() + c * capacity_;
}
void AlsaSource::pull(Block &out) {
    // Bound work even if a device continually reports more available data.
    int n = pcm_.read_planar(scratch_, capacity_);
    const float *planes[kMaxChannels]{};
    for (int c = 0; c < channels_; ++c)
        planes[c] = scratch_.plane[c];
    if (n > 0)
        drift_.push(planes, n);
    if (!primed_ && drift_.fill() >= out.frames * 3)
        primed_ = true;
    if (primed_)
        drift_.pull(out);
    else
        out.silence();
}
nlohmann::json AlsaSource::status() const {
    return {{"ready", ready()},
            {"channels", channels_},
            {"error_code", pcm_.open_error()},
            {"fill_frames", drift_.fill()},
            {"ppm", drift_.ppm()},
            {"underruns", drift_.counters().underruns},
            {"overruns", drift_.counters().overruns},
            {"xruns", pcm_.xruns()}};
}
} // namespace ndimon::audio
