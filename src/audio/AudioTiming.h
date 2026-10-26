#pragma once
#include <cstdint>

namespace ndimon::audio {
// Two periods rounded up to whole milliseconds: truncation can time out a
// healthy master before its next period (including at non-default sizes).
constexpr int master_wait_ms(int period, int rate) {
    return static_cast<int>((int64_t{2} * period * 1000 + rate - 1) / rate);
}

// delay() includes the plugin lead. Keep two actual periods as well, so one
// period remains queued while the engine waits to supply the next write.
constexpr int playback_drift_target_frames(int period, int lead) {
    return 2 * period + lead;
}
} // namespace ndimon::audio
