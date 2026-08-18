#include "AlsaAudio.h"
#include <iostream>
#include <fstream>
#include <cstring>
#include <cmath>
#include <climits>
#include <dirent.h>

AlsaAudio::AlsaAudio() = default;

AlsaAudio::~AlsaAudio() {
    destroy();
}

// ---------------------------------------------------------------------------
// find_hdmi_device — enumerate ALSA cards/devices to find HDMI audio output
// ---------------------------------------------------------------------------
std::string AlsaAudio::find_hdmi_device(const std::string& connector_name) {
    // Strategy:
    // 1. Enumerate ALSA cards, look for HDMI/hdmi/DisplayPort in the card name
    // 2. For each HDMI card, find the first playback device (PCM)
    // 3. Match connector_name to card index if possible (HDMI-A-1 → hdmi0, HDMI-A-2 → hdmi1)
    // 4. Fall back to "default" if nothing found

    struct HdmiCard {
        int         card;
        int         device;
        std::string id;       // e.g. "rockchiphdmi0", "vc4hdmi0"
        std::string name;     // longname
    };
    std::vector<HdmiCard> candidates;

    int card = -1;
    while (snd_card_next(&card) == 0 && card >= 0) {
        char* name = nullptr;
        char* longname = nullptr;
        snd_card_get_name(card, &name);
        snd_card_get_longname(card, &longname);

        std::string card_name  = name     ? name     : "";
        std::string card_long  = longname ? longname : "";
        free(name);
        free(longname);

        // Check if this card is HDMI/DP related
        auto contains_ci = [](const std::string& haystack, const char* needle) {
            std::string h = haystack, n = needle;
            for (auto& c : h) c = (char)tolower((unsigned char)c);
            for (auto& c : n) c = (char)tolower((unsigned char)c);
            return h.find(n) != std::string::npos;
        };

        bool is_hdmi = contains_ci(card_name, "hdmi") ||
                       contains_ci(card_long, "hdmi") ||
                       contains_ci(card_name, "displayport") ||
                       contains_ci(card_long, "displayport");
        if (!is_hdmi) continue;

        // Find playback PCM devices on this card
        // Read the card's ID from /proc/asound
        std::string card_id;
        {
            char buf[64];
            snprintf(buf, sizeof(buf), "hw:%d", card);
            snd_ctl_t* ctl = nullptr;
            if (snd_ctl_open(&ctl, buf, 0) == 0) {
                snd_ctl_card_info_t* info;
                snd_ctl_card_info_alloca(&info);
                if (snd_ctl_card_info(ctl, info) == 0)
                    card_id = snd_ctl_card_info_get_id(info);
                snd_ctl_close(ctl);
            }
        }

        int dev = -1;
        char hw[32];
        snprintf(hw, sizeof(hw), "hw:%d", card);
        snd_ctl_t* ctl = nullptr;
        if (snd_ctl_open(&ctl, hw, 0) != 0) continue;

        while (snd_ctl_pcm_next_device(ctl, &dev) == 0 && dev >= 0) {
            snd_pcm_info_t* pcm_info;
            snd_pcm_info_alloca(&pcm_info);
            snd_pcm_info_set_device(pcm_info, dev);
            snd_pcm_info_set_subdevice(pcm_info, 0);
            snd_pcm_info_set_stream(pcm_info, SND_PCM_STREAM_PLAYBACK);
            if (snd_ctl_pcm_info(ctl, pcm_info) == 0) {
                HdmiCard hc;
                hc.card   = card;
                hc.device = dev;
                hc.id     = card_id;
                hc.name   = card_name;
                candidates.push_back(hc);
                break;  // take first playback device per card
            }
        }
        snd_ctl_close(ctl);
    }

    if (candidates.empty()) {
        std::cerr << "[AlsaAudioSink] No HDMI audio card found, using 'default'\n";
        return "default";
    }

    // Try to match connector_name to a specific HDMI card
    // "HDMI-A-1" → look for hdmi0/vc4hdmi0, "HDMI-A-2" → hdmi1/vc4hdmi1
    int connector_idx = 0;  // default to first HDMI
    if (connector_name.size() > 0) {
        // Extract trailing digit from "HDMI-A-1", "HDMI-A-2"
        char last = connector_name.back();
        if (last >= '1' && last <= '9')
            connector_idx = last - '1';  // "HDMI-A-1" → 0, "HDMI-A-2" → 1
    }

    // Select the matching candidate (or first if idx out of range)
    const HdmiCard& selected = (connector_idx < (int)candidates.size())
        ? candidates[connector_idx] : candidates[0];

    // Use plughw: for automatic format/rate conversion if the hardware
    // doesn't natively support our S16LE/48kHz. This is critical on
    // Rockchip where HDMI I2S often only accepts S32LE or specific rates.
    char device[64];
    snprintf(device, sizeof(device), "plughw:%d,%d",
             selected.card, selected.device);

    std::cout << "[AlsaAudioSink] HDMI audio device: " << device
              << " (card=" << selected.id << " '" << selected.name << "')\n";
    return device;
}

bool AlsaAudio::init(const std::string& device, int sample_rate, int channels) {
    sample_rate_ = sample_rate;
    channels_    = channels;
    device_name_ = device;

    int err = snd_pcm_open(&pcm_, device.c_str(), SND_PCM_STREAM_PLAYBACK, 0);
    if (err < 0) {
        std::cerr << "[AlsaAudioSink] snd_pcm_open(" << device
                  << ") failed: " << snd_strerror(err) << "\n";
        pcm_ = nullptr;
        return false;
    }

    // --- Hardware params ---
    snd_pcm_hw_params_t* hw_params;
    snd_pcm_hw_params_alloca(&hw_params);
    snd_pcm_hw_params_any(pcm_, hw_params);
    snd_pcm_hw_params_set_access(pcm_, hw_params, SND_PCM_ACCESS_RW_INTERLEAVED);
    snd_pcm_hw_params_set_format(pcm_, hw_params, SND_PCM_FORMAT_S16_LE);

    unsigned int rate = (unsigned int)sample_rate_;
    snd_pcm_hw_params_set_rate_near(pcm_, hw_params, &rate, 0);
    sample_rate_ = (int)rate;

    // Try requested channel count; fall back to what the device supports
    err = snd_pcm_hw_params_set_channels(pcm_, hw_params, channels_);
    if (err < 0) {
        // Some HDMI devices only support specific channel counts (e.g., 8)
        unsigned int min_ch = 0, max_ch = 0;
        snd_pcm_hw_params_get_channels_min(hw_params, &min_ch);
        snd_pcm_hw_params_get_channels_max(hw_params, &max_ch);
        // Pick the smallest count >= 2
        unsigned int try_ch = (min_ch >= 2) ? min_ch : 2;
        if (try_ch > max_ch) try_ch = max_ch;
        err = snd_pcm_hw_params_set_channels(pcm_, hw_params, try_ch);
        if (err < 0) {
            std::cerr << "[AlsaAudioSink] Cannot set any channel count on "
                      << device << "\n";
            snd_pcm_close(pcm_);
            pcm_ = nullptr;
            return false;
        }
        channels_ = (int)try_ch;
        std::cout << "[AlsaAudioSink] Device requires " << channels_
                  << " channels (requested " << channels << ")\n";
    }

    // Buffer: 4 periods × 1024 frames = ~85ms at 48kHz
    unsigned int periods = 4;
    snd_pcm_uframes_t period_size = 1024;
    snd_pcm_hw_params_set_periods_near(pcm_, hw_params, &periods, 0);
    snd_pcm_hw_params_set_period_size_near(pcm_, hw_params, &period_size, 0);

    err = snd_pcm_hw_params(pcm_, hw_params);
    if (err < 0) {
        std::cerr << "[AlsaAudioSink] snd_pcm_hw_params failed: "
                  << snd_strerror(err) << "\n";
        snd_pcm_close(pcm_);
        pcm_ = nullptr;
        return false;
    }

    // --- Software params: start threshold ---
    // Start playback after one period is filled (reduces initial latency).
    // Without this, ALSA waits until the entire buffer is full.
    snd_pcm_sw_params_t* sw_params;
    snd_pcm_sw_params_alloca(&sw_params);
    snd_pcm_sw_params_current(pcm_, sw_params);
    snd_pcm_sw_params_set_start_threshold(pcm_, sw_params, period_size);
    snd_pcm_sw_params(pcm_, sw_params);

    err = snd_pcm_prepare(pcm_);
    if (err < 0) {
        std::cerr << "[AlsaAudioSink] snd_pcm_prepare failed: "
                  << snd_strerror(err) << "\n";
        snd_pcm_close(pcm_);
        pcm_ = nullptr;
        return false;
    }

    std::cout << "[AlsaAudioSink] initialized: " << device
              << " rate=" << sample_rate_ << " ch=" << channels_
              << " period=" << period_size << " periods=" << periods << "\n";
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
    if (!pcm_ || num_samples <= 0) return false;

    pcm_buf_.resize(num_samples * channels_);

    // 5.1 surround → stereo downmix (ITU-R BS.775)
    // Layout: FL=0, FR=1, C=2, LFE=3, SL=4, SR=5
    // L = FL + 0.707*C + 0.707*SL (+0.5*LFE optional)
    // R = FR + 0.707*C + 0.707*SR (+0.5*LFE optional)
    if (num_channels >= 6 && channels_ == 2) {
        const float* fl  = channel_data[0];
        const float* fr  = channel_data[1];
        const float* c   = channel_data[2];
        const float* lfe = channel_data[3];
        const float* sl  = channel_data[4];
        const float* sr  = channel_data[5];
        constexpr float k = 0.707f;  // -3dB
        constexpr float lfe_k = 0.5f;
        for (int s = 0; s < num_samples; s++) {
            float center = c ? c[s] : 0.0f;
            float sub    = lfe ? lfe[s] : 0.0f;
            float lv = (fl ? fl[s] : 0.0f) + k * center + k * (sl ? sl[s] : 0.0f) + lfe_k * sub;
            float rv = (fr ? fr[s] : 0.0f) + k * center + k * (sr ? sr[s] : 0.0f) + lfe_k * sub;
            // Clamp
            if (lv >  1.0f) lv =  1.0f; if (lv < -1.0f) lv = -1.0f;
            if (rv >  1.0f) rv =  1.0f; if (rv < -1.0f) rv = -1.0f;
            pcm_buf_[s * 2]     = (int16_t)(lv * 32767.0f);
            pcm_buf_[s * 2 + 1] = (int16_t)(rv * 32767.0f);
        }
    } else {
        // Generic: copy up to channels_ from source, zero-fill the rest
        int src_channels = std::min(num_channels, channels_);
        for (int s = 0; s < num_samples; s++) {
            for (int c = 0; c < channels_; c++) {
                float v;
                if (c < src_channels && channel_data[c]) {
                    v = channel_data[c][s];
                } else {
                    v = 0.0f;
                }
                if (v >  1.0f) v =  1.0f;
                if (v < -1.0f) v = -1.0f;
                pcm_buf_[s * channels_ + c] = (int16_t)(v * 32767.0f);
            }
        }
    }

    snd_pcm_sframes_t written = snd_pcm_writei(pcm_, pcm_buf_.data(), num_samples);
    if (written < 0) {
        if (written == -EPIPE) {
            // Underrun: ALSA ran out of data. Common during source switches
            // or when the NDI sender's clock drifts from our playback clock.
            snd_pcm_prepare(pcm_);
        } else if (written == -ESTRPIPE) {
            // Suspended (e.g., power management)
            while (snd_pcm_resume(pcm_) == -EAGAIN)
                usleep(10000);
            snd_pcm_prepare(pcm_);
        } else {
            std::cerr << "[AlsaAudioSink] write error: "
                      << snd_strerror((int)written) << "\n";
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
