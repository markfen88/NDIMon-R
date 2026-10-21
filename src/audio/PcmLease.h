#pragma once
#include <cerrno>
#include <map>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <system_error>

namespace ndimon::audio {
// Control-side ownership survives graph preparation and old-graph retirement.
// A physical PCM is released only after the final graph reference is reclaimed.
class PcmLease {
  public:
    static std::shared_ptr<void> acquire(const std::string &pcm, const std::string &id) {
        auto lease = std::shared_ptr<PcmLease>(new PcmLease(pcm));
        std::lock_guard<std::mutex> lock(mutex_);
        const auto owner = owners_.find(pcm);
        if (owner != owners_.end()) {
            if (owner->second == id)
                throw std::system_error(EBUSY, std::generic_category(),
                                        "PCM retained by running graph");
            throw std::system_error(EBUSY, std::generic_category(),
                                    "PCM " + pcm + " already owned by sink " + owner->second);
        }
        owners_.emplace(pcm, id);
        lease->registered_ = true;
        return lease;
    }
    ~PcmLease() {
        if (registered_) {
            std::lock_guard<std::mutex> lock(mutex_);
            owners_.erase(pcm_);
        }
    }

  private:
    explicit PcmLease(std::string pcm) : pcm_(std::move(pcm)) {}
    std::string pcm_;
    bool registered_ = false;
    inline static std::mutex mutex_;
    inline static std::map<std::string, std::string> owners_;
};
} // namespace ndimon::audio
