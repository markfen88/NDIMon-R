#pragma once
#include "AudioSettings.h"
#include "AudioSink.h"
#include "AudioSource.h"
#include <array>
#include <atomic>
#include <functional>
#include <memory>
#include <mutex>
#include <set>
#include <thread>
namespace ndimon::audio {
class AudioEngine {
  public:
    struct Factories {
        std::function<std::unique_ptr<AudioSource>(const SourceSettings &, int)> make_source;
        std::function<std::unique_ptr<AudioSink>(const SinkSettings &, bool, int)> make_sink;
        std::function<std::unique_ptr<AudioSource>(const SourceSettings &, int)> make_ndi;
    };
    struct Hooks {
        // Control-side observation point, invoked while the serialization lock is held.
        std::function<void()> after_reopen_snapshot;
        std::function<int64_t()> now_ns;
    };
    explicit AudioEngine(Factories f, Hooks hooks = {});
    ~AudioEngine();
    struct Refusal {
        std::string id, reason;
    };
    struct ApplyResult {
        bool ok;
        bool restarted;
        std::vector<std::string> applied;
        std::string error;
        bool restart_required = false;
        std::vector<Refusal> refused;
        std::string error_code;
        uint64_t requested_bytes = 0, budget_bytes = 0;
        nlohmann::json to_json() const {
            nlohmann::json entries = nlohmann::json::array();
            for (const auto &entry : refused)
                entries.push_back({{"id", entry.id}, {"reason", entry.reason}});
            nlohmann::json result = {{"ok", ok},
                                     {"restarted", restarted},
                                     {"applied", applied},
                                     {"error", error},
                                     {"restart_required", restart_required},
                                     {"refused", entries}};
            if (!error_code.empty()) {
                result["error_code"] = error_code;
                result["requested_bytes"] = requested_bytes;
                result["budget_bytes"] = budget_bytes;
            }
            return result;
        }
    };
    ApplyResult apply(const AudioSettings &s, int video_cap_ms = 5000);
    ApplyResult reload(const std::string &path, int video_cap_ms = 5000);
    void start();
    void stop();
    nlohmann::json status() const;
    void set_master_fallback(bool hdmi_present);
    void set_master_fallback(int output, bool hdmi_present);
    void set_primary_output(int output);
    bool needs_ndi_audio(int output) const;
    bool uses_output_graph() const;
    void set_settings_error(std::string error);
    void unregister_ndi();
    void unregister_ndi(int output);
    void push_ndi(const Block &in, int sample_rate);
    void push_ndi(int output, const Block &in, int sample_rate);

  private:
    struct Graph;
    ApplyResult apply_locked(const AudioSettings &s, int video_cap_ms, bool allow_partial = true);
    void run();
    void adopt(Graph *g);
    Factories factories_;
    Hooks hooks_;
    int64_t now_ns() const;
    std::unique_ptr<Graph> graph_;
    mutable std::mutex control_mutex_;
    std::atomic<Graph *> pending_{nullptr}, adopted_{nullptr};
    std::atomic<bool> running_{false}, hdmi_present_{true}, timer_active_{false},
        reopen_requested_{false};
    std::thread thread_;
    std::string settings_error_;
    bool reopen_ = false;
    std::set<std::string> reopen_sinks_, reopen_sources_;
    int64_t next_recovery_ns_ = 0;
    int primary_output_ = 1;
    std::array<bool, 9> ndi_registered_{{true, true, true, true, true, true, true, true, true}};
    int master_output_ = 0;
    std::array<bool, 9> display_present_{{true, true, true, true, true, true, true, true, true}};
    struct NdiRate {
        uint64_t mismatch_frames = 0;
        int sample_rate = kRate, mismatch_rate = 0;
    };
    std::array<NdiRate, 9> ndi_rates_{};
    struct PeriodBucket {
        std::atomic<uint64_t> count{0}, busy_ns{0}, wall_ns{0};
    };
    std::array<PeriodBucket, 64> histogram_{};
    std::atomic<uint64_t> periods_{0};
};
} // namespace ndimon::audio
