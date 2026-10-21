#include "DriftCompensator.h"
#include <cmath>
#include <stdexcept>
extern "C" {
#include <libavutil/channel_layout.h>
#include <libswresample/swresample.h>
}
namespace ndimon::audio {
DriftCompensator::DriftCompensator(DriftParams p)
    : params_(p), fifo_(p.channels * p.capacity_frames), scratch_(p.channels * p.capacity_frames) {
    AVChannelLayout layout;
    av_channel_layout_default(&layout, p.channels);
    int rc = swr_alloc_set_opts2(&swr_, &layout, AV_SAMPLE_FMT_FLTP, p.rate, &layout,
                                 AV_SAMPLE_FMT_FLTP, p.rate, 0, nullptr);
    av_channel_layout_uninit(&layout);
    if (rc < 0 || !swr_ || swr_init(swr_) < 0) {
        swr_free(&swr_);
        throw std::runtime_error("audio resampler init failed");
    }
    // Force the resampling path at construction, reserving its filter storage before RT starts.
    swr_set_compensation(swr_, 0, p.rate);
    // Warm the maximum conversion before publication, reserving swr's sample
    // storage as well as its filter. The retained filter history is silence.
    const uint8_t *input[kMaxChannels]{};
    uint8_t *output[kMaxChannels]{};
    for (int c = 0; c < p.channels; ++c) {
        input[c] = reinterpret_cast<const uint8_t *>(scratch_.data() + c * p.capacity_frames);
        output[c] = reinterpret_cast<uint8_t *>(fifo_.data() + c * p.capacity_frames);
    }
    if (swr_convert(swr_, output, p.capacity_frames, input, p.capacity_frames) < 0) {
        swr_free(&swr_);
        throw std::runtime_error("audio resampler warmup failed");
    }
}
DriftCompensator::~DriftCompensator() { swr_free(&swr_); }
void DriftCompensator::reset() {
    read_ = 0;
    write_ = 0;
    integral_ = 0;
    ppm_ = 0;
    frames_ = 0;
    underruns_ = 0;
    overruns_ = 0;
    swr_close(swr_);
    swr_init(swr_);
    swr_set_compensation(swr_, 0, params_.rate);
}
void DriftCompensator::push(const float *const *planes, int frames) {
    auto w = write_.load(std::memory_order_relaxed);
    auto r = read_.load(std::memory_order_acquire);
    int n = std::min(frames, params_.capacity_frames - int(w - r));
    if (n < frames)
        ++overruns_;
    for (int c = 0; c < params_.channels; ++c)
        for (int i = 0; i < n; ++i)
            fifo_[c * params_.capacity_frames + (w + i) % params_.capacity_frames] = planes[c][i];
    write_.store(w + n, std::memory_order_release);
}
void DriftCompensator::control(int error, int, bool) {
    double next = integral_ + error;
    double demand = params_.kp * error + params_.ki * next;
    double limit = params_.max_ppm * 1e-6;
    if (std::abs(demand) < limit || demand * error < 0)
        integral_ = next;
    ppm_ = std::clamp((params_.kp * error + params_.ki * integral_) * 1e6, -double(params_.max_ppm),
                      double(params_.max_ppm));
    // A rate-length window retains sub-sample ppm precision (512-frame windows round 500 ppm to
    // zero).
    int delta = -int(std::lround(ppm_ * 1e-6 * params_.rate));
    swr_set_compensation(swr_, delta, params_.rate);
}
int DriftCompensator::convert(float *const *out, int frames) {
    int buffered = int(swr_get_delay(swr_, params_.rate));
    int wanted = std::max(0, int(std::ceil(frames * (1 + ppm_ * 1e-6))) + 32 - buffered);
    int n = std::min(fill(), wanted);
    auto r = read_.load(std::memory_order_relaxed);
    const uint8_t *src[kMaxChannels]{};
    uint8_t *dst[kMaxChannels]{};
    for (int c = 0; c < params_.channels; ++c) {
        float *s = scratch_.data() + c * params_.capacity_frames;
        for (int i = 0; i < n; ++i)
            s[i] = fifo_[c * params_.capacity_frames + (r + i) % params_.capacity_frames];
        src[c] = reinterpret_cast<const uint8_t *>(s);
        dst[c] = reinterpret_cast<uint8_t *>(out[c]);
    }
    int made = swr_convert(swr_, dst, frames, src, n);
    read_.store(r + n, std::memory_order_release);
    return std::max(0, made);
}
bool DriftCompensator::pull(Block &out) {
    if (fill() < out.frames) {
        out.silence();
        ++underruns_;
        return false;
    }
    // Target is the fill after consumption, as specified by the PI loop.
    control(fill() - out.frames - params_.target_fill_frames, out.frames, false);
    int made = convert(out.plane, out.frames);
    for (int c = 0; c < out.channels; ++c)
        std::fill(out.plane[c] + made, out.plane[c] + out.frames, 0);
    frames_ += out.frames;
    if (made < out.frames)
        ++underruns_;
    return made == out.frames;
}
int DriftCompensator::drain(float *const *out, int frames, int measured_fill) {
    control(measured_fill + fill() - params_.target_fill_frames, frames, true);
    int n = convert(out, frames);
    frames_ += n;
    return n;
}
} // namespace ndimon::audio
