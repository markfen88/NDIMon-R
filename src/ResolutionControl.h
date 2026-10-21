#pragma once
#include <chrono>
#include <cstdint>
#include <mutex>
#include <thread>

enum class ResolutionResult { Applied, Busy, Expired, Failed };

// Linux steady_clock and Node process.hrtime use the same CLOCK_MONOTONIC epoch.
// Wall-clock/NTP changes must not revive expired queued requests.
inline bool resolution_expired(int64_t deadline_monotonic_ms) {
    return deadline_monotonic_ms && std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now().time_since_epoch()).count() >= deadline_monotonic_ms;
}

template<typename Mutex>
ResolutionResult lock_resolution_control(std::unique_lock<Mutex>& lock, int64_t deadline_monotonic_ms) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::milliseconds(250);
    for (;;) {
        if (resolution_expired(deadline_monotonic_ms)) return ResolutionResult::Expired;
        if (lock.try_lock()) {
            if (!resolution_expired(deadline_monotonic_ms)) return ResolutionResult::Applied;
            lock.unlock();
            return ResolutionResult::Expired;
        }
        if (std::chrono::steady_clock::now() >= deadline) return ResolutionResult::Busy;
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
}
