#pragma once
#include <algorithm>
#include <cmath>
#include <cstdint>
#include <deque>
#include <fstream>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <sstream>
#include <string>
// Shared by all video queues, independently of the frame type. No callback
// acquires a worker lock while this allocator's lock is held.
class VideoDelayBudget {
  public:
    static std::shared_ptr<VideoDelayBudget> shared() {
        static auto budget = std::make_shared<VideoDelayBudget>();
        return budget;
    }
    uint64_t share(const void *owner, uint64_t held, bool delayed,
                   std::optional<uint64_t> available = {}) {
        std::lock_guard<std::mutex> lock(mutex_);
        entries_[owner] = {held, delayed};
        uint64_t queued = 0, workers = 0;
        for (const auto &item : entries_) {
            queued += item.second.held;
            workers += item.second.delayed;
        }
        if (available) {
            constexpr uint64_t reserve = 256ull * 1024 * 1024;
            auto total = *available + queued;
            budget_ = total > reserve ? total - reserve : 0;
        }
        return budget_ / std::max<uint64_t>(1, workers);
    }
    void remove(const void *owner) {
        std::lock_guard<std::mutex> lock(mutex_);
        entries_.erase(owner);
    }

  private:
    struct Entry {
        uint64_t held;
        bool delayed;
    };
    std::mutex mutex_;
    std::map<const void *, Entry> entries_;
    uint64_t budget_ = 0;
};
// External synchronization is the owner's responsibility, just as with std::queue.
template <class F> class DelayedFrameQueue {
  public:
    using MemInfo = std::function<std::string()>;
    explicit DelayedFrameQueue(
        size_t frame_bytes_hint,
        MemInfo meminfo =
            [] {
                std::ifstream f("/proc/meminfo");
                return std::string(std::istreambuf_iterator<char>(f), {});
            },
        std::shared_ptr<VideoDelayBudget> budget = VideoDelayBudget::shared())
        : frame_bytes_(std::max<size_t>(1, frame_bytes_hint)), meminfo_(std::move(meminfo)),
          budget_(std::move(budget)) {
        set_delay_ms(0, 60);
    }
    ~DelayedFrameQueue() { budget_->remove(this); }
    DelayedFrameQueue(const DelayedFrameQueue &) = delete;
    DelayedFrameQueue &operator=(const DelayedFrameQueue &) = delete;
    void set_frame_bytes_hint(size_t bytes, int fps_num, int fps_den = 1) {
        if (bytes != frame_bytes_ || fps_num != fps_num_ || fps_den != fps_den_) {
            frame_bytes_ = std::max<size_t>(1, bytes);
            set_delay_ms(requested_, fps_num, fps_den);
        }
    }
    void set_delay_ms(int ms) { set_delay_ms(ms, fps_num_, fps_den_); }
    void set_delay_ms(int ms, int fps_num, int fps_den = 1) {
        if (requested_ == 0 && ms > 0) {
            last_push_us_.reset();
            interval_us_ = 0;
        }
        requested_ = std::clamp(ms, 0, 5000);
        fps_num_ = std::max(1, fps_num);
        fps_den_ = std::max(1, fps_den);
        std::istringstream f(meminfo_());
        std::string line;
        uint64_t available = 0;
        while (std::getline(f, line))
            if (line.rfind("MemAvailable:", 0) == 0) {
                std::istringstream v(line.substr(13));
                v >> available;
                available *= 1024;
                break;
            }
        update_limit(available);
        resize_delay();
    }
    int max_delay_ms() const {
        return std::min(5000, int(maximum_ > 2 ? (maximum_ - 2) * frame_interval_us() / 1000 : 0));
    }
    // Call only for an incoming video frame that will be queued. Delay zero
    // retains FrameSync's duplicate-repeat behavior used by the legacy path.
    bool accept_frame_timestamp(int64_t timestamp, bool framesync) {
        if (framesync && requested_ > 0 && last_timestamp_ == timestamp)
            return false;
        if (framesync)
            last_timestamp_ = timestamp;
        return true;
    }
    size_t max_frames() const { return maximum_; }
    int requested_delay_ms() const { return requested_; }
    int effective_delay_ms() const { return delay_; }
    bool push(F &&frame, int64_t now_us) {
        if (requested_ > 0) {
            update_limit();
            resize_delay();
        }
        if (requested_ > 0) {
            if (last_push_us_ && now_us > *last_push_us_) {
                double interval = now_us - *last_push_us_;
                interval_us_ = interval_us_ > 0 ? .9 * interval_us_ + .1 * interval : interval;
                resize_delay(std::min(interval_us_, interval));
            }
            last_push_us_ = now_us;
        }
        if (depth_ == 0)
            return false;
        bool kept = true;
        if (queue_.size() >= depth_) {
            queue_.pop_front();
            kept = false;
        }
        queue_.push_back({std::move(frame), now_us});
        record_held();
        return kept;
    }
    std::optional<F> pop_due(int64_t now_us) {
        if (queue_.empty() || (delay_ > 0 && next_due_us() > now_us))
            return {};
        F frame = std::move(queue_.front().frame);
        queue_.pop_front();
        record_held();
        return frame;
    }
    // Enqueue timestamps already use the real monotonic clock; do not round
    // due times to an integer-fps tick.
    int64_t next_due_us() const {
        return queue_.empty() ? 0 : queue_.front().enqueued_us + int64_t(delay_) * 1000;
    }
    bool empty() const { return queue_.empty(); }
    size_t size() const { return queue_.size(); }
    void clear() {
        queue_.clear();
        last_push_us_.reset();
        last_timestamp_.reset();
        interval_us_ = 0;
        record_held();
    }

  private:
    void record_held() { budget_->share(this, queue_.size() * frame_bytes_, requested_ > 0); }
    void update_limit(std::optional<uint64_t> available = {}) {
        auto bytes = budget_->share(this, queue_.size() * frame_bytes_, requested_ > 0, available);
        maximum_ = 2 + std::min<uint64_t>(598, bytes / frame_bytes_);
    }
    double frame_interval_us() const {
        double nominal = 1000000.0 * fps_den_ / fps_num_;
        return interval_us_ > 0 ? std::min(nominal, interval_us_) : nominal;
    }
    void resize_delay(double interval = 0) {
        if (interval <= 0)
            interval = frame_interval_us();
        depth_ = std::min(maximum_, size_t(std::ceil(requested_ * 1000.0 / interval)) + 2);
        delay_ = std::min(requested_, int(maximum_ > 2 ? (maximum_ - 2) * interval / 1000 : 0));
        while (queue_.size() > depth_)
            queue_.pop_front();
        record_held();
    }
    std::optional<int64_t> last_push_us_, last_timestamp_;
    double interval_us_ = 0;
    struct Entry {
        F frame;
        int64_t enqueued_us;
    };
    std::deque<Entry> queue_;
    size_t frame_bytes_, maximum_ = 2, depth_ = 2;
    MemInfo meminfo_;
    std::shared_ptr<VideoDelayBudget> budget_;
    int requested_ = 0, fps_num_ = 60, fps_den_ = 1, delay_ = 0;
};
