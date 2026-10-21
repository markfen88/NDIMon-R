#pragma once
#include <nlohmann/json.hpp>
#include <ostream>
#include <string>

namespace ndimon::audio {
// Observe on the control thread: never format or write logs on the period thread.
class AudioMasterLog {
  public:
    void update(const nlohmann::json &status, std::ostream &output) {
        // start() launches asynchronously. Before the first completed period,
        // the initial timer flag does not yet describe the selected clock.
        if (!status.value("running", false) || status.value("periods", uint64_t{0}) == 0)
            return;
        const auto master = status.at("master").get<std::string>();
        const auto &engine = status.at("settings").at("engine");
        std::string message = "[AudioEngine] master=" + master;
        if (master == "timer") {
            if (!engine.at("enabled").get<bool>())
                message += " (engine disabled)";
            else
                message += " (" + engine.at("master").get<std::string>() + " unavailable)";
        }
        message += " period=" + std::to_string(status.at("period_frames").get<int>());
        if (message == previous_)
            return;
        output << message << '\n';
        previous_ = std::move(message);
    }

  private:
    std::string previous_;
};
} // namespace ndimon::audio
