#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/server.h"
#include <chrono>
#include <fstream>
#include <regex>

namespace lemon {
void JuicedService::preparation_api(const httplib::Request &req,
                                    httplib::Response &res) {
  try {
    json result;
    if (req.method == "GET") {
      if (req.has_param("cache"))
        result = {{"artifacts", server_.model_manager_->prepared_artifacts()}};
      else if (req.has_param("id")) {
        const auto id = req.get_param_value("id");
        if (!std::regex_match(id, std::regex("[A-Za-z0-9_-]{1,128}")))
          throw std::invalid_argument("Invalid preparation job identifier");
        {
          std::lock_guard<std::mutex> lock(mutex_);
          if (!state_["preparations"].contains(id))
            throw std::invalid_argument("Unknown preparation job");
          result["job"] = state_["preparations"][id];
        }
        std::ifstream log(directory_ / "preparations" / id / "output.log",
                          std::ios::binary);
        if (log) {
          log.seekg(0, std::ios::end);
          const std::streamoff size = log.tellg();
          log.seekg(size > std::streamoff(65536) ? size - std::streamoff(65536)
                                                 : std::streamoff(0));
          result["output"] =
              std::string(std::istreambuf_iterator<char>(log), {});
        }
      } else if (req.has_param("model"))
        throw AdmissionError("feature_retired", "Local model conversion and quantization are retired; find an existing published variant in Library", 410);
      else {
        std::lock_guard<std::mutex> lock(mutex_);
        result = {{"retired", true}, {"jobs", state_["preparations"]}};
      }
    } else {
      const auto body = json::parse(req.body);
      const auto action = body.value("action", "");
      if (action != "cancel" && action != "delete_cache")
        throw AdmissionError("feature_retired", "Local model conversion and quantization are retired; find an existing published variant in Library", 410);
      ResourceAdmission::Lease mutation;
      if (action == "delete_cache")
        mutation = server_.router_->admit_management(body.value("id", std::string()), "legacy-artifact-cleanup");
      if (body.value("action", "") == "delete_cache") {
        const auto id = body.at("id").get<std::string>();
        if (!std::regex_match(id, std::regex("[a-f0-9]{64}")))
          throw std::invalid_argument("Invalid prepared artifact identifier");
        MutationClaim cleanup(mutex_, prepared_cleanup_active_);
        if (!cleanup.acquire(id))
          throw AdmissionError("artifact_cleanup_busy", "Wait for this prepared copy cleanup", 409);
        {
          std::lock_guard<std::mutex> lock(mutex_);
          for (const auto &[key, job] : state_["preparations"].items())
            if (job.value("pending", false))
              throw std::invalid_argument("Wait for active preparations before deleting cached artifacts");
        }
        server_.model_manager_->remove_prepared_artifact(id);
        result = {{"removed", true}};
      } else if (body.value("action", "") == "cancel") {
        const auto id = body.at("id").get<std::string>();
        std::lock_guard<std::mutex> lock(mutex_);
        if (!state_["preparations"].contains(id))
          throw std::invalid_argument("Unknown preparation job");
        if (state_["preparations"][id].value("pending", false))
          preparation_cancelled_.insert(id);
        result = {{"accepted", true}, {"pending", state_["preparations"][id].value("pending", false)},
                  {"recovery_required", state_["preparations"][id].value("cleanup_pending", false)}};
      }

    }
    res.set_content(result.dump(), "application/json");
  } catch (const AdmissionError &error) {
    res.status = error.status;
    if (error.retry_after()) res.set_header("Retry-After", std::to_string(error.retry_after()));
    res.set_content(error.to_json().dump(), "application/json");
  } catch (const std::exception &error) {
    res.status = 400;
    res.set_content(json{{"error", error.what()}}.dump(), "application/json");
  }
  res.set_header("Cache-Control", "no-store");
}
void JuicedService::preparation_run() {
  while (!stopping_) {
    {
      std::unique_lock<std::mutex> lock(mutex_);
      wake_.wait_for(lock, std::chrono::milliseconds(250), [this] {
        return stopping_ || bool(preparation_active_);
      });
      if (stopping_) break;
    }
    if (!finish_preparation_cleanup())
      std::this_thread::sleep_for(std::chrono::seconds(1));
  }
}

bool JuicedService::finish_preparation_cleanup() {
  if (!preparation_active_) return true;
  auto& task = *preparation_active_;
  const auto id = task.job.at("id").get<std::string>();
  try {
    if (task.process) {
      task.process->stop();
      if (!task.process->exited()) throw std::runtime_error("Preparation writer exit is unconfirmed");
    }
    std::filesystem::remove_all(directory_ / "prepared" / (".partial-" + id));
    {
      std::lock_guard<std::mutex> lock(mutex_);
      state_["preparations"][id].update(task.outcome);
      if (preparation_cancelled_.count(id)) state_["preparations"][id]["phase"] = "cancelled";
      state_["preparations"][id].update({{"writer_state", "stopped"}, {"cleanup_pending", false}});
      persist_locked();
      preparation_cancelled_.erase(id);
    }
    task.admission.completed();
    task.operation.completed();
    preparation_active_.reset();
    return true;
  } catch (const std::exception& error) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto candidate = state_["preparations"][id];
    candidate.update({{"pending", true}, {"phase", "cleanup-pending"},
        {"cleanup_pending", true}, {"error", error.what()}});
    if (candidate != state_["preparations"][id]) {
      state_["preparations"][id] = std::move(candidate);
      try { persist_locked(); } catch (...) {}
    }
    return false;
  }
}
} // namespace lemon
