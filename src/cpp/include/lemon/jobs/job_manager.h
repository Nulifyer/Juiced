#pragma once

#include "lemon/resource_admission.h"

#include <nlohmann/json.hpp>

#include <mutex>
#include <optional>
#include <stdexcept>
#include <string>
#include <utility>

namespace lemon::jobs {

using json = nlohmann::ordered_json;

class JobError : public std::runtime_error {
public:
    JobError(int status, std::string message)
        : std::runtime_error(std::move(message)), status(status) {}
    int status;
};

class JobManager {
public:
    JobManager(const std::string& config_dir, ResourceAdmission admission);
    json snapshot() const;
    std::optional<json> get(const std::string& id) const;
    bool remove(const std::string& id);

private:
    friend struct JobRecordTestHook;
    void load_from_disk();
    void persist_locked(const json& document);

    std::string storage_path_;
    ResourceAdmission admission_;
    mutable std::mutex mutex_;
    json document_ = {{"version", 1}, {"jobs", json::array()}};
    std::string recovery_error_;
};

} // namespace lemon::jobs
