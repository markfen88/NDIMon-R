#include "AudioSettings.h"
#include <fstream>
#include <regex>
#include <set>
#include <stdexcept>
namespace ndimon::audio {
using nlohmann::json;
static void require(bool ok, const char *error) {
    if (!ok)
        throw std::runtime_error(error);
}
static int range(const json &j, const char *key, int lo, int hi) {
    const auto &x = j.at(key);
    require(x.is_number_integer(), "integer required");
    if (x.is_number_unsigned()) {
        auto v = x.get<uint64_t>();
        require(v >= uint64_t(lo) && v <= uint64_t(hi), "integer out of range");
        return static_cast<int>(v);
    }
    auto v = x.get<int64_t>();
    require(v >= lo && v <= hi, "integer out of range");
    return static_cast<int>(v);
}
static float gain(const json &j) {
    require(j.is_number(), "gain must be numeric");
    double v = j.get<double>();
    require(v >= -60 && v <= 12, "gain out of range");
    return v;
}
AudioSettings AudioSettings::load(const std::string &path, std::string *error) {
    try {
        std::ifstream file(path);
        json j;
        file >> j;
        return parse(j, error);
    } catch (const std::exception &e) {
        if (error) *error = e.what();
        return {};
    }
}
AudioSettings AudioSettings::parse(const json &j, std::string *error) {
    if (error)
        error->clear();
    try {
        require(j.at("version") == 1 || j.at("version") == 2, "unsupported audio settings version");
        AudioSettings s;
        s.version = j.at("version").get<int>();
        if (j.contains("revision")) {
            require(j["revision"].is_number_unsigned() || j["revision"].is_number_integer(),
                    "invalid revision");
            require(j["revision"].get<int64_t>() >= 0 &&
                        j["revision"].get<uint64_t>() <= 9007199254740991ULL,
                    "invalid revision");
            s.revision = j["revision"].get<uint64_t>();
        }
        auto e = j.at("engine");
        s.engine = {e.at("enabled").get<bool>(), range(e, "rate", 48000, 48000),
                    range(e, "period_frames", 128, 2048), e.at("master").get<std::string>()};
        if (e.contains("memory_budget_mib"))
            s.engine.memory_budget_mib = range(e, "memory_budget_mib", 1, 512);
        require(std::set<int>{128, 256, 512, 1024, 2048}.count(s.engine.period_frames),
                "invalid period_frames");
        s.video_delay_ms = range(j, "video_delay_ms", 0, 5000);
        require(j.at("sources").is_object() && j.at("sinks").is_object(),
                "sources and sinks must be objects");
        require(j["sources"].size() <= 32 && j["sinks"].size() <= 32, "too many audio endpoints");
        std::regex pcm("^[A-Za-z0-9:_=,.+-]{1,512}$");
        std::regex ids("^[a-z][a-z0-9-]{0,31}$");
        s.sources.clear();
        s.sinks.clear();
        std::set<std::string> names;
        for (auto it = j["sources"].begin(); it != j["sources"].end(); ++it) {
            auto &v = it.value();
            require(std::regex_match(it.key(), ids), "invalid source id");
            SourceSettings x{it.key(),
                             v.at("type").get<std::string>(),
                             v.value("alsa_pcm", std::string{}),
                             v.at("enabled").get<bool>(),
                             range(v, "channels", 1, 16),
                             s.version == 1 ? range(v, "delay_ms", 0, 5000) : 0,
                             s.version == 1 ? gain(v.at("gain_db")) : 0};
            require(s.version == 1 ? x.type == (x.id == "ndi" ? "ndi" : "alsa")
                                   : (x.type == "ndi" || x.type == "alsa"),
                    "invalid source type");
            if (s.version == 2 && x.type == "ndi")
                x.output = range(v, "output", 1, 8);
            x.endpoint = v.value("endpoint", std::string{});
            x.resource = v.value("resource", std::string{});
            if (v.contains("channel_start"))
                x.channel_start = range(v, "channel_start", 1, 65535);
            x.channel_count =
                v.contains("channel_count") ? range(v, "channel_count", 1, 16) : x.channels;
            require(x.channel_count == x.channels, "source channel_count must equal channels");
            require(x.type == "ndi" ? !v.contains("alsa_pcm") : std::regex_match(x.alsa_pcm, pcm),
                    "missing ALSA PCM");
            names.insert(x.id);
            s.sources.push_back(x);
        }
        require(s.version == 2 || names.count("ndi"), "missing ndi source");
        bool master = false, hdmi = false;
        for (auto it = j["sinks"].begin(); it != j["sinks"].end(); ++it) {
            auto &v = it.value();
            require(std::regex_match(it.key(), ids), "invalid sink id");
            SinkSettings x{it.key(),
                           v.at("type").get<std::string>(),
                           v.at("alsa_pcm").get<std::string>(),
                           v.at("enabled").get<bool>(),
                           range(v, "channels", 1, 16),
                           s.version == 1 ? range(v, "delay_ms", 0, 5000) : 0,
                           {}};
            require(x.type == "alsa" && std::regex_match(x.alsa_pcm, pcm), "invalid sink");
            x.endpoint = v.value("endpoint", std::string{});
            x.resource = v.value("resource", std::string{});
            if (v.contains("channel_start"))
                x.channel_start = range(v, "channel_start", 1, 65535);
            x.channel_count =
                v.contains("channel_count") ? range(v, "channel_count", 1, 16) : x.channels;
            require(x.channel_count == x.channels, "sink channel_count must equal channels");
            if (s.version == 1)
                require(v.at("mix").is_object(), "mix must be object");
            if (s.version == 1) for (auto m = v.at("mix").begin(); m != v.at("mix").end(); ++m) {
                require(std::regex_match(m.key(), ids), "invalid mix id");
                float g = gain(m.value());
                if (names.count(m.key()))
                    x.mix[m.key()] = g;
                else
                    s.warnings.push_back("unknown mix source: " + m.key());
            }
            if (x.id == s.engine.master && x.enabled)
                master = true;
            if (x.id == "hdmi")
                hdmi = true;
            s.sinks.push_back(x);
        }
        require((master && (s.version == 2 || hdmi)) ||
                    (s.version == 2 && s.engine.master.empty() && s.sources.empty() &&
                     s.sinks.empty() && j.at("routes").empty()),
                "master must name enabled sink; hdmi required");
        if (s.version == 2) {
            require(j.at("outputs").is_object() && j.at("routes").is_object(),
                    "outputs and routes must be objects");
            for (auto o = j["outputs"].begin(); o != j["outputs"].end(); ++o) {
                require(std::regex_match(o.key(), std::regex("^[1-8]$")), "invalid output number");
                require(o.value().at("enabled").is_boolean(), "output enabled must be boolean");
                s.outputs[std::stoi(o.key())] = o.value();
            }
            std::set<std::string> sink_ids;
            for (const auto &x : s.sinks)
                sink_ids.insert(x.id);
            for (auto r = j["routes"].begin(); r != j["routes"].end(); ++r) {
                const auto &v = r.value();
                require(std::regex_match(r.key(), ids), "invalid route id");
                RouteSettings route{r.key(),
                                    v.at("source").get<std::string>(),
                                    v.at("sink").get<std::string>(),
                                    range(v, "output", 1, 8),
                                    v.at("enabled").get<bool>(),
                                    gain(v.at("gain_db")),
                                    range(v, "delay_ms", 0, 5000)};
                require(v.value("mapping", std::string("default")) == "default",
                        "unsupported channel mapping");
                require(s.outputs.count(route.output) && names.count(route.source) &&
                            sink_ids.count(route.sink),
                        "route references missing output or endpoint");
                s.routes.push_back(route);
            }
        }
        return s;
    } catch (const std::exception &e) {
        if (error)
            *error = e.what();
        return {};
    }
}
bool AudioSettings::needs_restart(const AudioSettings &s) const {
    return engine.rate != s.engine.rate || engine.period_frames != s.engine.period_frames ||
           engine.master != s.engine.master;
}
json AudioSettings::to_json() const {
    json j = {{"version", version},
              {"engine",
               {{"enabled", engine.enabled},
                {"rate", engine.rate},
                {"period_frames", engine.period_frames},
                {"master", engine.master}}},
              {"sources", json::object()},
              {"sinks", json::object()},
              {"video_delay_ms", video_delay_ms}};
    if (version == 2 || revision)
        j["revision"] = revision;
    if (version == 2) {
        j["engine"]["memory_budget_mib"] = engine.memory_budget_mib;
        j["outputs"] = json::object();
        j["routes"] = json::object();
        for (const auto &[id, output] : outputs)
            j["outputs"][std::to_string(id)] = output;
        for (const auto &route : routes)
            j["routes"][route.id] = {{"output", route.output},   {"source", route.source},
                                     {"sink", route.sink},       {"enabled", route.enabled},
                                     {"gain_db", route.gain_db}, {"delay_ms", route.delay_ms}};
    }
    for (const auto &x : sources) {
        j["sources"][x.id] = {{"type", x.type},
                              {"enabled", x.enabled},
                              {"channels", x.channels},
                              {"delay_ms", x.delay_ms},
                              {"gain_db", x.gain_db}};
        if (version == 2) {
            j["sources"][x.id].erase("delay_ms");
            j["sources"][x.id].erase("gain_db");
            if (x.type == "ndi")
                j["sources"][x.id]["output"] = x.output;
        }
        if (!x.endpoint.empty())
            j["sources"][x.id]["endpoint"] = x.endpoint;
        if (!x.resource.empty()) {
            j["sources"][x.id]["resource"] = x.resource;
            j["sources"][x.id]["channel_start"] = x.channel_start;
            j["sources"][x.id]["channel_count"] = x.channel_count ? x.channel_count : x.channels;
        }
        if (!x.alsa_pcm.empty())
            j["sources"][x.id]["alsa_pcm"] = x.alsa_pcm;
    }
    for (const auto &x : sinks) {
        j["sinks"][x.id] = {{"type", x.type},         {"enabled", x.enabled},
                            {"channels", x.channels}, {"delay_ms", x.delay_ms},
                            {"alsa_pcm", x.alsa_pcm}, {"mix", x.mix}};
        if (version == 2) {
            j["sinks"][x.id].erase("delay_ms");
            j["sinks"][x.id].erase("mix");
        }
        if (!x.endpoint.empty())
            j["sinks"][x.id]["endpoint"] = x.endpoint;
        if (!x.resource.empty()) {
            j["sinks"][x.id]["resource"] = x.resource;
            j["sinks"][x.id]["channel_start"] = x.channel_start;
            j["sinks"][x.id]["channel_count"] = x.channel_count ? x.channel_count : x.channels;
        }
    }
    return j;
}
} // namespace ndimon::audio
