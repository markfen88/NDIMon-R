#pragma once
#include <algorithm>
#include <alsa/asoundlib.h>
#include <cctype>
#include <string>
#include <vector>

namespace ndimon::audio {
// Known per-connector ALSA card IDs on Rockchip and Raspberry Pi. Unlike the
// legacy resolver, this never redirects a missing second connector to the first.
// Other platforms must provide an explicitly resolved PCM in the catalog.
inline std::string connector_pcm(const std::string &connector) {
    std::string kind;
    size_t prefix = 0;
    if (connector.rfind("HDMI-A-", 0) == 0) {
        kind = "hdmi";
        prefix = 7;
    } else if (connector.rfind("DP-", 0) == 0) {
        kind = "dp";
        prefix = 3;
    } else
        return {};
    if (connector.size() != prefix + 1 || connector[prefix] < '1' || connector[prefix] > '9')
        return {};
    const auto suffix = kind + std::to_string(connector[prefix] - '1');
    const std::vector<std::string> ids{suffix, "vc4" + suffix, "rockchip" + suffix};
    int card = -1;
    while (snd_card_next(&card) == 0 && card >= 0) {
        snd_ctl_t *control = nullptr;
        const auto name = "hw:" + std::to_string(card);
        if (snd_ctl_open(&control, name.c_str(), 0) < 0)
            continue;
        snd_ctl_card_info_t *info;
        snd_ctl_card_info_alloca(&info);
        if (snd_ctl_card_info(control, info) < 0) {
            snd_ctl_close(control);
            continue;
        }
        std::string id = snd_ctl_card_info_get_id(info);
        auto lower = id;
        std::transform(lower.begin(), lower.end(), lower.begin(),
                       [](unsigned char c) { return char(std::tolower(c)); });
        if (std::find(ids.begin(), ids.end(), lower) == ids.end()) {
            snd_ctl_close(control);
            continue;
        }
        int device = -1;
        while (snd_ctl_pcm_next_device(control, &device) == 0 && device >= 0) {
            snd_pcm_info_t *pcm;
            snd_pcm_info_alloca(&pcm);
            snd_pcm_info_set_device(pcm, device);
            snd_pcm_info_set_subdevice(pcm, 0);
            snd_pcm_info_set_stream(pcm, SND_PCM_STREAM_PLAYBACK);
            if (snd_ctl_pcm_info(control, pcm) == 0) {
                snd_ctl_close(control);
                return "plughw:CARD=" + id + ",DEV=" + std::to_string(device);
            }
        }
        snd_ctl_close(control);
    }
    return {};
}
} // namespace ndimon::audio
