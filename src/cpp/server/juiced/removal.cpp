#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include <fstream>

namespace lemon {
namespace {
bool within(const std::filesystem::path &path,
            const std::filesystem::path &root) {
  auto relative = std::filesystem::weakly_canonical(path).lexically_relative(
      std::filesystem::weakly_canonical(root));
  return !relative.empty() && relative != "." && *relative.begin() != "..";
}
void respond(httplib::Response &res, const nlohmann::json &data,
             int status = 200) {
  res.status = status;
  res.set_content(data.dump(), "application/json");
  res.set_header("Cache-Control", "no-store");
}
} // namespace
JuicedService::json JuicedService::removal_plan(const std::string &requested) {
  json plan;
  json stored = json::object();
  std::ifstream input(std::filesystem::path(server_.config_dir_) /
                      "user_models.json");
  if (input)
    input >> stored;
  std::string canonical, name;
  try {
    auto info = server_.model_manager_->get_model_info(requested);
    canonical = info.model_name;
    name = server_.model_manager_->get_public_model_name(canonical);
    plan = {{"model", name},
            {"canonical", canonical},
            {"checkpoints", info.checkpoints},
            {"action", info.source == "local_path" ? "unregister" : "cache"}};
    const auto clean =
        canonical.rfind("user.", 0) == 0 ? canonical.substr(5) : canonical;
    if (stored.contains(clean) &&
        stored[clean].value("juiced_existing_artifact", false))
      plan["action"] = "unregister";
    if (info.source == "local_path") {
      const auto file = std::filesystem::path(info.checkpoint());
      if (within(file, directory_ / "artifacts")) {
        bool shared = false;
        for (const auto &[id, other] :
             server_.model_manager_->get_supported_models()) {
          if (server_.model_manager_->get_public_model_name(id) == name)
            continue;
          for (const auto &[role, path] : other.checkpoints)
            if (std::filesystem::weakly_canonical(path) ==
                std::filesystem::weakly_canonical(file))
              shared = true;
        }
        if (!shared)
          plan["action"] = "upload";
      }
    }
  } catch (const std::exception &) {
    // Retained unsupported registrations still have an operator-owned catalog
    // entry.
    auto clean =
        requested.rfind("user.", 0) == 0 ? requested.substr(5) : requested;
    if (!stored.contains(clean) ||
        !stored[clean].value("juiced_existing_artifact", false))
      throw;
    name = clean;
    canonical = "user." + clean;
    plan = {{"model", name},
            {"canonical", canonical},
            {"checkpoints", stored[clean].value("checkpoints", json::object())},
            {"action", "unregister"}};
  }
  auto action = plan["action"].get<std::string>();
  plan["label"] = action == "unregister" ? "Remove registration"
                                         : "Delete downloaded weights";
  plan["description"] =
      action == "unregister" ? "Remove this catalog registration. External or "
                               "shared source files stay on disk."
      : action == "upload"
          ? "Delete this uploaded GGUF from Juiced storage and remove its "
            "registration."
          : "Remove this model's managed download. Files still used by other "
            "models stay. Original read-only library files stay. Built-in "
            "entries remain available to download again.";
  std::string reason;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    for (const auto &[id, job] : state_["preparations"].items())
      if (job.value("model", "") == name && job.value("pending", false))
        reason = "Cancel or finish preparation before removing its source model.";
    if (state_["policies"].value(name, "disabled") != "disabled")
      reason = "Move this model to Disabled before removing it.";
    else if (operations_.contains(name) &&
             operations_[name].value("pending", false))
      reason = "Wait for the current model operation to finish.";
  }
  if (server_.router_->is_model_loaded(name))
    reason = "Wait for this model to unload before removing it.";
  {
    std::lock_guard<std::mutex> lock(server_.downloads_mutex_);
    for (const auto &[id, job] : server_.download_jobs_) {
      const auto j = server_.download_job_to_json(job);
      if (j.value("model_name", "") == canonical ||
          j.value("model_name", "") == name) {
        const auto status = j.value("status", "");
        if (status == "downloading" || status == "queued" ||
            status == "running")
          reason = "Cancel or finish the active download before removing this "
                   "model.";
      }
    }
  }
  plan["allowed"] = reason.empty();
  plan["reason"] = reason;
  return plan;
}
void JuicedService::remove_model(const httplib::Request &req,
                                 httplib::Response &res) {
  const auto body = req.body.empty() ? json::object() : json::parse(req.body);
  const auto model = req.method == "GET" ? req.get_param_value("model")
                                         : body.at("model").get<std::string>();
  auto plan = removal_plan(model);
  if (req.method == "GET") {
    respond(res, plan);
    return;
  }
  if (!plan["allowed"].get<bool>()) {
    respond(res, {{"error", plan["reason"]}}, 409);
    return;
  }
  if (body.value("confirmation", "") != plan["model"].get<std::string>()) {
    respond(res, {{"error", "Confirm removal with the exact model name"}}, 400);
    return;
  }
  if (!body.contains("expected") || body["expected"] != plan) {
    respond(res, {{"error", "The model changed; review removal again"}}, 409);
    return;
  }
  const auto name = plan["model"].get<std::string>();
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (state_["policies"].value(name, "disabled") != "disabled" ||
        (operations_.contains(name) &&
         operations_[name].value("pending", false))) {
      respond(res, {{"error", "The model changed; review removal again"}}, 409);
      return;
    }
    operations_[name] = {{"pending", true}, {"state", "removing"}};
  }
  try {
    if (plan["action"] == "upload") {
      const auto file = plan["checkpoints"].at("main").get<std::string>();
      std::filesystem::remove(file);
      server_.model_manager_->unregister_user_model(plan["canonical"]);
    } else if (plan["action"] == "unregister") {
      server_.model_manager_->unregister_user_model(plan["canonical"]);
    } else
      server_.model_manager_->delete_model(plan["canonical"]);
    {
      std::lock_guard<std::mutex> lock(mutex_);
      operations_.erase(name);
    }
    refresh();
    wake_.notify_all();
    respond(res,
            {{"removed", true}, {"model", name}, {"action", plan["action"]}});
  } catch (...) {
    std::lock_guard<std::mutex> lock(mutex_);
    operations_.erase(name);
    throw;
  }
}
} // namespace lemon
