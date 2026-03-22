#pragma once
#include <string>
#include <vector>
#include <memory>
#include <alsa/asoundlib.h>

class AlsaAudio {
public:
    AlsaAudio();
    ~AlsaAudio();

    bool init(const std::string& device = "default",
              int sample_rate = 48000, int channels = 2);
    void destroy();

    bool is_initialized() const { return pcm_ != nullptr; }

    // Write planar float audio (NDI native format) to ALSA.
    // channel_data: array of float* for each channel
    // channel_stride: stride in bytes between samples within each channel
    bool write_audio(const float** channel_data, int num_channels,
                     int num_samples, int channel_stride_bytes);

    // Write interleaved int16 audio
    bool write_audio_i16(const int16_t* interleaved, int num_samples, int channels);

private:
    snd_pcm_t*   pcm_      = nullptr;
    int sample_rate_        = 48000;
    int channels_           = 2;
    std::vector<float> mix_buf_;
};
