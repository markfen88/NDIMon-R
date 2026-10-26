#pragma once
#include "Config.h"
#include <map>
#include <optional>

namespace follower {
struct NdiConfig {
    TransportConfig transport;
    FinderConfig finder;
    NDIGroupConfig group;
    std::string off_subnet_ips, tally_mode, color_space;
};
struct DisplayConfig {
    SplashConfig splash;
    OsdConfig osd;
};
struct AudioConfig {
    std::string engine;
    ndimon::audio::AudioSettings settings;
    bool primary_enabled = true;
};

// A supplied domain is a complete snapshot, using ndimon-types schema 1.
// These parsers are independent of the SDK, ALSA and device hardware.
DeviceConfig parse_device(const nlohmann::json& j);
std::unordered_map<int, OutputConfig> parse_outputs(const nlohmann::json& j);
NdiConfig parse_ndi(const nlohmann::json& j);
AudioConfig parse_audio(const nlohmann::json& j, uint64_t revision);
DisplayConfig parse_display(const nlohmann::json& j);
ndisdk::NetworkBinding parse_network(const nlohmann::json& j);

class NdiFollower {
public:
    Revision revision;
    std::optional<NdiConfig> config;
    nlohmann::json document;
    nlohmann::json apply_config(const nlohmann::json& request,
        const std::function<bool(const NdiConfig&)>& apply = {}) {
        try {
            const auto next = revision.check(request);
            const auto& domains = request.at("config");
            if (!domains.is_object() || domains.size() != 1 || !domains.contains("ndi"))
                return failure("config: finder accepts only the complete ndi domain");
            auto candidate = parse_ndi(domains.at("ndi"));
            if (apply && (!config || document != domains.at("ndi")) && !apply(candidate))
                return failure("ndi: finder recreation failed");
            config = std::move(candidate);
            document = domains.at("ndi");
            revision.commit(next);
            return {{"ok", true}, {"revision", next}};
        } catch (const std::exception& e) { return failure(e.what()); }
    }
};
} // namespace follower
