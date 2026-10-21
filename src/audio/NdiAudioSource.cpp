#include "NdiAudioSource.h"
namespace ndimon::audio {
NdiAudioSource::NdiAudioSource(std::string id, int ch, Puller puller)
    : id_(std::move(id)), channels_(ch), puller_(std::move(puller)),
      fifo_({ch, kRate, 4096, 16384}), native_storage_(kMaxChannels * 2048) {
    for (int c = 0; c < kMaxChannels; ++c)
        native_.plane[c] = native_storage_.data() + c * 2048;
    native_inputs_.push_back({&native_, 1.f});
}
void NdiAudioSource::push_fifo(const float *const *planes, int frames) {
    if (using_fifo_)
        fifo_.push(planes, frames);
}
void NdiAudioSource::pull(Block &out) {
    if (discard_pending_.exchange(false, std::memory_order_acq_rel))
        fifo_.discard();
    if (out.frames > 2048) {
        out.silence();
        ++underruns_;
        return;
    }
    native_.channels = kMaxChannels;
    native_.frames = out.frames;
    int n = std::clamp(puller_(out.frames, kMaxChannels, native_.plane, &native_.channels), 0,
                       out.frames);
    if (native_.channels < 1 || native_.channels > kMaxChannels)
        n = 0;
    using_fifo_ = n == 0;
    if (n == 0) {
        if (!fallback_valid_.load(std::memory_order_acquire)) {
            fifo_.discard();
            out.silence();
            ++underruns_;
        } else if (!fifo_.pull(out))
            ++underruns_;
    } else {
        fifo_.discard();
        for (int c = 0; c < native_.channels; ++c)
            std::fill(native_.plane[c] + n, native_.plane[c] + out.frames, 0.f);
        mix(native_inputs_, out, false);
    }
    frames_ += out.frames;
}
nlohmann::json NdiAudioSource::status() const {
    return {{"ready", true},
            {"channels", channels_},
            {"fill_frames", fifo_.fill()},
            {"fallback_valid", fallback_valid_.load()},
            {"ppm", fifo_.ppm()},
            {"underruns", underruns_.load()},
            {"frames", frames_.load()}};
}
} // namespace ndimon::audio
