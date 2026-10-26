#pragma once
#include <cstdint>
#include <map>
#include <nlohmann/json.hpp>
#include <string>
#include <vector>
namespace ndimon::audio {
struct SourceSettings {
    std::string id, type, alsa_pcm;
    bool enabled = true;
    int channels = 2, delay_ms = 0;
    float gain_db = 0;
    int output = 0;
    std::string endpoint, resource;
    int channel_start = 1, channel_count = 0;
};
struct SinkSettings {
    std::string id, type, alsa_pcm;
    bool enabled = true;
    int channels = 2, delay_ms = 0;
    std::map<std::string, float> mix;
    std::string endpoint, resource;
    int channel_start = 1, channel_count = 0;
};
struct EngineSettings {
    bool enabled = true;
    int rate = 48000, period_frames = 512;
    std::string master = "hdmi";
    int memory_budget_mib = 64;
};
struct RouteSettings {
    std::string id, source, sink;
    int output = 1;
    bool enabled = true;
    float gain_db = 0;
    int delay_ms = 0;
};
struct AudioSettings {
    int version = 1;
    uint64_t revision = 0;
    EngineSettings engine;
    std::vector<SourceSettings> sources{{"ndi", "ndi", "", true, 2, 0, 0}};
    std::vector<SinkSettings> sinks{{"hdmi", "alsa", "auto", true, 2, 0, {{"ndi", 0}}}};
    int video_delay_ms = 0;
    std::vector<std::string> warnings;
    std::map<int, nlohmann::json> outputs;
    std::vector<RouteSettings> routes;
    static AudioSettings load(const std::string &path, std::string *error);
    static AudioSettings parse(const nlohmann::json &j, std::string *error);
    bool needs_restart(const AudioSettings &other) const;
    nlohmann::json to_json() const;
};
} // namespace ndimon::audio
