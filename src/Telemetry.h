#pragma once
// Opt-in per-second diagnostics for root-causing SDK receive drops.
// Enabled by NDIMON_TELEMETRY_CSV=/path/prefix (one file per output: prefix-chN.csv).
// Everything here is cheap (a mutex and a few integers per FrameSync pull) and writes
// nothing unless the variable is set.
#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <mutex>
#include <string>

class DropTelemetry {
public:
    static bool enabled() { return std::getenv("NDIMON_TELEMETRY_CSV") != nullptr; }

    explicit DropTelemetry(int ch) : ch_(ch) {
        if (const char* p = std::getenv("NDIMON_TELEMETRY_CSV"))
            path_ = std::string(p) + "-ch" + std::to_string(ch) + ".csv";
    }

    static int64_t wall_100ns() {
        return std::chrono::duration_cast<std::chrono::nanoseconds>(
                   std::chrono::system_clock::now().time_since_epoch()).count() / 100;
    }
    static int64_t mono_us() {
        return std::chrono::duration_cast<std::chrono::microseconds>(
                   std::chrono::steady_clock::now().time_since_epoch()).count();
    }

    // One FrameSync pull; ts is the NDI frame timestamp (sender clock, 100 ns).
    void on_pull(int64_t ts) {
        std::lock_guard<std::mutex> lk(m_);
        ++pulls_;
        if (ts == 0 || ts == last_ts_) return;
        ++new_frames_;
        const double age_ms = double(wall_100ns() - ts) / 10000.0;
        age_min_ = std::min(age_min_, age_ms);
        age_max_ = std::max(age_max_, age_ms);
        age_sum_ += age_ms;
        if (last_ts_ != 0) ts_gap_max_ms_ = std::max(ts_gap_max_ms_, double(ts - last_ts_) / 10000.0);
        last_ts_ = ts;
    }
    void on_queue_drop() { std::lock_guard<std::mutex> lk(m_); ++queue_drops_; }
    void on_render(int64_t queue_wait_us, int64_t render_us) {
        std::lock_guard<std::mutex> lk(m_);
        ++renders_;
        render_sum_us_ += render_us;
        render_max_us_ = std::max(render_max_us_, render_us);
        qwait_max_us_ = std::max(qwait_max_us_, queue_wait_us);
        const int64_t now = mono_us();
        if (last_render_us_) flip_gap_max_us_ = std::max(flip_gap_max_us_, now - last_render_us_);
        last_render_us_ = now;
    }

    // Called from the worker tick (500 ms). Writes at most one row per second.
    void emit(int sdk_total, int sdk_dropped, int sdk_vq) {
        if (path_.empty()) return;
        const int64_t now = mono_us();
        if (now - last_emit_us_ < 1000000) return;
        last_emit_us_ = now;
        std::lock_guard<std::mutex> lk(m_);
        FILE* f = std::fopen(path_.c_str(), "a");
        if (!f) return;
        if (!header_) {
            std::fputs("wall_ms,ch,sdk_total,sdk_dropped,sdk_video_queue,pulls,new_frames,ts_gap_max_ms,"
                       "age_min_ms,age_avg_ms,age_max_ms,queue_drops,renders,render_avg_ms,render_max_ms,"
                       "queue_wait_max_ms,flip_gap_max_ms\n", f);
            header_ = true;
        }
        const double age_avg = new_frames_ ? age_sum_ / new_frames_ : 0.0;
        std::fprintf(f, "%lld,%d,%d,%d,%d,%lld,%lld,%.2f,%.2f,%.2f,%.2f,%lld,%lld,%.3f,%.3f,%.3f,%.3f\n",
                     (long long)(wall_100ns() / 10000), ch_, sdk_total, sdk_dropped, sdk_vq,
                     (long long)pulls_, (long long)new_frames_, ts_gap_max_ms_,
                     new_frames_ ? age_min_ : 0.0, age_avg, new_frames_ ? age_max_ : 0.0,
                     (long long)queue_drops_, (long long)renders_,
                     renders_ ? render_sum_us_ / 1000.0 / renders_ : 0.0, render_max_us_ / 1000.0,
                     qwait_max_us_ / 1000.0, flip_gap_max_us_ / 1000.0);
        std::fclose(f);
        pulls_ = new_frames_ = queue_drops_ = renders_ = 0;
        ts_gap_max_ms_ = age_sum_ = 0; age_min_ = 1e18; age_max_ = -1e18;
        render_sum_us_ = render_max_us_ = qwait_max_us_ = flip_gap_max_us_ = 0;
    }

private:
    int ch_;
    std::string path_;
    std::mutex m_;
    bool header_ = false;
    int64_t last_emit_us_ = 0, last_ts_ = 0, last_render_us_ = 0;
    int64_t pulls_ = 0, new_frames_ = 0, queue_drops_ = 0, renders_ = 0;
    double ts_gap_max_ms_ = 0, age_sum_ = 0, age_min_ = 1e18, age_max_ = -1e18;
    int64_t render_sum_us_ = 0, render_max_us_ = 0, qwait_max_us_ = 0, flip_gap_max_us_ = 0;
};
