#include "AlsaPcm.h"
#include <cmath>
namespace ndimon::audio {
AlsaPcm::AlsaPcm(const std::string &name, int channels, int period, bool capture, bool master)
    : channels_(channels), capacity_(period * 8), capture_(capture), floats_(capacity_ * channels),
      i32_(capacity_ * channels), i16_(capacity_ * channels) {
    auto stream = capture ? SND_PCM_STREAM_CAPTURE : SND_PCM_STREAM_PLAYBACK;
    // Candidate opens must never wait behind the active exclusive handle.
    int flags = SND_PCM_NONBLOCK;
    int rc = snd_pcm_open(&pcm_, name.c_str(), stream, flags);
    if (rc < 0 && name.rfind("hw:", 0) == 0)
        rc = snd_pcm_open(&pcm_, ("plughw:" + name.substr(3)).c_str(), stream, flags);
    if (rc < 0) {
        open_error_ = rc;
        pcm_ = nullptr;
        return;
    }
    for (auto f : {SND_PCM_FORMAT_FLOAT_LE, SND_PCM_FORMAT_S32_LE, SND_PCM_FORMAT_S16_LE}) {
        snd_pcm_hw_params_t *hw;
        snd_pcm_hw_params_alloca(&hw);
        snd_pcm_hw_params_any(pcm_, hw);
        unsigned rate = kRate;
        snd_pcm_uframes_t p = period, buf = period * 4;
        if (snd_pcm_hw_params_set_access(pcm_, hw, SND_PCM_ACCESS_RW_INTERLEAVED) < 0 ||
            snd_pcm_hw_params_set_format(pcm_, hw, f) < 0 ||
            snd_pcm_hw_params_set_channels(pcm_, hw, channels) < 0 ||
            snd_pcm_hw_params_set_rate_near(pcm_, hw, &rate, nullptr) < 0 || rate != kRate ||
            snd_pcm_hw_params_set_period_size_near(pcm_, hw, &p, nullptr) < 0 ||
            snd_pcm_hw_params_set_buffer_size_near(pcm_, hw, &buf) < 0 ||
            snd_pcm_hw_params(pcm_, hw) < 0)
            continue;
        snd_pcm_sw_params_t *sw;
        snd_pcm_sw_params_alloca(&sw);
        if (snd_pcm_sw_params_current(pcm_, sw) < 0 ||
            snd_pcm_sw_params_set_avail_min(pcm_, sw, p) < 0 ||
            snd_pcm_sw_params_set_start_threshold(pcm_, sw, capture ? 1 : p) < 0 ||
            snd_pcm_sw_params(pcm_, sw) < 0)
            continue;
        format_ = f;
        rc = snd_pcm_prepare(pcm_);
        if (rc >= 0 && capture)
            rc = snd_pcm_start(pcm_);
        // Only I/O becomes blocking, after all device parameters are accepted.
        if (rc >= 0 && !capture && master)
            rc = snd_pcm_nonblock(pcm_, 0);
        if (rc < 0) {
            open_error_ = rc;
            break;
        }
        return;
    }
    if (!open_error_)
        open_error_ = -EINVAL;
    snd_pcm_close(pcm_);
    pcm_ = nullptr;
}
AlsaPcm::~AlsaPcm() {
    if (pcm_)
        snd_pcm_close(pcm_);
}
bool AlsaPcm::recover(int err) {
    if (!ready())
        return false;
    if (err == -EAGAIN || err == -EINTR)
        return false;
    if (err == -EPIPE || err == -ESTRPIPE) {
        ++xruns_;
        int result = snd_pcm_prepare(pcm_);
        if (result >= 0 && capture_)
            result = snd_pcm_start(pcm_);
        if (result >= 0)
            return true;
        err = result;
    }
    // Keep the failed handle alive until a control-side graph swap acknowledges
    // retirement. Never close/reopen a PCM on the audio thread.
    runtime_error_.store(err, std::memory_order_release);
    healthy_.store(false, std::memory_order_release);
    return false;
}
int AlsaPcm::avail() {
    if (!ready())
        return 0;
    auto n = snd_pcm_avail_update(pcm_);
    if (n < 0) {
        recover(int(n));
        return 0;
    }
    return int(n);
}
int AlsaPcm::delay() {
    snd_pcm_sframes_t d = 0;
    if (pcm_ && snd_pcm_delay(pcm_, &d) >= 0)
        return int(d);
    return 0;
}
bool AlsaPcm::wait(int ms) {
    if (!ready())
        return false;
    int rc = snd_pcm_wait(pcm_, ms);
    return rc < 0 ? recover(rc) : rc > 0;
}
int AlsaPcm::write_planar(const Block &b) {
    if (!ready())
        return 0;
    int frames = std::min(b.frames, capacity_);
    for (int n = 0; n < frames; ++n)
        for (int c = 0; c < channels_; ++c) {
            float x = std::clamp(b.plane[c][n], -1.f, 1.f);
            int i = n * channels_ + c;
            floats_[i] = x;
            i32_[i] = int32_t(double(x) * 2147483647);
            i16_[i] = int16_t(x * 32767);
        }
    const void *p = format_ == SND_PCM_FORMAT_FLOAT_LE ? static_cast<void *>(floats_.data())
                    : format_ == SND_PCM_FORMAT_S32_LE ? static_cast<void *>(i32_.data())
                                                       : static_cast<void *>(i16_.data());
    auto n = snd_pcm_writei(pcm_, p, frames);
    if (n < 0) {
        recover(int(n));
        return 0;
    }
    return int(n);
}
int AlsaPcm::read_planar(Block &b, int maximum) {
    if (!ready())
        return 0;
    int count = std::min({maximum, capacity_, avail()});
    if (count <= 0)
        return 0;
    void *p = format_ == SND_PCM_FORMAT_FLOAT_LE ? static_cast<void *>(floats_.data())
              : format_ == SND_PCM_FORMAT_S32_LE ? static_cast<void *>(i32_.data())
                                                 : static_cast<void *>(i16_.data());
    auto n = snd_pcm_readi(pcm_, p, count);
    if (n < 0) {
        recover(int(n));
        return 0;
    }
    for (int c = 0; c < channels_; ++c)
        for (int i = 0; i < n; ++i) {
            int j = i * channels_ + c;
            b.plane[c][i] = format_ == SND_PCM_FORMAT_FLOAT_LE ? floats_[j]
                            : format_ == SND_PCM_FORMAT_S32_LE ? float(i32_[j] / 2147483648.0)
                                                               : i16_[j] / 32768.f;
        }
    return int(n);
}
} // namespace ndimon::audio
