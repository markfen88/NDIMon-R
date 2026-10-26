#include "AudioEngine.h"
#include "AudioTiming.h"
#include "DelayLine.h"
#include "MixMatrix.h"
#include "TimerMaster.h"
#include "UnavailableAudio.h"
#include <chrono>
#include <cmath>
#include <sched.h>
#include <set>
#include <system_error>
namespace ndimon::audio {
namespace {
constexpr const char *kBusyReason = "device busy: structural change needs a core restart";
struct BusyRefusal : std::runtime_error {
    std::string id;
    explicit BusyRefusal(std::string endpoint)
        : std::runtime_error(kBusyReason), id(std::move(endpoint)) {}
};
struct ResourceRefusal : std::runtime_error {
    uint64_t requested, budget;
    ResourceRefusal(uint64_t r, uint64_t b)
        : std::runtime_error("audio graph requires " + std::to_string(r) + " bytes; budget " +
                             std::to_string(b) + " bytes"),
          requested(r), budget(b) {}
};
int hdmi_output(const SinkSettings &sink, int primary) {
    if (sink.resource.rfind("hdmi:", 0) == 0 && sink.resource.size() == 6 &&
        sink.resource[5] >= '1' && sink.resource[5] <= '8')
        return sink.resource[5] - '0';
    return sink.id == "hdmi" && sink.alsa_pcm == "auto" ? primary : 0;
}
uint64_t buffer_bytes(int channels, int frames) {
    return uint64_t(channels) * uint64_t(frames) * sizeof(float);
}
// Control-side recovery placeholder: the RT graph must stop borrowing the old
// exclusive handle before that handle can be destroyed and reopened.
struct ReleasedMaster final : AudioSink {
    std::string name;
    int channel_count;
    ReleasedMaster(std::string id, int channels) : name(std::move(id)), channel_count(channels) {}
    const std::string &id() const override { return name; }
    int channels() const override { return channel_count; }
    bool is_master() const override { return true; }
    bool placeholder() const override { return true; }
    bool wait_period(int) override { return false; }
    void push(const Block &) override {}
    nlohmann::json status() const override { return {{"ready", false}}; }
};
// Overlay only controls that can use the already-open endpoint graph.
AudioSettings live_subset(const AudioSettings &current, const AudioSettings &requested) {
    auto live = current;
    live.video_delay_ms = requested.video_delay_ms;
    for (auto &source : live.sources)
        for (const auto &next : requested.sources)
            if (source.id == next.id) {
                source.enabled = next.enabled;
                source.delay_ms = next.delay_ms;
                source.gain_db = next.gain_db;
            }
    for (auto &sink : live.sinks)
        for (const auto &next : requested.sinks)
            if (sink.id == next.id) {
                sink.enabled = next.enabled;
                sink.delay_ms = next.delay_ms;
                sink.mix = next.mix;
                // Retaining the old master requires that it remain enabled.
                if (sink.id == current.engine.master)
                    sink.enabled = true;
            }
    return live;
}
struct Buffer {
    std::vector<float> data;
    Block block;
    Buffer(int ch, int frames) : data(ch * frames) {
        block.channels = ch;
        block.frames = frames;
        for (int c = 0; c < ch; ++c)
            block.plane[c] = data.data() + c * frames;
    }
};
int64_t mono_ns() {
    timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return int64_t(t.tv_sec) * 1000000000 + t.tv_nsec;
}
} // namespace
struct AudioEngine::Graph {
    struct Source {
        SourceSettings settings;
        std::shared_ptr<AudioSource> device;
        std::shared_ptr<Buffer> buffer;
        std::shared_ptr<DelayLine> delay;
        bool selected = true;
    };
    struct Sink {
        SinkSettings settings;
        std::shared_ptr<AudioSink> device;
        std::shared_ptr<Buffer> buffer;
        std::shared_ptr<DelayLine> delay;
        std::vector<MixInput> inputs;
    };
    struct Route {
        RouteSettings settings;
        size_t source = 0, sink = 0;
        std::shared_ptr<Buffer> buffer;
        std::shared_ptr<DelayLine> delay;
        int capacity = 0;
        bool history_reset = false;
    };
    AudioSettings settings;
    std::vector<Source> sources;
    std::vector<Sink> sinks;
    std::vector<Route> routes;
    uint64_t memory_bytes = 0;
    TimerMaster timer;
    int master = -1;
    int video_cap_ms = 5000;
    explicit Graph(const AudioSettings &s) : settings(s), timer(s.engine.period_frames) {}
    void inputs() {
        for (auto &sink : sinks)
            sink.inputs.clear();
        if (settings.version == 1) {
            for (auto &sink : sinks)
                for (auto &source : sources) {
                    auto gain = sink.settings.mix.find(source.settings.id);
                    if (gain != sink.settings.mix.end())
                        sink.inputs.push_back(
                            {&source.buffer->block,
                             source.settings.enabled
                                 ? db_to_gain(gain->second) * db_to_gain(source.settings.gain_db)
                                 : 0.f});
                }
        } else {
            for (auto &source : sources)
                source.selected = false;
            for (auto &route : routes) {
                auto out = settings.outputs.find(route.settings.output);
                bool enabled = route.settings.enabled && out != settings.outputs.end() &&
                               out->second.value("enabled", false) &&
                               sources[route.source].settings.enabled;
                sinks[route.sink].inputs.push_back(
                    {&route.buffer->block, enabled ? db_to_gain(route.settings.gain_db) : 0.f});
                if (enabled && sinks[route.sink].settings.enabled)
                    sources[route.source].selected = true;
            }
        }
    }
};
AudioEngine::AudioEngine(Factories f, Hooks hooks)
    : factories_(std::move(f)), hooks_(std::move(hooks)) {}
int64_t AudioEngine::now_ns() const { return hooks_.now_ns ? hooks_.now_ns() : mono_ns(); }
AudioEngine::~AudioEngine() { stop(); }
void AudioEngine::adopt(Graph *g) {
    auto *old = adopted_.load(std::memory_order_acquire);
    for (auto &source : g->sources) {
        bool was_selected = false;
        if (old)
            for (const auto &prior : old->sources)
                if (prior.device == source.device)
                    was_selected = prior.selected;
        if (source.selected && !was_selected)
            source.device->discard_fallback();
    }
    for (auto &sink : g->sinks)
        for (auto &input : sink.inputs) {
            float start = 0;
            if (old)
                for (const auto &prior : old->sinks)
                    if (prior.settings.id == sink.settings.id)
                        for (const auto &p : prior.inputs)
                            if (p.block == input.block) {
                                float t = std::min(1.f, float(p.fade_offset) / 480);
                                start = p.start_gain < 0
                                            ? p.gain
                                            : p.start_gain + (p.gain - p.start_gain) * t;
                            }
            input.start_gain = start;
            input.fade_offset = 0;
        }
    for (auto &s : g->sources)
        s.delay->set_delay_frames(s.settings.delay_ms * kRate / 1000);
    for (auto &s : g->sinks)
        s.delay->set_delay_frames(s.settings.delay_ms * kRate / 1000);
    for (auto &r : g->routes)
        r.delay->set_delay_frames(r.settings.delay_ms * kRate / 1000);
    adopted_.store(g, std::memory_order_release);
}
AudioEngine::ApplyResult AudioEngine::reload(const std::string &path, int video_cap_ms) {
    std::lock_guard<std::mutex> lock(control_mutex_);
    auto settings = AudioSettings::load(path, &settings_error_);
    if (!settings_error_.empty() && graph_)
        return {false, false, {}, settings_error_};
    // Defaults are permitted only when constructing the initial graph.
    return apply_locked(settings, video_cap_ms);
}
AudioEngine::ApplyResult AudioEngine::apply(const AudioSettings &s, int video_cap_ms) {
    std::lock_guard<std::mutex> lock(control_mutex_);
    return apply_locked(s, video_cap_ms);
}
AudioEngine::ApplyResult AudioEngine::apply_locked(const AudioSettings &s, int video_cap_ms,
                                                   bool allow_partial) {
    ApplyResult result{true, graph_ && graph_->settings.needs_restart(s), {}, ""};
    try {
        const int period = s.engine.period_frames;
        if ((s.version != 1 && s.version != 2) || s.engine.rate != kRate ||
            !std::set<int>{128, 256, 512, 1024, 2048}.count(period) ||
            s.engine.memory_budget_mib < 1 || s.engine.memory_budget_mib > 512)
            throw std::runtime_error("invalid audio engine geometry or memory budget");
        std::map<std::string, const SourceSettings *> source_ids;
        std::map<std::string, const SinkSettings *> sink_ids;
        std::set<std::string> ndi_bindings;
        std::set<std::string> capture_bindings;
        uint64_t required = 0;
        for (const auto &x : s.sources) {
            if (x.channels < 1 || x.channels > kMaxChannels ||
                (x.type != "ndi" && x.type != "alsa") || x.output < 0 || x.output > 8 ||
                !source_ids.emplace(x.id, &x).second)
                throw std::runtime_error("invalid or duplicate source");
            if (x.channel_start < 1 || uint64_t(x.channel_start) + x.channels - 1 > 65535 ||
                (x.channel_count && x.channel_count != x.channels))
                throw std::runtime_error("invalid source channel range: " + x.id);
            if (s.version == 2 && x.type == "ndi" &&
                (x.output < 1 || x.output > 8 ||
                 !ndi_bindings.insert(std::to_string(x.output)).second))
                throw std::runtime_error("duplicate or invalid NDI output binding");
            if (s.version == 2 && x.type == "alsa") {
                const auto identity = x.resource.empty()
                                          ? "pcm:" + x.alsa_pcm
                                          : x.resource + ":" + std::to_string(x.channel_start) +
                                                ":" + std::to_string(x.channels);
                if (!capture_bindings.insert(identity).second)
                    throw std::runtime_error("duplicate canonical capture source: " + x.id);
            }
            required += buffer_bytes(x.channels, period + (s.version == 1 ? 5 * kRate : 0) + 1);
        }
        for (const auto &x : s.sinks) {
            if (x.channels < 1 || x.channels > kMaxChannels || x.type != "alsa" ||
                x.alsa_pcm.empty() || !sink_ids.emplace(x.id, &x).second)
                throw std::runtime_error("invalid or duplicate sink");
            if (x.channel_start < 1 || uint64_t(x.channel_start) + x.channels - 1 > 65535 ||
                (x.channel_count && x.channel_count != x.channels))
                throw std::runtime_error("invalid sink channel range: " + x.id);
            if (s.version == 2)
                for (const auto &prior : s.sinks) {
                    if (&prior == &x)
                        break;
                    const bool same_pcm =
                        x.alsa_pcm == prior.alsa_pcm &&
                        !(x.alsa_pcm == "auto" && hdmi_output(x, primary_output_) &&
                          hdmi_output(prior, primary_output_) && x.resource != prior.resource);
                    const bool overlap = !x.resource.empty() && x.resource == prior.resource &&
                                         x.channel_start < prior.channel_start + prior.channels &&
                                         prior.channel_start < x.channel_start + x.channels;
                    if (same_pcm || overlap)
                        throw std::runtime_error("sink " + x.id + " overlaps canonical sink " +
                                                 prior.id + "; share that sink ID instead");
                }
            required += buffer_bytes(x.channels, period + (s.version == 1 ? 5 * kRate : 0) + 1);
        }
        std::map<std::pair<std::string, std::string>, int> contributions;
        std::map<std::string, int> route_capacities;
        std::set<std::string> route_ids;
        auto master_setting = sink_ids.find(s.engine.master);
        if (!(s.version == 2 && s.engine.master.empty() && s.sources.empty() && s.sinks.empty() &&
              s.routes.empty()) &&
            (master_setting == sink_ids.end() ||
             (s.version == 2 && !master_setting->second->enabled)))
            throw std::runtime_error("master must name enabled sink");
        for (const auto &r : s.routes) {
            auto source = source_ids.find(r.source);
            auto sink = sink_ids.find(r.sink);
            if (!route_ids.insert(r.id).second || source == source_ids.end() ||
                sink == sink_ids.end() || !s.outputs.count(r.output) || r.delay_ms < 0 ||
                r.delay_ms > 5000 || !std::isfinite(r.gain_db) || r.gain_db < -60 || r.gain_db > 12)
                throw std::runtime_error("invalid route: " + r.id);
            if (r.enabled && s.outputs.at(r.output).value("enabled", false) &&
                source->second->enabled && sink->second->enabled) {
                auto key = std::make_pair(r.source, r.sink);
                auto inserted = contributions.emplace(key, r.output);
                if (!inserted.second)
                    throw std::runtime_error(
                        "duplicate source/sink contribution on output " + std::to_string(r.output) +
                        "; already used by output " + std::to_string(inserted.first->second));
            }
            int capacity = r.delay_ms * kRate / 1000;
            if (graph_ && !result.restarted)
                for (const auto &old_route : graph_->routes) {
                    if (old_route.settings.id == r.id && old_route.settings.source == r.source &&
                        old_route.settings.sink == r.sink &&
                        old_route.buffer->block.channels == source->second->channels &&
                        old_route.capacity >= capacity) {
                        capacity = old_route.capacity;
                        break;
                    }
                }
            route_capacities[r.id] = capacity;
            required += buffer_bytes(source->second->channels, period + capacity + 1);
        }
        const uint64_t budget = uint64_t(s.engine.memory_budget_mib) * 1024 * 1024;
        if (s.version == 2 && required > budget)
            throw ResourceRefusal(required, budget);
        // The previous graph remains published until every factory and buffer
        // allocation succeeds. An exclusive-device factory may refuse a change;
        // that refusal must never replace working playback with a transition.
        auto next = std::make_unique<Graph>(s);
        next->memory_bytes = required;
        next->video_cap_ms = std::clamp(video_cap_ms, 0, 5000);
        int p = s.engine.period_frames;
        auto old = graph_ ? graph_->settings.to_json() : nlohmann::json::object();
        auto doc = s.to_json();
        for (const auto *section : {"sources", "sinks"})
            for (auto it = doc[section].begin(); it != doc[section].end(); ++it) {
                if (!old.contains(section) || !old[section].contains(it.key())) {
                    result.applied.push_back(std::string(section) + "." + it.key());
                    continue;
                }
                for (auto field = it.value().begin(); field != it.value().end(); ++field)
                    if (!old[section][it.key()].contains(field.key()) ||
                        old[section][it.key()][field.key()] != field.value())
                        result.applied.push_back(std::string(section) + "." + it.key() + "." +
                                                 field.key());
            }
        for (const auto *section : {"sources", "sinks"}) {
            if (!old.contains(section))
                continue;
            for (auto it = old[section].begin(); it != old[section].end(); ++it)
                if (!doc[section].contains(it.key()))
                    result.applied.push_back(std::string(section) + "." + it.key());
        }
        if (old.contains("video_delay_ms") && old["video_delay_ms"] != s.video_delay_ms)
            result.applied.push_back("video_delay_ms");
        if (result.restarted)
            result.applied.push_back("engine");
        if (s.version == 2) {
            for (const auto *field : {"routes", "outputs"})
                if (old.value(field, nlohmann::json::object()) != doc[field])
                    result.applied.push_back(field);
        }
        for (const auto &x : s.sources) {
            if (x.type == "ndi" && !ndi_registered_[x.output ? x.output : primary_output_])
                continue;
            Graph::Source node;
            node.settings = x;
            if (graph_ && !result.restarted && !reopen_sources_.count(x.id))
                for (const auto &prior : graph_->sources)
                    if (!prior.device->placeholder() && prior.settings.id == x.id &&
                        prior.settings.type == x.type && prior.settings.channels == x.channels &&
                        prior.settings.output == x.output &&
                        prior.settings.resource == x.resource &&
                        prior.settings.channel_start == x.channel_start &&
                        (prior.settings.channel_count ? prior.settings.channel_count
                                                      : prior.settings.channels) ==
                            (x.channel_count ? x.channel_count : x.channels) &&
                        prior.settings.alsa_pcm == x.alsa_pcm) {
                        node.device = prior.device;
                        node.buffer = prior.buffer;
                        if (s.version == graph_->settings.version)
                            node.delay = prior.delay;
                        break;
                    }
            const bool candidate = !node.device;
            if (candidate) {
                try {
                    node.device =
                        x.type == "ndi" ? factories_.make_ndi(x, p) : factories_.make_source(x, p);
                } catch (const std::system_error &error) {
                    if (error.code().value() == EBUSY)
                        throw BusyRefusal(x.id);
                    throw;
                }
                node.buffer = std::make_shared<Buffer>(x.channels, p);
            }
            if (!node.delay)
                node.delay =
                    std::make_shared<DelayLine>(x.channels, s.version == 1 ? 5 * kRate : 0);
            if (!node.device)
                throw std::runtime_error("source factory returned null: " + x.id);
            if (candidate && graph_ && !reopen_ &&
                node.device->status().value("error_code", 0) == -EBUSY)
                throw BusyRefusal(x.id);
            if (candidate && graph_ && !reopen_ && !node.device->ready())
                for (const auto &prior : graph_->sources)
                    if (prior.settings.id == x.id && prior.device->ready())
                        throw std::runtime_error("source unavailable: " + x.id);
            next->sources.push_back(std::move(node));
        }
        for (const auto &x : s.sinks) {
            Graph::Sink node;
            node.settings = x;
            bool master = x.id == s.engine.master;
            if (graph_ && !result.restarted && !reopen_sinks_.count(x.id))
                for (const auto &prior : graph_->sinks)
                    if (!prior.device->placeholder() && prior.settings.id == x.id &&
                        prior.settings.type == x.type && prior.settings.channels == x.channels &&
                        prior.settings.resource == x.resource &&
                        prior.settings.channel_start == x.channel_start &&
                        (prior.settings.channel_count ? prior.settings.channel_count
                                                      : prior.settings.channels) ==
                            (x.channel_count ? x.channel_count : x.channels) &&
                        prior.settings.alsa_pcm == x.alsa_pcm) {
                        node.device = prior.device;
                        node.buffer = prior.buffer;
                        if (s.version == graph_->settings.version)
                            node.delay = prior.delay;
                        break;
                    }
            const bool candidate = !node.device;
            if (candidate) {
                try {
                    node.device = factories_.make_sink(x, master, p);
                } catch (const std::system_error &error) {
                    if (error.code().value() == EBUSY)
                        throw BusyRefusal(x.id);
                    throw;
                }
                node.buffer = std::make_shared<Buffer>(x.channels, p);
            }
            if (!node.delay)
                node.delay =
                    std::make_shared<DelayLine>(x.channels, s.version == 1 ? 5 * kRate : 0);
            if (!node.device)
                throw std::runtime_error("sink factory returned null: " + x.id);
            if (candidate && graph_ && !reopen_ &&
                node.device->status().value("error_code", 0) == -EBUSY)
                throw BusyRefusal(x.id);
            if (candidate && graph_ && !reopen_ && !node.device->status().value("ready", false))
                for (const auto &prior : graph_->sinks)
                    if (prior.settings.id == x.id && prior.device->status().value("ready", false))
                        throw std::runtime_error("sink unavailable: " + x.id);
            for (auto &source : next->sources) {
                auto gain = x.mix.find(source.settings.id);
                if (gain != x.mix.end())
                    node.inputs.push_back(
                        {&source.buffer->block,
                         source.settings.enabled
                             ? db_to_gain(gain->second) * db_to_gain(source.settings.gain_db)
                             : 0.f});
            }
            if (master && x.enabled)
                next->master = int(next->sinks.size());
            next->sinks.push_back(std::move(node));
        }
        for (const auto &r : s.routes) {
            auto source = std::find_if(next->sources.begin(), next->sources.end(),
                                       [&](const auto &x) { return x.settings.id == r.source; });
            auto sink = std::find_if(next->sinks.begin(), next->sinks.end(),
                                     [&](const auto &x) { return x.settings.id == r.sink; });
            if (source == next->sources.end())
                continue; // receiver revoked before teardown
            if (sink == next->sinks.end())
                throw std::runtime_error("route sink unavailable");
            Graph::Route route;
            route.settings = r;
            route.source = source - next->sources.begin();
            route.sink = sink - next->sinks.begin();
            route.capacity = route_capacities.at(r.id);
            if (graph_ && !result.restarted)
                for (const auto &old_route : graph_->routes) {
                    if (old_route.settings.id == r.id && old_route.settings.source == r.source &&
                        old_route.settings.sink == r.sink && old_route.capacity == route.capacity &&
                        old_route.buffer->block.channels == source->settings.channels) {
                        route.buffer = old_route.buffer;
                        route.delay = old_route.delay;
                        break;
                    }
                }
            if (!route.buffer)
                route.buffer = std::make_shared<Buffer>(source->settings.channels, p);
            if (!route.delay) {
                route.history_reset = true;
                route.delay =
                    std::make_shared<DelayLine>(source->settings.channels, route.capacity);
            }
            next->routes.push_back(std::move(route));
        }
        next->inputs();
        // Control thread owns both graphs until the RT thread acknowledges the swap.
        if (running_) {
            pending_.store(next.get(), std::memory_order_release);
            while (adopted_.load(std::memory_order_acquire) != next.get())
                std::this_thread::sleep_for(std::chrono::microseconds(100));
        } else
            adopt(next.get());
        graph_ = std::move(next);
        master_output_ = 0;
        if (graph_->master >= 0)
            master_output_ = hdmi_output(graph_->sinks[graph_->master].settings, primary_output_);
        hdmi_present_ = !master_output_ || display_present_[master_output_];
        reopen_ = false;
    } catch (const BusyRefusal &refusal) {
        result.ok = false;
        result.restarted = false;
        result.restart_required = true;
        result.error = kBusyReason;
        result.applied.clear();
        result.refused = {{refusal.id, kBusyReason}};
        reopen_ = false;
        if (graph_ && allow_partial && s.version == 1) {
            auto live = live_subset(graph_->settings, s);
            auto partial = apply_locked(live, video_cap_ms, false);
            if (partial.ok)
                result.applied = partial.applied;
            else
                result.error += "; live apply failed: " + partial.error;
            auto effective = graph_->settings.to_json();
            auto requested = s.to_json();
            for (const auto *section : {"sources", "sinks"}) {
                std::set<std::string> ids;
                for (auto it = effective[section].begin(); it != effective[section].end(); ++it)
                    ids.insert(it.key());
                for (auto it = requested[section].begin(); it != requested[section].end(); ++it)
                    ids.insert(it.key());
                for (const auto &id : ids) {
                    bool deferred =
                        !effective[section].contains(id) || !requested[section].contains(id);
                    if (!deferred)
                        for (const auto *key : {"type", "channels", "alsa_pcm", "enabled"}) {
                            if (effective[section][id].value(key, nlohmann::json{}) !=
                                requested[section][id].value(key, nlohmann::json{}))
                                deferred = true;
                        }
                    bool listed = std::any_of(result.refused.begin(), result.refused.end(),
                                              [&](const Refusal &entry) { return entry.id == id; });
                    if (deferred && !listed)
                        result.refused.push_back({id, "structural change deferred because a device "
                                                      "is busy; needs a core restart"});
                }
            }
        }
    } catch (const ResourceRefusal &e) {
        result.ok = false;
        result.restarted = false;
        result.applied.clear();
        result.error = e.what();
        result.error_code = "resource_exhausted";
        result.requested_bytes = e.requested;
        result.budget_bytes = e.budget;
        reopen_ = false;
    } catch (const std::exception &e) {
        result.ok = false;
        result.restarted = false;
        result.applied.clear();
        result.error = e.what();
        reopen_ = false;
    }
    return result;
}
void AudioEngine::start() {
    std::lock_guard<std::mutex> lock(control_mutex_);
    if (running_ || !graph_)
        return;
    running_ = true;
    thread_ = std::thread(&AudioEngine::run, this);
}
void AudioEngine::stop() {
    std::lock_guard<std::mutex> lock(control_mutex_);
    running_ = false;
    if (thread_.joinable())
        thread_.join();
}
void AudioEngine::set_master_fallback(bool present) {
    set_master_fallback(primary_output_, present);
}
void AudioEngine::set_primary_output(int output) {
    std::lock_guard<std::mutex> lock(control_mutex_);
    if (output < 1 || output > 8 || graph_)
        throw std::runtime_error(
            "primary audio output must be selected before configuring the engine");
    primary_output_ = output;
}
void AudioEngine::set_master_fallback(int output, bool output_present) {
    std::lock_guard<std::mutex> lock(control_mutex_);
    if (output < 1 || output > 8)
        return;
    const bool output_was_present = display_present_[output];
    display_present_[output] = output_present;
    bool present = !master_output_ || display_present_[master_output_];
    bool prior = hdmi_present_.exchange(present);
    if (!graph_)
        return;
    const bool hdmi_master = master_output_ != 0;
    const bool requested = reopen_requested_.exchange(false);
    const auto now = now_ns();
    const bool retry_due = now >= next_recovery_ns_;
    std::set<std::string> targets, source_targets;
    if (present && ((requested && retry_due) || (hdmi_master && !prior)) && graph_->master >= 0)
        targets.insert(graph_->sinks[graph_->master].settings.id);
    if (output_present && !output_was_present)
        for (const auto &sink : graph_->sinks)
            if (sink.settings.enabled && sink.settings.id != graph_->settings.engine.master &&
                hdmi_output(sink.settings, primary_output_) == output &&
                !sink.device->status().value("ready", false))
                targets.insert(sink.settings.id);
    if (retry_due && graph_->settings.engine.enabled) {
        next_recovery_ns_ = now + 2000000000LL;
        for (const auto &source : graph_->sources)
            if (source.settings.enabled && source.selected && !source.device->ready())
                source_targets.insert(source.settings.id);
        for (const auto &sink : graph_->sinks) {
            const int hdmi = hdmi_output(sink.settings, primary_output_);
            if (sink.settings.enabled && (!hdmi || display_present_[hdmi]) &&
                !sink.device->status().value("ready", false))
                targets.insert(sink.settings.id);
        }
    }
    if (targets.empty() && source_targets.empty())
        return;
    auto settings = graph_->settings;
    int cap = graph_->video_cap_ms;
    if (hooks_.after_reopen_snapshot)
        hooks_.after_reopen_snapshot();
    {
        auto transition = std::make_unique<Graph>(settings);
        transition->video_cap_ms = cap;
        transition->sources = graph_->sources;
        for (auto &source : transition->sources)
            if (source_targets.count(source.settings.id))
                source.device = std::make_shared<UnavailableSource>(
                    source.settings.id, source.settings.channels, "endpoint recovery pending");
        transition->routes = graph_->routes;
        transition->memory_bytes = graph_->memory_bytes;
        // Rebuild inputs from immutable settings: the RT thread mutates each
        // prior input's fade offset, so copying that vector here would race it.
        for (const auto &prior : graph_->sinks) {
            Graph::Sink node;
            node.settings = prior.settings;
            node.device = prior.device;
            node.buffer = prior.buffer;
            node.delay = prior.delay;
            for (auto &source : transition->sources) {
                auto gain = node.settings.mix.find(source.settings.id);
                if (gain != node.settings.mix.end())
                    node.inputs.push_back(
                        {&source.buffer->block,
                         source.settings.enabled
                             ? db_to_gain(gain->second) * db_to_gain(source.settings.gain_db)
                             : 0.f});
            }
            transition->sinks.push_back(std::move(node));
        }
        transition->inputs();
        transition->master = graph_->master;
        for (auto &sink : transition->sinks)
            if (targets.count(sink.settings.id))
                sink.device =
                    std::make_shared<ReleasedMaster>(sink.settings.id, sink.settings.channels);
        if (running_) {
            pending_.store(transition.get(), std::memory_order_release);
            while (adopted_.load(std::memory_order_acquire) != transition.get())
                std::this_thread::sleep_for(std::chrono::microseconds(100));
        } else
            adopt(transition.get());
        graph_ = std::move(transition); // closes the old master after RT acknowledgement
    }
    reopen_ = true;
    reopen_sinks_ = std::move(targets);
    reopen_sources_ = std::move(source_targets);
    auto recovery = apply_locked(settings, cap, false);
    if (!recovery.ok)
        settings_error_ = "audio endpoint recovery failed: " + recovery.error;
    else if (settings_error_.rfind("audio endpoint recovery failed: ", 0) == 0)
        settings_error_.clear();
    reopen_sinks_.clear();
    reopen_sources_.clear();
}
void AudioEngine::unregister_ndi() { unregister_ndi(primary_output_); }
void AudioEngine::unregister_ndi(int output) {
    std::lock_guard<std::mutex> lock(control_mutex_);
    if (output < 1 || output > 8 || !ndi_registered_[output])
        return;
    ndi_registered_[output] = false;
    if (!graph_)
        return;
    auto settings = graph_->settings;
    auto result = apply_locked(settings, graph_->video_cap_ms);
    if (!result.ok) { // Even an allocation failure must not leave a borrowed receiver callable.
        settings_error_ =
            "audio stopped while revoking output " + std::to_string(output) + ": " + result.error;
        running_ = false;
        if (thread_.joinable())
            thread_.join();
    }
}
bool AudioEngine::needs_ndi_audio(int output) const {
    std::lock_guard<std::mutex> lock(control_mutex_);
    if (!graph_ || !graph_->settings.engine.enabled || output < 1 || output > 8 ||
        !ndi_registered_[output])
        return false;
    const auto &settings = graph_->settings;
    for (const auto &source : settings.sources) {
        if (source.type != "ndi" || !source.enabled ||
            (source.output ? source.output : primary_output_) != output)
            continue;
        if (settings.version == 1)
            return true;
        for (const auto &route : settings.routes) {
            auto out = settings.outputs.find(route.output);
            auto sink = std::find_if(settings.sinks.begin(), settings.sinks.end(),
                                     [&](const auto &x) { return x.id == route.sink; });
            if (route.source == source.id && route.enabled && out != settings.outputs.end() &&
                out->second.value("enabled", false) && sink != settings.sinks.end() &&
                sink->enabled)
                return true;
        }
    }
    return false;
}
bool AudioEngine::uses_output_graph() const {
    std::lock_guard<std::mutex> lock(control_mutex_);
    return graph_ && graph_->settings.version == 2;
}
void AudioEngine::set_settings_error(std::string e) {
    std::lock_guard<std::mutex> lock(control_mutex_);
    settings_error_ = std::move(e);
}
void AudioEngine::run() {
    sched_param priority{};
    priority.sched_priority = 60;
    pthread_setschedparam(pthread_self(), SCHED_FIFO, &priority);
    Graph *g = adopted_.load(std::memory_order_acquire);
    int64_t previous = now_ns();
    while (running_) {
        if (auto *next = pending_.exchange(nullptr, std::memory_order_acq_rel)) {
            g = next;
            adopt(g);
        }
        auto *master = g->master >= 0 ? g->sinks[g->master].device.get() : nullptr;
        bool real =
            g->settings.engine.enabled && hdmi_present_ && master &&
            // Two periods, rounded up: 512 frames is 10.67 ms, and a truncated
            // 10 ms timeout made the HDMI master fall back to the timer.
            master->wait_period(std::max(1, master_wait_ms(g->settings.engine.period_frames, kRate)));
        timer_active_ = !real;
        if (!real) {
            if (master && g->settings.engine.enabled && hdmi_present_)
                reopen_requested_ = true;
            g->timer.wait_period(0);
        }
        int64_t busy_start = now_ns();
        for (auto &source : g->sources) {
            auto &b = source.buffer->block;
            if (g->settings.engine.enabled && source.settings.enabled && source.selected &&
                source.device->ready())
                source.device->pull(b);
            else
                b.silence();
            source.delay->process(b, b);
        }
        for (auto &route : g->routes)
            route.delay->process(g->sources[route.source].buffer->block, route.buffer->block);
        for (size_t i = 0; i < g->sinks.size(); ++i) {
            auto &sink = g->sinks[i];
            if (!sink.settings.enabled)
                continue;
            auto &b = sink.buffer->block;
            mix(sink.inputs, b, false);
            for (auto &input : sink.inputs)
                input.fade_offset = std::min(480, input.fade_offset + b.frames);
            sink.delay->process(b, b);
            if (int(i) != g->master)
                sink.device->push(b);
        }
        int64_t busy_end = now_ns();
        if (real && master && g->settings.engine.enabled)
            master->push(g->sinks[g->master].buffer->block);
        int64_t now = now_ns();
        auto wall = std::max<int64_t>(0, now - previous);
        auto us = wall / 1000;
        previous = now;
        auto &bucket = histogram_[std::clamp<int64_t>(us / 1000, 0, 63)];
        bucket.busy_ns.fetch_add(std::max<int64_t>(0, busy_end - busy_start),
                                 std::memory_order_relaxed);
        bucket.wall_ns.fetch_add(wall, std::memory_order_relaxed);
        bucket.count.fetch_add(1, std::memory_order_relaxed);
        ++periods_;
    }
}
nlohmann::json AudioEngine::status() const {
    std::lock_guard<std::mutex> lock(control_mutex_);
    nlohmann::json j = {{"running", running_.load()},
                        {"settings_error", settings_error_},
                        {"sources", nlohmann::json::object()},
                        {"sinks", nlohmann::json::object()},
                        {"xruns", 0},
                        {"cpu_percent", 0.0}};
    if (!graph_)
        return j;
    const auto &s = graph_->settings;
    j["settings"] = s.to_json();
    j["primary_output"] = primary_output_;
    j["memory_bytes"] = graph_->memory_bytes;
    j["memory_budget_bytes"] = uint64_t(s.engine.memory_budget_mib) * 1024 * 1024;
    j["routes"] = nlohmann::json::object();
    j["outputs"] = nlohmann::json::object();
    for (const auto &[id, output] : s.outputs)
        j["outputs"][std::to_string(id)] = output;
    for (const auto &route : graph_->routes)
        j["routes"][route.settings.id] = {
            {"output", route.settings.output},
            {"source", route.settings.source},
            {"sink", route.settings.sink},
            {"enabled", route.settings.enabled},
            {"delay_ms", route.settings.delay_ms},
            {"delay_capacity_frames", route.capacity},
            {"history_reset", route.history_reset},
            {"mapping", "default"},
            {"source_channels", graph_->sources[route.source].settings.channels},
            {"sink_channels", graph_->sinks[route.sink].settings.channels}};
    j["master"] = timer_active_ ? "timer" : s.engine.master;
    j["rate"] = s.engine.rate;
    j["period_frames"] = s.engine.period_frames;
    j["video_delay_ms"] = s.video_delay_ms;
    j["requested_video_delay_ms"] = s.video_delay_ms;
    j["effective_video_delay_ms"] = std::min(s.video_delay_ms, graph_->video_cap_ms);
    j["video_delay_capped"] = s.video_delay_ms > graph_->video_cap_ms;
    uint64_t total = 0, sum = 0;
    uint64_t busy_ns = 0, wall_ns = 0;
    for (const auto &b : histogram_) {
        total += b.count.load();
        busy_ns += b.busy_ns.load();
        wall_ns += b.wall_ns.load();
    }
    j["cpu_percent"] = wall_ns ? std::clamp(100.0 * busy_ns / wall_ns, 0.0, 100.0) : 0.0;
    for (int i = 0; i < 64; ++i) {
        sum += histogram_[i].count.load();
        if (sum * 100 >= total * 99) {
            j["period_us_p99"] = (i + 1) * 1000;
            break;
        }
    }
    for (const auto &x : graph_->sources) {
        auto v = x.device->status();
        v["channels"] = x.settings.channels;
        if (!x.settings.endpoint.empty())
            v["endpoint"] = x.settings.endpoint;
        v["delay_ms"] = x.settings.delay_ms;
        v["enabled"] = x.settings.enabled;
        if (x.settings.type == "ndi") {
            const auto &rate = ndi_rates_[x.settings.output ? x.settings.output : primary_output_];
            v["output"] = x.settings.output ? x.settings.output : primary_output_;
            v["rate_mismatch_frames"] = rate.mismatch_frames;
            v["rate_mismatch"] = rate.sample_rate != kRate;
            v["rate_mismatch_rate"] = rate.mismatch_rate;
            v["sample_rate"] = rate.sample_rate;
        }
        j["sources"][x.settings.id] = v;
    }
    uint64_t xruns = 0;
    for (const auto &x : graph_->sinks) {
        auto v = x.device->status();
        v["channels"] = x.settings.channels;
        if (!x.settings.resource.empty())
            v["resource"] = x.settings.resource;
        if (!x.settings.endpoint.empty())
            v["endpoint"] = x.settings.endpoint;
        v["delay_ms"] = x.settings.delay_ms;
        v["enabled"] = x.settings.enabled;
        xruns += v.value("xruns", 0);
        j["sinks"][x.settings.id] = v;
    }
    j["xruns"] = xruns;
    j["periods"] = periods_.load();
    return j;
}
} // namespace ndimon::audio

namespace ndimon::audio {
void AudioEngine::push_ndi(const Block &in, int rate) { push_ndi(primary_output_, in, rate); }
void AudioEngine::push_ndi(int output, const Block &in, int rate) {
    std::lock_guard<std::mutex> lock(control_mutex_);
    if (!graph_ || output < 1 || output > 8)
        return;
    for (auto &source : graph_->sources)
        if (source.settings.type == "ndi" && source.settings.enabled && source.selected &&
            (source.settings.output ? source.settings.output : primary_output_) == output) {
            auto &state = ndi_rates_[output];
            state.sample_rate = rate;
            if (rate != kRate) {
                source.device->set_fallback_valid(false);
                state.mismatch_frames += std::max(0, in.frames);
                state.mismatch_rate = rate;
                return;
            }
            source.device->set_fallback_valid(true);
            float storage[kMaxChannels * 2048];
            Block out;
            out.channels = source.settings.channels;
            for (int c = 0; c < out.channels; ++c)
                out.plane[c] = storage + c * 2048;
            std::vector<MixInput> inputs(1);
            Block slice = in;
            inputs[0] = {&slice, 1};
            for (int offset = 0; offset < in.frames; offset += 2048) {
                slice.frames = out.frames = std::min(2048, in.frames - offset);
                for (int c = 0; c < in.channels; ++c)
                    slice.plane[c] = in.plane[c] + offset;
                mix(inputs, out, false);
                const float *planes[kMaxChannels]{};
                for (int c = 0; c < out.channels; ++c)
                    planes[c] = out.plane[c];
                source.device->push_fifo(planes, out.frames);
            }
        }
}
} // namespace ndimon::audio
