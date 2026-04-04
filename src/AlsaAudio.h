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
    std::string device_name() const { return device_name_; }

    // Write planar float audio (NDI native format) to ALSA.
    // channel_data: array of float* for each channel
    bool write_audio(const float** channel_data, int num_channels,
                     int num_samples, int channel_stride_bytes);

    // Write interleaved int16 audio
    bool write_audio_i16(const int16_t* interleaved, int num_samples, int channels);

    // Auto-detect an HDMI audio device for the given DRM connector.
    // Returns an ALSA device string like "hw:0,3" or "plughw:CARD=vc4hdmi0",
    // or empty string if not found. Falls back to "default" as last resort.
    static std::string find_hdmi_device(const std::string& connector_name = "");

private:
    snd_pcm_t*   pcm_      = nullptr;
    int sample_rate_        = 48000;
    int channels_           = 2;
    std::string device_name_;
    std::vector<int16_t> pcm_buf_;  // reusable interleaved S16 buffer
};
