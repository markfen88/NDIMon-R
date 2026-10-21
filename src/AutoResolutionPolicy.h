#pragma once
#include "ResolutionControl.h"

#include <atomic>
#include <chrono>
#include <cstdint>
#include <mutex>
#include <numeric>
#include <optional>

struct SourceModeKey {
    uint32_t width, height;
    int rate_n, rate_d;

    static SourceModeKey make(uint32_t width, uint32_t height, int n, int d) {
        if (n <= 0 || d <= 0) return {width, height, 0, 0};
        const int divisor = std::gcd(n, d);
        return {width, height, n / divisor, d / divisor};
    }
    bool operator==(const SourceModeKey& other) const {
        return width == other.width && height == other.height &&
               rate_n == other.rate_n && rate_d == other.rate_d;
    }
};

// Mode callbacks may take DRM's frame mutex or Config's leaf lock, but must
// never acquire decoder_mutex_: compressed receive already holds it here.
class AutoResolutionPolicy {
public:
    using Clock = std::chrono::steady_clock;

    explicit AutoResolutionPolicy(bool enabled) : enabled_(enabled) {}
    bool enabled() const { return enabled_.load(); }

    template<typename Apply, typename Persist>
    ResolutionResult set_manual(Apply apply, Persist persist, int64_t deadline_monotonic_ms = 0) {
        std::unique_lock<std::mutex> lock(mutex_, std::defer_lock);
        auto result = lock_resolution_control(lock, deadline_monotonic_ms);
        if (result != ResolutionResult::Applied) return result;
        result = apply();
        if (result != ResolutionResult::Applied) return result;
        enabled_ = false;
        matched_.reset();
        failed_.reset();
        persist();
        return ResolutionResult::Applied;
    }

    template<typename Persist>
    ResolutionResult enable_auto(Persist persist, int64_t deadline_monotonic_ms = 0) {
        std::unique_lock<std::mutex> lock(mutex_, std::defer_lock);
        auto result = lock_resolution_control(lock, deadline_monotonic_ms);
        if (result != ResolutionResult::Applied) return result;
        persist();
        matched_.reset();
        failed_.reset();
        enabled_ = true;
        return ResolutionResult::Applied;
    }

    template<typename Reinitialize>
    bool reinitialize(Reinitialize change) {
        std::lock_guard<std::mutex> lock(mutex_);
        if (!change()) return false;
        matched_.reset();
        failed_.reset();
        return true;
    }

    template<typename Apply>
    void match_source(const SourceModeKey& source, Apply apply,
                      Clock::time_point now = Clock::now()) {
        if (!enabled_) return;
        // An IPC modeset must not block the receive thread or queue a stale
        // automatic decision behind the manual request. Retry on a later frame.
        std::unique_lock<std::mutex> lock(mutex_, std::try_to_lock);
        if (!lock.owns_lock() || !enabled_) return;
        if (matched_ && *matched_ == source) return;
        if (failed_ && *failed_ == source && now < retry_after_) return;
        if (apply()) {
            matched_ = source;
            failed_.reset();
        } else {
            // Failed modesets remain retryable without hammering DRM each frame.
            matched_.reset();
            failed_ = source;
            retry_after_ = Clock::now() + std::chrono::seconds(1);
        }
    }

private:
    std::atomic<bool> enabled_;
    std::mutex mutex_;
    std::optional<SourceModeKey> matched_, failed_;
    Clock::time_point retry_after_{};
};
