#include "AlsaAudio.h"
#include <iostream>
#include <cstring>
#include <cmath>
#include <climits>

AlsaAudio::AlsaAudio() = default;

AlsaAudio::~AlsaAudio() {
    destroy();
}

bool AlsaAudio::init(const std::string& device, int sample_rate, int channels) {
    sample_rate_ = sample_rate;
    channels_    = channels;

    int err = snd_pcm_open(&pcm_, device.c_str(), SND_PCM_STREAM_PLAYBACK, 0);
    if (err < 0) {
        std::cerr << "[AlsaAudioSink] snd_pcm_open failed: " << snd_strerror(err) << "\n";
        pcm_ = nullptr;
        return false;
    }

    snd_pcm_hw_params_t* hw_params;
    snd_pcm_hw_params_alloca(&hw_params);
    snd_pcm_hw_params_any(pcm_, hw_params);
    snd_pcm_hw_params_set_access(pcm_, hw_params, SND_PCM_ACCESS_RW_INTERLEAVED);
    snd_pcm_hw_params_set_format(pcm_, hw_params, SND_PCM_FORMAT_S16_LE);
    snd_pcm_hw_params_set_rate_near(pcm_, hw_params, (unsigned int*)&sample_rate_, 0);
    snd_pcm_hw_params_set_channels(pcm_, hw_params, channels_);

    unsigned int periods = 4;
    snd_pcm_uframes_t period_size = 1024;
    snd_pcm_hw_params_set_periods_near(pcm_, hw_params, &periods, 0);
    snd_pcm_hw_params_set_period_size_near(pcm_, hw_params, &period_size, 0);

    err = snd_pcm_hw_params(pcm_, hw_params);
    if (err < 0) {
        std::cerr << "[AlsaAudioSink] snd_pcm_hw_params failed: " << snd_strerror(err) << "\n";
        snd_pcm_close(pcm_);
        pcm_ = nullptr;
        return false;
    }

    err = snd_pcm_prepare(pcm_);
    if (err < 0) {
        std::cerr << "[AlsaAudioSink] snd_pcm_prepare failed: " << snd_strerror(err) << "\n";
        snd_pcm_close(pcm_);
        pcm_ = nullptr;
        return false;
    }

    std::cout << "[AlsaAudioSink] initialized rate=" << sample_rate_
              << " ch=" << channels_ << "\n";
    return true;
}

void AlsaAudio::destroy() {
    if (pcm_) {
        snd_pcm_drain(pcm_);
        snd_pcm_close(pcm_);
        pcm_ = nullptr;
    }
}

bool AlsaAudio::write_audio(const float** channel_data, int num_channels,
                              int num_samples, int channel_stride_bytes) {
    if (!pcm_) return false;

    // Convert planar float to interleaved S16LE
    int out_channels = std::min(num_channels, channels_);
    mix_buf_.resize(num_samples * channels_);

    for (int s = 0; s < num_samples; s++) {
        for (int c = 0; c < channels_; c++) {
            if (c < out_channels && channel_data[c]) {
                // channel_data[c] is a float* with stride channel_stride_bytes
                const float* ch = (const float*)((const uint8_t*)channel_data[c]
                                                  + s * sizeof(float));
                mix_buf_[s * channels_ + c] = ch[0];
            } else {
                mix_buf_[s * channels_ + c] = 0.0f;
            }
        }
    }

    // Convert to S16
    std::vector<int16_t> pcm_data(num_samples * channels_);
    for (int i = 0; i < num_samples * channels_; i++) {
        float v = mix_buf_[i];
        if (v >  1.0f) v =  1.0f;
        if (v < -1.0f) v = -1.0f;
        pcm_data[i] = (int16_t)(v * 32767.0f);
    }

    snd_pcm_sframes_t written = snd_pcm_writei(pcm_, pcm_data.data(), num_samples);
    if (written < 0) {
        if (written == -EPIPE) {
            snd_pcm_prepare(pcm_);
        } else {
            std::cerr << "[AlsaAudioSink] write error: " << snd_strerror((int)written) << "\n";
        }
        return false;
    }
    return true;
}

bool AlsaAudio::write_audio_i16(const int16_t* interleaved, int num_samples, int channels) {
    if (!pcm_) return false;
    snd_pcm_sframes_t written = snd_pcm_writei(pcm_, interleaved, num_samples);
    if (written < 0) {
        if (written == -EPIPE) snd_pcm_prepare(pcm_);
        return false;
    }
    return true;
}
