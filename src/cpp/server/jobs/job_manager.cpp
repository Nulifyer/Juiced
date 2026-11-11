#include "lemon/jobs/job_manager.h"
#include "lemon/config_file.h"

#include "lemon/utils/path_utils.h"

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <set>

namespace fs = std::filesystem;

namespace lemon::jobs {
namespace {

bool cleanup_pending(const json& job) {
    return job.value("deleted", false);
}

json historical_view(json job) {
    const auto status = job.value("status", "queued");
    if (status == "queued" || status == "running") {
        job["status"] = "interrupted";
        job["error"] = "Historical workflow execution is disabled";
    }
    return job;
}

json summary(const json& job) {
    json result;
    for (const auto* field : {"id", "name", "status", "created_at", "finished_at", "summary", "error"})
        if (job.contains(field)) result[field] = job[field];
    const auto status = job.value("status", "queued");
    if (status == "queued" || status == "running") {
        result["status"] = "interrupted";
        result["error"] = "Historical workflow execution is disabled";
    }
    const auto steps = job.find("steps");
    size_t completed = 0;
    if (steps != job.end() && steps->is_array()) for (const auto& step : *steps) {
        if (!step.is_object()) continue;
        const auto status = step.find("status");
        if (status != step.end() && (*status == "completed" || *status == "skipped" ||
            (*status == "failed" && step.contains("failure_handled") && step["failure_handled"] == true))) ++completed;
    }
    result["progress"] = {{"cursor", job.value("cursor", json())},
        {"completed", completed}, {"step_count", steps != job.end() && steps->is_array() ? steps->size() : 0}};
    return result;
}

} // namespace

JobManager::JobManager(const std::string& config_dir, ResourceAdmission admission)
    : storage_path_(utils::path_to_utf8(utils::path_from_utf8(config_dir) / "jobs.json")),
      admission_(std::move(admission)) {
    ConfigFile::require_complete_directory(config_dir);
    load_from_disk();
}

void JobManager::load_from_disk() {
    try {
        const auto path = utils::path_from_utf8(storage_path_);
        if (!fs::exists(path)) return;
        std::ifstream input(path);
        if (!input) throw std::runtime_error("unreadable");
        auto document = json::parse(input);
        if (!document.is_object() || !document.contains("jobs") || !document["jobs"].is_array() ||
            (document.contains("version") && document["version"] != 1))
            throw std::runtime_error("unsupported schema");
        std::set<std::string> ids;
        for (const auto& job : document["jobs"]) {
            if (!job.is_object() || !job.contains("id") || !job["id"].is_string() ||
                job["id"].get<std::string>().empty() || !ids.insert(job["id"].get<std::string>()).second ||
                (job.contains("deleted") && !job["deleted"].is_boolean()) ||
                (job.contains("status") && !job["status"].is_string()))
                throw std::runtime_error("ambiguous records");
        }
        document_ = std::move(document);
    } catch (const std::exception&) {
        recovery_error_ = "Retained job storage requires recovery; its contents have been preserved";
    }
}

json JobManager::snapshot() const {
    std::lock_guard<std::mutex> lock(mutex_);
    json records = json::array(), quarantined = json::array();
    size_t pending = 0;
    for (const auto& job : document_["jobs"]) {
        if (cleanup_pending(job)) {
            ++pending;
            if (quarantined.size() < 50) quarantined.push_back({{"id", job["id"]},
                {"reason", "legacy_cleanup_unresolved"}});
        } else records.push_back(summary(job));
    }
    json recovery = {{"required", !recovery_error_.empty() || pending != 0},
        {"pending_cleanup", pending}, {"quarantined", std::move(quarantined)}};
    if (!recovery_error_.empty()) recovery["error"] = recovery_error_;
    return {{"jobs", std::move(records)}, {"metadata_only", true}, {"recovery", std::move(recovery)}};
}

std::optional<json> JobManager::get(const std::string& id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    for (const auto& job : document_["jobs"])
        if (job["id"] == id && !cleanup_pending(job)) return historical_view(job);
    return std::nullopt;
}

void JobManager::persist_locked(const json& document) {
    const auto target = utils::path_from_utf8(storage_path_);
    const auto temporary = utils::path_from_utf8(storage_path_ + ".tmp");
    {
        std::ofstream output(temporary, std::ios::trunc);
        output << document.dump(2);
        output.flush();
        output.close();
        if (!output.good()) throw JobError(500, "Could not durably delete job metadata: staging failed");
    }
    std::error_code error;
    utils::atomic_replace_file(temporary, target, error);
    if (error) {
        std::error_code cleanup;
        fs::remove(temporary, cleanup);
        throw JobError(500, "Could not durably delete job metadata: replacement failed");
    }
}

bool JobManager::remove(const std::string& id) {
    auto operation = admission_.write_scope("retained-job-metadata");
    std::lock_guard<std::mutex> lock(mutex_);
    if (!recovery_error_.empty()) throw JobError(409, recovery_error_);
    const auto& records = document_["jobs"];
    const auto found = std::find_if(records.begin(), records.end(), [&](const auto& job) { return job["id"] == id; });
    if (found == records.end()) return false;
    if (cleanup_pending(*found)) throw JobError(409, "Legacy job cleanup is unresolved; the record has been preserved");
    auto next = document_;
    next["jobs"].erase(next["jobs"].begin() + std::distance(records.begin(), found));
    persist_locked(next);
    document_ = std::move(next);
    return true;
}

} // namespace lemon::jobs
