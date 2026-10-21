#include "TimerMaster.h"
#include <cerrno>
namespace ndimon::audio {
bool TimerMaster::wait_period(int) {
    timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    int64_t n = int64_t(now.tv_sec) * 1000000000 + now.tv_nsec;
    if (deadline_ < n - period_ns_)
        deadline_ = n;
    deadline_ += period_ns_;
    timespec due{deadline_ / 1000000000, deadline_ % 1000000000};
    int rc;
    do {
        rc = clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &due, nullptr);
    } while (rc == EINTR);
    return rc == 0;
}
} // namespace ndimon::audio
