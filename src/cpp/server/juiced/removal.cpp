#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include <iomanip>
#include <sstream>
#include <mbedtls/md.h>

namespace lemon {
namespace {
bool within(const std::filesystem::path &path,
            const std::filesystem::path &root) {
  auto relative = std::filesystem::weakly_canonical(path).lexically_relative(
      std::filesystem::weakly_canonical(root));
  return !relative.empty() && relative != "." && *relative.begin() != "..";
}
std::string definition_identity(const nlohmann::json& definition) {
  const auto input = definition.dump();
  unsigned char digest[32];
  if (mbedtls_md(mbedtls_md_info_from_type(MBEDTLS_MD_SHA256),
      reinterpret_cast<const unsigned char*>(input.data()), input.size(), digest))
    throw std::runtime_error("Cannot identify the removal definition");
  std::ostringstream result;
  for (const auto byte : digest) result << std::hex << std::setw(2) << std::setfill('0') << int(byte);
  return result.str();
}
void respond(httplib::Response &res, const nlohmann::json &data,
             int status = 200) {
  res.status = status;
  res.set_content(data.dump(), "application/json");
  res.set_header("Cache-Control", "no-store");
}
} // namespace
bool JuicedService::import_owns_model_locked(const std::string& name) const {
  const auto bare = [](const std::string& value) { return value.rfind("user.", 0) == 0 ? value.substr(5) : value; };
  for (const auto& [id, job] : state_.at("imports").items())
    if (job.value("status", "") == "publishing" && bare(job.at("publication").at("name").get<std::string>()) == bare(name)) return true;
  return false;
}
void JuicedService::guard_model_removal_locked(const std::string& model) const {
  if (import_owns_model_locked(model))
    throw AdmissionError("import_publication_pending", "Finish import publication before removing this model", 409);
  const auto bare = [](const std::string& value) { return value.rfind("user.", 0) == 0 ? value.substr(5) : value; };
  for (const auto& [id, job] : state_.at("preparations").items()) {
    if (!job.value("pending", false)) continue;
    const auto source = job.find("model");
    if (source == job.end() || !source->is_string() || source->get_ref<const std::string&>().empty() ||
        bare(source->get<std::string>()) == bare(model))
      throw AdmissionError("preparation_cleanup_pending", "Resolve retained writer ownership before removing its source model", 409);
  }
}
void JuicedService::with_model_removal(const std::string& model, const std::function<void()>& remove) {
  auto writer = server_.router_->admission_view().write_scope("model-removal");
  auto mutation = server_.model_manager_->model_mutation(model, false);
  if (!mutation.owns_lock()) throw AdmissionError("model_mutation_busy", "Wait for model publication to finish", 409);
  {
    std::lock_guard<std::mutex> lock(mutex_);
    guard_model_removal_locked(model);
  }
  remove();
}
JuicedService::json JuicedService::removal_plan(const std::string &requested) {
  json plan;
  auto definition = server_.model_manager_->user_model_definition(requested);
  const bool cleanup_pending = definition.is_object() && definition.contains("juiced_removal");
  const bool flm_cleanup = cleanup_pending && definition.at("juiced_removal").value("provider", "") == "flm";
  if (cleanup_pending) definition.erase("juiced_removal");
  json stored = json::object();
  auto requested_clean = requested.rfind("user.", 0) == 0 ? requested.substr(5) : requested;
  if (!definition.is_null()) stored[requested_clean] = definition;
  std::string canonical, name;
  try {
    if (cleanup_pending) throw std::runtime_error("Resume owned artifact cleanup");
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
    if (info.recipe == "flm" && canonical.rfind("user.", 0) == 0 &&
        server_.model_manager_->flm_checkpoint_referenced(canonical, info.checkpoint()))
      plan["action"] = "unregister";
    if (info.source == "local_path") {
      const auto file = std::filesystem::path(info.checkpoint());
      if (within(file, directory_ / "artifacts")) {
        const bool shared = server_.model_manager_->artifact_referenced(canonical, file);
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
        (!cleanup_pending && !stored[clean].value("juiced_existing_artifact", false)))
      throw;
    name = clean;
    canonical = "user." + clean;
    plan = {{"model", name},
            {"canonical", canonical},
            {"checkpoints", stored[clean].value("checkpoints", json::object())},
            {"action", cleanup_pending ? (flm_cleanup ? "cache" : "upload") : "unregister"}};
  }
  if (cleanup_pending && !plan["checkpoints"].contains("main") && definition.contains("checkpoint"))
    plan["checkpoints"]["main"] = definition.at("checkpoint");
  plan["definition"] = definition_identity({{"registration", definition}, {"checkpoints", plan["checkpoints"]}});
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
    if (import_owns_model_locked(name)) reason = "Finish import publication before removing this model.";
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
  if (removal_claim_barrier_) removal_claim_barrier_();
  bool operation_owned = false;
  try {
    with_model_removal(plan["canonical"], [&] {
      if (removal_plan(model) != plan)
        throw AdmissionError("model_definition_changed", "The model changed; review removal again", 409);
      auto definition = server_.model_manager_->user_model_definition(plan["canonical"]);
      if (definition.is_object()) definition.erase("juiced_removal");
      {
        std::lock_guard<std::mutex> lock(mutex_);
        guard_model_removal_locked(name);
        if (state_["policies"].value(name, "disabled") != "disabled" ||
            (operations_.contains(name) && operations_[name].value("pending", false)))
          throw AdmissionError("model_mutation_busy", "Wait for the current model operation to finish", 409);
        operations_[name] = {{"pending", true}, {"state", "removing"}};
        operation_owned = true;
      }
      if (plan["action"] == "upload")
        server_.model_manager_->remove_uploaded_model(plan["canonical"], definition);
      else if (plan["action"] == "unregister")
        server_.model_manager_->unregister_user_model(plan["canonical"]);
      else
        server_.model_manager_->delete_model(plan["canonical"]);
    });
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
    if (operation_owned) operations_.erase(name);
    throw;
  }
}
} // namespace lemon
