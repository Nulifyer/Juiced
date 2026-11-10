#include "lemon/juiced/service.h"
#include "lemon/config_file.h"
#include "lemon/gguf_reader.h"
#include "lemon/model_manager.h"
#include "lemon/router.h"
#include "lemon/request_identity.h"
#include "lemon/server.h"
#include "lemon/utils/http_client.h"
#include "lemon/utils/path_utils.h"
#include "lemon/version.h"
#include <algorithm>
#include <chrono>
#include <fstream>
#include <iomanip>
#include <limits>
#include <random>
#include <set>
#include <sstream>

namespace lemon {
namespace {
using json = nlohmann::json;
json import_labels(const std::string& recipe, const std::string& purpose) {
  if (recipe != "llamacpp" && recipe != "asr4all" && recipe != "parakeet" && recipe != "whispercpp")
    throw std::invalid_argument("Unsupported GGUF backend");
  if (recipe != "llamacpp") return json::array({"transcription"});
  if (purpose != "chat" && purpose != "embedding" && purpose != "reranking")
    throw std::invalid_argument("Choose Chat, Embeddings or Rerank for llama.cpp imports");
  return json::array({purpose});
}
thread_local std::chrono::steady_clock::time_point request_start;
thread_local json request_usage = json::object();
json response_usage(const httplib::Response &res) {
  json result = json::object();
  if (!res.body.empty() && res.body.size() <= 1048576 &&
      res.get_header_value("Content-Type").find("json") != std::string::npos) {
    const auto body = json::parse(res.body, nullptr, false);
    if (body.is_object() && body.contains("usage") && body["usage"].is_object()) {
      const auto &usage = body["usage"];
      for (const auto &field : {std::make_pair("input_tokens", "prompt_tokens"),
                               std::make_pair("output_tokens", "completion_tokens")}) {
        const auto value = usage.value(field.first, usage.value(field.second, json(nullptr)));
        if (value.is_number_integer() && value.get<long long>() >= 0)
          result[field.first] = value;
      }
    }
  }
  return result;
}
long long now() {
  return std::chrono::duration_cast<std::chrono::seconds>(
             std::chrono::system_clock::now().time_since_epoch())
      .count();
}
std::string identifier() {
  static std::atomic<unsigned long> n{0};
  return std::to_string(now()) + "-" + std::to_string(++n) + "-" +
         std::to_string(std::random_device{}());
}
void reply(httplib::Response &r, const json &j, int status = 200) {
  r.status = status;
  r.set_content(j.dump(), "application/json");
  r.set_header("Cache-Control", "no-store");
}
bool safe_name(const std::string &s) {
  return !s.empty() && s.size() < 160 && s != "." && s != ".." &&
         s.find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVW"
                             "XYZ0123456789._-") == std::string::npos;
}
json read_json(const std::filesystem::path &p, const json &fallback) {
  if (!std::filesystem::exists(p))
    return fallback;
  std::ifstream f(p);
  json j;
  f >> j;
  return j;
}
void atomic_json(const std::filesystem::path &p, const json &j) {
  auto tmp = p;
  tmp += ".tmp";
  {
    std::ofstream f(tmp);
    f << j.dump(2);
    f.flush();
    f.close();
    if (!f)
      throw std::runtime_error("Could not save state");
  }
  std::error_code ec;
  utils::atomic_replace_file(tmp, p, ec);
  if (ec) {
    std::error_code cleanup;
    std::filesystem::remove(tmp, cleanup);
    throw std::runtime_error("Could not replace state: " + ec.message());
  }
}
std::string service(const json &m) {
  auto labels = m.value("labels", std::vector<std::string>{});
  for (const auto &l : labels) {
    if (l == "transcription")
      return "Transcribe";
    if (l == "tts")
      return "Speak";
    if (l == "image" || l == "image-generation")
      return "Images";
    if (l == "embedding" || l == "embeddings")
      return "Embeddings";
    if (l == "reranking")
      return "Rerank";
    if (l == "audio-generation")
      return "Audio";
    if (l == "3d")
      return "3D";
    if (l == "classification")
      return "Classify";
  }
  if (std::find(labels.begin(), labels.end(), "chat") != labels.end())
    return "Chat";
  return "Other";
}
} // namespace
JuicedService::JuicedService(Server &server)
    : server_(server),
      directory_(std::filesystem::path(server.config_dir_) / "juiced") {
  std::filesystem::create_directories(directory_ / "imports");
  state_ = read_json(
      directory_ / "state.json",
      {{"version", 1},
       {"revision", 0},
       {"policies", json::object()},
       {"cache",
        {{"mode", "memory"}, {"retention_days", 7}, {"disk_budget_gib", 8}}},
       {"imports", json::object()}});
  bind_snapshot_storage();
  if (!state_.contains("transfers"))
    state_["transfers"] = json::object();
  for (auto &[id, j] : state_["transfers"].items())
    if (j.value("running", false)) {
      j["running"] = false;
      j["status"] = "interrupted";
    }
  cache_run_id_ = identifier();
  voice_recovery_required_locked();
  if (!state_.contains("qualified_profiles"))
    state_["qualified_profiles"] = json::object();
  if (!state_.contains("preparations")) state_["preparations"] = json::object();
  if (!state_["preparations"].is_object()) throw std::runtime_error("Malformed retained preparation inventory requires recovery");
  const auto previous_preparations = state_["preparations"];
  for (auto &[id, job] : state_["preparations"].items()) {
    if (!job.is_object()) throw std::runtime_error("Malformed retained preparation record requires recovery");
    const auto pending = job.find("pending");
    const auto writer = job.find("writer_state");
    const auto phase = job.find("phase");
    const auto model = job.find("model");
    const bool ambiguous = model == job.end() || !model->is_string() || model->get_ref<const std::string&>().empty() ||
        pending == job.end() || !pending->is_boolean() ||
        (writer != job.end() && (!writer->is_string() ||
          (*writer != "queued" && *writer != "stopped" && *writer != "dispatching"))) ||
        (*pending == false && ((writer != job.end() && *writer == "dispatching") ||
          phase == job.end() || !phase->is_string() ||
          (*phase != "complete" && *phase != "cancelled" && *phase != "failed" && *phase != "interrupted")));
    if (ambiguous && !job.contains("recovery_record")) {
      const auto original = job;
      job["recovery_record"] = original;
    }
    if (ambiguous || job.value("pending", false)) {
      if (!ambiguous && job.value("writer_state", "") == "queued" && !job.value("cleanup_pending", false)) {
        job["pending"] = false;
        job["phase"] = "interrupted";
      } else {
        job["pending"] = true;
        job["phase"] = "cleanup-pending";
        job["cleanup_pending"] = true;
        job["error"] = "Previous preparation writer termination is unconfirmed; recovery is required";
      }
    }
  }
  if (!state_.contains("execution_trials")) state_["execution_trials"] = json::object();
  committed_state_ = state_;
  if (state_["preparations"] != previous_preparations) persist_locked();
  std::vector<std::string> recovered;
  for (const auto &[name, trial] : state_["execution_trials"].items()) {
    try {
      server_.model_manager_->select_execution_profile(name, trial.at("previous"));
      recovered.push_back(name);
    } catch (const std::exception &error) {
      operations_[name] = {{"pending", false}, {"state", "failed"},
          {"error", "Interrupted trial recovery failed: " + std::string(error.what())}, {"updated", now()}};
    }
  }
  for (const auto &name : recovered) state_["execution_trials"].erase(name);
  if (!recovered.empty()) persist_locked();
  const auto fingerprint = credential_configuration_fingerprint(server_.api_key_, server_.admin_api_key_);
  if (state_.value("authentication", json::object()).value("configuration", "") != fingerprint ||
      state_.value("authentication", json::object()).value("epoch", "").empty()) {
    const auto generation = state_.value("authentication", json::object()).value("generation", uint64_t(0));
    if (generation == (std::numeric_limits<uint64_t>::max)()) throw std::runtime_error("Authentication epoch generation is exhausted");
    auto candidate = state_;
    candidate["authentication"] = {{"configuration", fingerprint}, {"generation", generation + 1},
                                    {"epoch", std::to_string(generation + 1) + ":" + identifier()}};
    state_ = save_state_locked(std::move(candidate));
  }
  server_.authentication_epoch_ = state_.at("authentication").at("epoch").get<std::string>();
  server_.model_manager_->set_model_removal_guard([this](const std::string& name) {
    std::lock_guard<std::mutex> lock(mutex_);
    guard_model_removal_locked(name);
  }, directory_ / "artifacts", directory_ / "prepared");
  for (const auto& [model, policy] : state_["policies"].items())
    server_.router_->publish_residency_policy(model, policy_revision_locked(model), policy == "hot");
  worker_ = std::thread([this] { run(); });
  preparation_worker_ = std::thread([this] { preparation_run(); });
}
JuicedService::~JuicedService() { stop(); }
void JuicedService::bind_snapshot_storage() {
  namespace fs = std::filesystem;
  const auto recovery = [] {
    throw std::runtime_error("Snapshot storage binding requires recovery; preserve state and snapshot directories");
  };
  if (!state_.is_object()) recovery();
  const auto configured = utils::get_environment_variable_utf8("JUICED_SNAPSHOT_DIR");
  if (std::getenv("JUICED_SNAPSHOT_DIR") && configured.empty()) recovery();
  snapshot_directory_ = utils::ManagedDirectoryLease::resolve_root(configured.empty() ? directory_ / "snapshots" : utils::path_from_utf8(configured));
  const auto state_directory = utils::path_to_utf8(fs::canonical(directory_));
  const auto snapshot_directory = utils::path_to_utf8(snapshot_directory_);
  for (const auto* field : {"snapshots", "snapshot_cleanup", "snapshot_writes"}) {
    if (!state_.contains(field)) state_[field] = json::object();
    if (!state_[field].is_object()) recovery();
  }
  const bool bound = state_.contains("snapshot_store");
  json binding;
  bool ready = false;
  if (bound) {
    binding = state_["snapshot_store"];
    if (!binding.is_object() || !binding.contains("ready") || !binding["ready"].is_boolean()) recovery();
    ready = binding["ready"].get<bool>();
    binding.erase("ready");
    if (binding.size() != 4 || binding.value("version", json()) != 1 ||
        binding.value("state_directory", json()) != state_directory ||
        binding.value("directory", json()) != snapshot_directory ||
        !binding.contains("id") || !binding["id"].is_string() || binding["id"].get_ref<const std::string&>().empty()) recovery();
    if (!ready)
      for (const auto* field : {"snapshots", "snapshot_cleanup", "snapshot_writes"})
        if (!state_[field].empty()) recovery();
  } else {
    for (const auto* field : {"snapshots", "snapshot_cleanup", "snapshot_writes"})
      if (!state_[field].empty()) recovery();
    binding = {{"version", 1}, {"state_directory", state_directory},
               {"directory", snapshot_directory}, {"id", identifier()}};
  }
  if (ready && !fs::is_directory(snapshot_directory_)) recovery();
  if (server_.storage_owner_) {
    server_.snapshot_root_claim_ = server_.storage_owner_->claim(snapshot_directory_);
    if (server_.snapshot_root_claim_.root() != snapshot_directory_) recovery();
  } else {
    fs::create_directories(snapshot_directory_);
  }
  const auto marker = snapshot_directory_ / ".juiced-snapshot-store.json";
  const auto status = fs::symlink_status(marker);
  if (fs::exists(status)) {
    if (!bound || !fs::is_regular_file(status)) recovery();
    try { if (read_json(marker, json()) != binding) recovery(); }
    catch (const json::exception&) { recovery(); }
  } else {
    if (ready) recovery();
    for (const auto& entry : fs::directory_iterator(snapshot_directory_))
      if (entry.path().filename() != utils::ManagedDirectoryLease::file_name) recovery();
    if (!bound) {
      auto candidate = state_;
      candidate["snapshot_store"] = binding;
      candidate["snapshot_store"]["ready"] = false;
      state_ = save_state_locked(std::move(candidate));
    }
    // Persist the intent before the marker so an interrupted first bind is retryable.
    atomic_json(marker, binding);
  }
  if (!ready) {
    for (const auto& entry : fs::directory_iterator(snapshot_directory_))
      if (entry.path() != marker && entry.path().filename() != utils::ManagedDirectoryLease::file_name) recovery();
    auto candidate = state_;
    candidate["snapshot_store"] = binding;
    candidate["snapshot_store"]["ready"] = true;
    state_ = save_state_locked(std::move(candidate));
  }
}
void JuicedService::stop() {
  stopping_ = true;
  wake_.notify_all();
  if (worker_.joinable())
    worker_.join();
  {
    std::lock_guard<std::mutex> lock(mutex_);
    for (const auto& accepted : pending_) {
      const auto model = accepted.command.at("model").get<std::string>();
      operations_[model] = {{"pending", false}, {"state", "cancelled"},
                            {"error", "Server stopped before command execution"}, {"updated", now()}};
    }
    pending_.clear();
  }
  if (preparation_worker_.joinable()) preparation_worker_.join();
  finish_preparation_cleanup();
  finish_voice_cleanup();
}
JuicedService::json JuicedService::save_state_locked(json candidate) {
  candidate["revision"] = state_.value("revision", uint64_t(0)) + 1;
  auto durable_candidate = candidate;
  atomic_json(directory_ / "state.json", candidate);
  committed_state_.swap(durable_candidate);
  return candidate;
}
void JuicedService::persist_locked() {
  auto rollback = committed_state_;
  try {
    const auto committed = save_state_locked(state_);
    state_["revision"] = committed["revision"];
  } catch (...) {
    state_.swap(rollback);
    throw;
  }
}
void JuicedService::set_policy_locked(const std::string& model, const std::string& policy) {
  auto candidate = state_;
  candidate["policies"][model] = policy;
  candidate["policy_revisions"][model] = state_.value("revision", uint64_t(0)) + 1;
  auto committed = save_state_locked(std::move(candidate));
  state_.swap(committed);
}
uint64_t JuicedService::policy_revision_locked(const std::string& model) const {
  return state_.value("policy_revisions", json::object()).value(model, uint64_t(0));
}
void JuicedService::apply_policy_pin_locked(const std::string& model, uint64_t revision,
                                           uint64_t generation) {
  bool pinned;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (revision != policy_revision_locked(model)) return;
    pinned = state_["policies"].value(model, "disabled") == "hot";
  }
  try {
    server_.router_->publish_residency_policy(model, revision, pinned, generation);
  } catch (const std::exception& error) {
    LOG(WARNING, "Juiced") << "Committed policy will reconcile worker pin: " << error.what() << std::endl;
  }
  wake_.notify_all();
}
bool JuicedService::allows(const std::string &requested) const {
  const auto target = server_.alias_manager_
      ? server_.alias_manager_->resolve_alias(requested).value_or(requested) : requested;
  if (server_.model_manager_->model_exists(target) &&
      is_omni_collection_recipe(server_.model_manager_->get_model_info(target).recipe))
    return false;
  auto model = server_.model_manager_->get_public_model_name(
      server_.model_manager_->resolve_model_name(target));
  std::lock_guard<std::mutex> lock(mutex_);
  return state_["policies"].value(model, "disabled") != "disabled";
}
bool JuicedService::hot(const std::string &requested) const {
  auto model = server_.model_manager_->get_public_model_name(
      server_.model_manager_->resolve_model_name(requested));
  std::lock_guard<std::mutex> lock(mutex_);
  return state_["policies"].value(model, "disabled") == "hot";
}
void JuicedService::set_hot(const std::string &requested, bool pinned) {
  auto writer = server_.router_->admission_view().write_scope("model-pin");
  auto model = server_.model_manager_->get_public_model_name(
      server_.model_manager_->resolve_model_name(requested));
  if (is_omni_collection_recipe(server_.model_manager_->get_model_info(model).recipe))
    throw std::invalid_argument("Agent collections cannot be enabled as runtime services");
  std::lock_guard<std::mutex> effects(policy_effect_mutex_);
  uint64_t revision;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto current = state_["policies"].value(model, "disabled");
    set_policy_locked(model, pinned ? "hot" : current == "disabled" ? "disabled" : "on-demand");
    revision = policy_revision_locked(model);
  }
  if (policy_effect_barrier_) policy_effect_barrier_();
  apply_policy_pin_locked(model, revision);
}
bool JuicedService::inference_guard(const httplib::Request &req,
                                    httplib::Response &res) {
  if (req.method != "POST")
    return false;
  const auto last = req.path.substr(req.path.find_last_of('/') + 1);
  const bool inference =
      last == "completions" || last == "responses" || last == "messages" ||
      last == "chat" || last == "generate" || last == "embeddings" ||
      last == "embed" || last == "rerank" || last == "reranking" ||
      last == "classify" || last == "generations" || last == "speech" || last == "upscale";
  if (!inference)
    return false;
  try {
    auto body = json::parse(req.body);
    auto model = body.value("model", "");
    if (model.size() > 7 && model.substr(model.size() - 7) == ":latest")
      model.resize(model.size() - 7);
    if (!model.empty() && !allows(model)) {
      reply(res,
            {{"error",
              {{"message", "Model is disabled by its loading policy"},
               {"type", "invalid_request_error"}}}},
            409);
      return true;
    }
    if (!model.empty()) {
      const auto canonical = server_.model_manager_->get_public_model_name(
          server_.model_manager_->resolve_model_name(model));
      std::lock_guard<std::mutex> lock(mutex_);
      if (operations_.contains(canonical) &&
          operations_[canonical].value("pending", false)) {
        res.set_header("Retry-After", "2");
        reply(res,
              {{"error",
                "Model settings or residency are changing; retry shortly"}},
              503);
        return true;
      }
      if (state_.value("execution_trials", json::object()).contains(canonical)) {
        reply(res, {{"error", {{"message", "Recover the interrupted runtime trial from the model Runtime screen"}, {"type", "invalid_request_error"}}}}, 409);
        return true;
      }
    }
    if ((req.path == "/api/chat" || req.path == "/api/generate") &&
        body.contains("keep_alive") && body["keep_alive"] == 0 &&
        body.value("messages", json::array()).empty() &&
        body.value("prompt", "").empty() && hot(model)) {
      reply(res,
            {{"error",
              "Hot models cannot be unloaded by client keep_alive hints"}},
            409);
      return true;
    }
  } catch (const std::exception &) {
  }
  if (req.path.find("/chat/completions") == std::string::npos)
    return unleased_protocol(req, res);
  return false;
}
void JuicedService::enqueue(const json &command) {
  auto writer = server_.router_->admission_view().write_scope("queued-model-command");
  std::lock_guard<std::mutex> lock(mutex_);
  auto m = command.at("model").get<std::string>();
  if (operations_.contains(m) && operations_[m].value("pending", false))
    throw std::invalid_argument("A model operation is already pending");
  pending_.push_back({command, writer.continuation()});
  operations_[m] = {
      {"state", command.value("action", "") == "load" ? "queued" : "draining"},
      {"pending", true},
      {"updated", now()}};
  wake_.notify_all();
}
void JuicedService::refresh() {
  auto writer = server_.router_->admission_view().write_scope("catalog-reconciliation");
  auto all = server_.model_manager_->get_supported_models();
  json result = json::array();
  for (const auto &[id, info] : all) {
    auto m = server_.model_info_to_json(id, info);
    m["service"] = service(m);
    if (is_omni_collection_recipe(info.recipe)) {
      m["available"] = false;
      m["unavailable_reason"] = "Agent collections are outside Juiced model services";
    }
    m["execution_profile"] = info.extras.count("execution_profile") ? info.extras.at("execution_profile").get<std::string>() : "registered";
    m["capability_contract"] = model_capabilities(info, m);
    result.push_back(std::move(m));
  }
  // Unsupported stored deployments remain visible in Library, without inventing
  // a runtime.
  auto definitions =
      read_json(std::filesystem::path(server_.config_dir_) / "user_models.json",
                json::object());
  for (const auto &[name, definition] : definitions.items()) {
    if (!definition.value("juiced_existing_artifact", false) ||
        server_.model_manager_->model_exists("user." + name))
      continue;
    try {
      json m = {
          {"id", name},
          {"recipe", definition.value("recipe", "")},
          {"labels", definition.value("labels", json::array())},
          {"checkpoints", definition.value("checkpoints", json::object())},
          {"size", definition.value("size", 0.0)},
          {"downloaded", true},
          {"loaded", false},
          {"state", "unsupported"},
          {"available", false},
          {"unavailable_reason",
           "Weights preserved from the previous manager; this optional "
           "backend is not included in Juiced"}};
      m["service"] = service(m);
      result.push_back(m);
    } catch (const std::exception &) {
    }
  }
  json transfers = json::object();
  {
    std::lock_guard<std::mutex> lock(server_.downloads_mutex_);
    for (const auto &[id, j] : server_.download_jobs_)
      transfers[id] = server_.download_job_to_json(j);
  }
  std::lock_guard<std::mutex> lock(mutex_);
  catalog_ = std::move(result);
  bool changed = false;
  for (const auto &[id, j] : transfers.items())
    if (!state_["transfers"].contains(id) || state_["transfers"][id] != j) {
      state_["transfers"][id] = j;
      changed = true;
    }
  if (changed)
    persist_locked();
}
void JuicedService::execute(const json &command) {
  auto writer = server_.router_->admission_view().write_scope("model-command");
  const auto name = command.at("model").get<std::string>();
  const auto action = command.at("action").get<std::string>();
  json previous_profile;
  bool profile_changed = false, trial_started = false, was_loaded = false;
  try {
    if (action == "unload") {
      std::lock_guard<std::mutex> effects(policy_effect_mutex_);
      {
        std::lock_guard<std::mutex> lock(mutex_);
        if (state_["policies"].value(name, "disabled") == "hot" ||
            (command.contains("policy_revision") &&
             command.at("policy_revision") != policy_revision_locked(name))) {
          operations_.erase(name);
          return;
        }
      }
      const auto generation = command.value("worker_generation", server_.router_->get_model_generation(name));
      if (generation && !server_.router_->apply_model_policy(name, generation, false, true))
        throw std::runtime_error("Worker changed or is busy; retry unloading after active requests finish");
      std::lock_guard<std::mutex> lock(mutex_);
      operations_.erase(name);
      return;
    }

    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (command.contains("policy_revision") &&
          (command.at("policy_revision") != policy_revision_locked(name) || state_["policies"].value(name, "disabled") != "hot")) {
        operations_.erase(name);
        return;
      }
      operations_[name] = {{"pending", true},
                           {"state", action == "reload" ? "reloading"
                                     : action == "load" ? "loading"
                                                        : "unloading"},
                           {"updated", now()}};
    }
    auto info = server_.model_manager_->get_model_info(name);
    if (command.contains("profile")) {
      const auto state = server_.model_manager_->get_execution_profiles(name);
      const auto selected = state.value("selected", "registered");
      if (command.value("expected_selected", selected) != selected)
        throw std::runtime_error("Execution profile changed elsewhere; refresh and retry");
      previous_profile = state.value("profiles", json::object()).value(selected,
          json{{"id", selected}, {"recipe", info.recipe}, {"options", info.recipe_options.to_json()}});
      previous_profile["id"] = selected;
      previous_profile["recipe"] = info.recipe;
      previous_profile["options"] = info.recipe_options.to_json();
      was_loaded = server_.router_->is_model_loaded(name);
      for (const auto &loaded : server_.router_->get_all_loaded_models())
        if (loaded.value("model_name", "") == name && loaded.value("is_busy", false))
          throw std::runtime_error("Model is busy; retry after active requests finish");
      {
        std::lock_guard<std::mutex> lock(mutex_);
        if (state_["execution_trials"].contains(name))
          previous_profile = state_["execution_trials"][name].at("previous");
        state_["execution_trials"][name] = {{"previous", previous_profile}, {"was_loaded", was_loaded}};
        persist_locked();
        trial_started = true;
      }
      if (was_loaded) {
        server_.router_->unload_model(name);
        if (server_.router_->is_model_loaded(name)) throw std::runtime_error("Model is still draining");
      }
      server_.model_manager_->select_execution_profile(name, command.at("profile"));
      profile_changed = true;
      info = server_.model_manager_->get_model_info(name);
      info.extras["execution_trial"] = true;
    }
    if (action == "load" || action == "reload") {
      if (!allows(name))
        throw std::invalid_argument("Model is disabled");
      if (!server_.model_manager_->is_model_downloaded(name))
        throw std::invalid_argument("Download this model before loading it");
      std::string policy;
      {
        std::lock_guard<std::mutex> lock(mutex_);
        policy = state_["policies"].value(name, "disabled");
      }
      if (action == "reload" && !profile_changed) {
        for (const auto &l : server_.router_->get_all_loaded_models())
          if (l.value("model_name", "") == name && l.value("is_busy", false))
            throw std::runtime_error(
                "Model is busy; retry after active requests finish");
        server_.router_->unload_model(name);
        if (server_.router_->is_model_loaded(name))
          throw std::runtime_error(
              "Model is still draining; retry after requests finish");
      }
      server_.router_->load_model(
          name, info, RecipeOptions(info.recipe, json::object()), true, false,
          policy == "hot", LoadPurpose::UserInference, &stopping_,
          command.contains("policy_revision") ? std::optional<uint64_t>(command.at("policy_revision").get<uint64_t>()) : std::nullopt);
    }
    {
      std::lock_guard<std::mutex> effects(policy_effect_mutex_);
      uint64_t revision;
      { std::lock_guard<std::mutex> lock(mutex_); revision = policy_revision_locked(name); }
      apply_policy_pin_locked(name, revision);
    }
    {
      std::lock_guard<std::mutex> lock(mutex_);
      operations_.erase(name);
      if (trial_started) { state_["execution_trials"].erase(name); persist_locked(); }
    }
  } catch (const std::exception &e) {
    if (command.contains("policy_revision") && !command.contains("profile")) {
      std::lock_guard<std::mutex> lock(mutex_);
      if (command.at("policy_revision") != policy_revision_locked(name)) {
        operations_.erase(name);
        wake_.notify_all();
        return;
      }
    }

    std::string error = e.what();
    if (trial_started) {
      try {
        if (server_.router_->is_model_loaded(name)) server_.router_->unload_model(name);
        server_.model_manager_->select_execution_profile(name, previous_profile);
        if (was_loaded && !stopping_) {
          auto restored = server_.model_manager_->get_model_info(name);
          restored.extras["execution_trial"] = true;
          server_.router_->load_model(name, restored, RecipeOptions(restored.recipe, json::object()),
              true, false, hot(name), LoadPurpose::UserInference, &stopping_);
        }
        { std::lock_guard<std::mutex> lock(mutex_); state_["execution_trials"].erase(name); persist_locked(); }
        error += "; previous execution profile restored";
      } catch (const std::exception &restore) { error += "; recovery failed: " + std::string(restore.what()); }
    }
    std::lock_guard<std::mutex> lock(mutex_);
    operations_[name] = {{"pending", false},
                         {"state", "failed"},
                         {"error", error},
                         {"updated", now()}};
  }
}
void JuicedService::reconcile() {
  auto writer = server_.router_->admission_view().write_scope("policy-reconciliation");
  json policies, revisions, catalog, interrupted;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    policies = state_["policies"];
    revisions = state_.value("policy_revisions", json::object());
    catalog = catalog_;
    interrupted = state_["execution_trials"];
  }
  if (reconciliation_barrier_) reconciliation_barrier_();
  std::vector<std::pair<std::string, json>> ordered;
  for (const auto &item : policies.items())
    ordered.emplace_back(item.key(), item.value());
  auto weight = [&](const std::string &name) {
    for (const auto &m : catalog)
      if (m.value("id", "") == name) {
        if (m.value("service", "") == "Speak" ||
            m.value("service", "") == "Transcribe")
          return 0.0;
        return m.value("size", 1000.0);
      }
    return 1000.0;
  };
  std::stable_sort(ordered.begin(), ordered.end(),
                   [&](const auto &a, const auto &b) {
                     return weight(a.first) < weight(b.first);
                   });
  for (const auto &[name, p] : ordered) {
    if (p == "hot" && interrupted.contains(name)) continue;
    std::lock_guard<std::mutex> effects(policy_effect_mutex_);
    const auto revision = revisions.value(name, uint64_t(0));
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (revision != policy_revision_locked(name) || state_["policies"].value(name, "disabled") != p) continue;
    }
    bool loaded = false, downloaded = false;
    uint64_t generation = 0;
    for (const auto &m : catalog)
      if (m.value("id", "") == name) {
        loaded = m.value("loaded", false);
        generation = m.value("runtime", json::object()).value("worker_generation", uint64_t(0));
        if (loaded && generation) apply_policy_pin_locked(name, revision, generation);
        downloaded = m.value("downloaded", false);
        break;
      }
    bool pending = false, retry = true;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (operations_.contains(name)) {
        pending = operations_[name].value("pending", false);
        retry = now() - operations_[name].value("updated", 0LL) > 30;
      }
    }
    if (!pending && retry && p == "hot" && !loaded && downloaded && allows(name))
      enqueue({{"model", name}, {"action", "load"}, {"policy_revision", revision}});
    if (!pending && retry && p == "disabled" && loaded)
      enqueue({{"model", name}, {"action", "unload"}, {"policy_revision", revision}, {"worker_generation", generation}});
  }
}
bool JuicedService::execute_pending() {
  QueuedCommand accepted;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (pending_.empty()) return false;
    accepted = std::move(pending_.front());
    pending_.pop_front();
  }
  auto publication = accepted.writer.enter();
  execute(accepted.command);
  return true;
}
void JuicedService::run() {
  while (!stopping_) {
    try {
      execute_pending();
      auto writer = server_.router_->admission_view().write_scope("juiced-reconciliation");
      refresh();
      reconcile_imports();
      reconcile_snapshot_writes();
      {
        std::lock_guard<std::mutex> lock(mutex_);
        prune_cache_locked();
      }
      reconcile();
      reconcile_voice();
    } catch (const std::exception &e) {
      std::lock_guard<std::mutex> lock(mutex_);
      state_["last_error"] = e.what();
    }
    std::unique_lock<std::mutex> lock(mutex_);
    wake_.wait_for(lock, std::chrono::seconds(2),
                   [this] { return stopping_ || !pending_.empty(); });
  }
}
void JuicedService::request_started() {
  request_start = std::chrono::steady_clock::now();
  request_usage = json::object();
}
void JuicedService::capture_response_usage(const httplib::Response &res) {
  try { request_usage = response_usage(res); } catch (...) {}
}
void JuicedService::request_finished(const httplib::Request &req,
                                     const httplib::Response &res) {
  if (req.method != "POST") return;
  std::string route = req.path;
  std::string protocol = "openai";
  if (route == "/api/chat" || route == "/api/generate" ||
      route == "/api/embed" || route == "/api/embeddings") {
    protocol = "ollama";
  } else if (route == "/sdapi/v1/txt2img") {
    protocol = "sdapi";
  } else {
    bool versioned = false;
    for (const std::string prefix : {"/api/v0/", "/api/v1/", "/v0/", "/v1/"}) {
      if (route.rfind(prefix, 0) == 0) {
        route = route.substr(prefix.size());
        versioned = true;
        break;
      }
    }
    const std::set<std::string> inference = {"chat/completions", "completions",
      "responses", "messages", "audio/transcriptions", "audio/speech",
      "images/generations", "images/edits", "embeddings", "rerank", "classify"};
    if (!versioned || !inference.count(route)) return;
    if (route == "messages") protocol = "anthropic";
  }
  try {
    std::string model;
    bool stream = false;
    if (!req.body.empty() && req.body.size() <= 262144 &&
        req.get_header_value("Content-Type").find("json") != std::string::npos) {
      const auto body = json::parse(req.body, nullptr, false);
      if (body.is_object()) {
        if (body.contains("model") && body["model"].is_string()) model = body["model"].get<std::string>();
        stream = body.value("stream", json(false)) == json(true);
      }
    } else if (req.form.has_field("model")) {
      model = req.form.get_field("model");
    }
    // Only a registered identifier is retained; never retain arbitrary body text
    // presented as a model, headers, query strings, prompts or response content.
    if (model.size() > 256 || !server_.model_manager_->model_exists_unfiltered(model)) model.clear();
    json record = {{"endpoint", route}, {"protocol", protocol},
      {"model", model.empty() ? json(nullptr) : json(model)}, {"stream", stream},
      {"status", res.status}, {"input_tokens", nullptr}, {"output_tokens", nullptr},
      {"duration_ms", std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - request_start).count()},
      {"timestamp_ms", std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::system_clock::now().time_since_epoch()).count()}};
    record.update(request_usage);
    record.update(response_usage(res));
    std::lock_guard<std::mutex> lock(requests_mutex_);
    record["id"] = ++request_sequence_;
    requests_.push_back(std::move(record));
    while (requests_.size() > 200) requests_.pop_front();
  } catch (...) {
    // Diagnostics must never alter serving or throw from the HTTP logger.
  }
}

void JuicedService::routes(httplib::Server &http) {
  const auto post = [this, &http](const std::string& path, httplib::Server::Handler handler) {
    http.Post(path, server_.contextual_handler(std::move(handler)));
  };
  compatibility_routes(http);
  const std::string ui = utils::get_resource_path("resources/juiced");
  if (std::filesystem::is_regular_file(std::filesystem::path(ui) /
                                       "index.html")) {
    http.set_mount_point("/", ui);
    http.Get("/", [ui](const auto &, auto &res) {
      std::ifstream f(std::filesystem::path(ui) / "index.html");
      std::ostringstream b;
      b << f.rdbuf();
      res.set_content(b.str(), "text/html");
      res.set_header("Cache-Control", "no-store");
    });
  }
  for (const std::string prefix : {"/api/v0/", "/api/v1/", "/v0/", "/v1/"}) {
    http.Get(prefix + "juiced/admission", [this](const auto&, auto& res) {
      reply(res, server_.router_->admission_snapshot());
    });
    post(prefix + "juiced/admission", [this](const auto& req, auto& res) {
      try {
        auto body = json::parse(req.body);
        if (!body.is_object() || body.size() != 1 || !body.contains("paused") || !body.at("paused").is_boolean())
          throw std::invalid_argument("Expected only a boolean paused setting");
        server_.router_->pause_admission(body.at("paused").template get<bool>());
        reply(res, server_.router_->admission_snapshot());
      } catch (const std::exception& error) { reply(res, {{"error", error.what()}}, 400); }
    });
    http.Get(prefix + "juiced/capabilities", [this](const auto &q, auto &r) { capabilities_api(q, r); });
    http.Get(prefix + "juiced/preparation", [this](const auto &q, auto &r) { preparation_api(q, r); });
    post(prefix + "juiced/preparation", [this](const auto &q, auto &r) { preparation_api(q, r); });
    http.Get(prefix + "juiced/execution", [this](const auto &q, auto &r) { execution_api(q, r); });
    http.Get(prefix + "juiced/suitability", [this](const auto& req, auto& res) {
      try { reply(res, model_suitability(req.get_param_value("model"))); }
      catch (const std::invalid_argument& error) { reply(res, {{"error", error.what()}}, 400); }
      catch (const std::exception& error) { reply(res, {{"error", error.what()}}, 400); }
    });
    post(prefix + "juiced/execution", [this](const auto &q, auto &r) { execution_api(q, r); });
    http.Get(prefix + "juiced/devices",
             [this](const auto &q, auto &r) { execution_devices(q, r); });
    http.Get(prefix + "audio/voices",
             [this](const auto &q, auto &r) { handle(q, r, "voices"); });
    for (const std::string operation :
         {"catalog", "state", "threads", "thread", "cache", "imports",
          "transfers", "removal", "requests"})
      http.Get(prefix + "juiced/" + operation,
               [this, operation](const auto &q, auto &r) {
                 handle(q, r, operation);
               });
    for (const std::string operation :
         {"operation", "policy", "thread", "cache", "cache/forget",
          "cache/profile", "cache/qualify", "cache/retain", "cache/recover", "removal"})
      post(prefix + "juiced/" + operation,
                [this, operation](const auto &q, auto &r) {
                  handle(q, r, operation);
                });
    for (const std::string operation :
         {"start", "chunk", "finish", "cancel", "server"})
      post(prefix + "juiced/import/" + operation,
                [this, operation](const auto &q, auto &r) {
                  import_file(q, r, operation);
                });
  }
  http.Get("/.well-known/juiced.json", [this](const auto &, auto &r) {
    reply(r, {{"name", "Juiced"},
              {"version", service_version()},
              {"catalog", "/v1/juiced/catalog"},
              {"capabilities", "/v1/juiced/capabilities"},
              {"capability_schema", 1},
              {"protocols", "/v1/juiced/protocols"},
              {"inference", "/v1"},
              {"ui", "/"}});
  });
}
void JuicedService::handle(const httplib::Request &req, httplib::Response &res,
                           const std::string &op) {
  try {
    auto writer = req.method == "GET" ? ResourceAdmission::WriteScope{} :
        server_.router_->admission_view().write_scope("juiced-mutation");
    if (op == "requests") {
      std::lock_guard<std::mutex> lock(requests_mutex_);
      json records = json::array();
      for (auto it = requests_.rbegin(); it != requests_.rend(); ++it)
        records.push_back(*it);
      reply(res, {{"data", records}, {"capacity", 200},
                  {"content_captured", false}, {"retention", "memory since manager restart"}});
      return;
    }
    json body = req.body.empty() ? json::object() : json::parse(req.body);
    if (op == "removal") {
      remove_model(req, res);
      return;
    }
    if (op == "voices") {
      const auto requested = req.get_param_value("model");
      if (requested.empty())
        throw std::invalid_argument("Specify a speech model");
      const auto info = server_.model_manager_->get_model_info(requested);
      const auto name = server_.model_manager_->get_public_model_name(
          server_.model_manager_->resolve_model_name(requested));
      if (info.recipe != "kokoro") {
        reply(res, {{"error", "This backend does not expose voice discovery"}},
              501);
        return;
      }
      std::string address;
      for (const auto &loaded : server_.router_->get_all_loaded_models())
        if (loaded.value("model_name", "") == name) {
          address = loaded.value("backend_url", "");
          break;
        }
      if (address.empty()) {
        reply(res,
              {{"error", "Load the speech model before listing its voices"}},
              409);
        return;
      }
      if (address.size() >= 3 && address.substr(address.size() - 3) == "/v1")
        address.resize(address.size() - 3);
      try {
        auto response =
            utils::HttpClient::get(address + "/v1/audio/voices", {}, 10,
                                   utils::HttpSecurityPolicy::TrustedLoopback);
        if (response.status_code != 200)
          throw std::runtime_error(
              "Voice discovery failed at the speech backend");
        auto data = json::parse(response.body);
        if (!data.contains("voices") || !data["voices"].is_array())
          throw std::runtime_error(
              "Speech backend returned an invalid voice list");
        std::set<std::string> voices;
        for (const auto &voice : data["voices"])
          if (voice.is_string())
            voices.insert(voice.get<std::string>());
        reply(res,
              {{"model", name}, {"voices", voices}, {"source", "backend"}});
      } catch (const std::exception &e) {
        reply(res, {{"error", e.what()}}, 502);
      }
      return;
    }
    if (op == "catalog") {
      std::lock_guard<std::mutex> lock(mutex_);
      auto data = catalog_;
      for (auto &m : data) {
        auto name = m.value("id", "");
        m["policy"] = state_["policies"].value(name, "disabled");
        if (operations_.contains(name))
          m["operation"] = operations_[name];
      }
      reply(res, {{"object", "list"},
                  {"data", data},
                  {"revision", state_["revision"]}});
      return;
    }
    if (op == "state") {
      std::lock_guard<std::mutex> lock(mutex_);
      reply(res, {{"version", service_version()},
                  {"revision", state_["revision"]},
                  {"operations", operations_},
                  {"catalog_ready", !catalog_.empty()}});
      return;
    }
    if (op == "operation" || op == "policy") {
      auto model = body.at("model").get<std::string>();
      model = server_.model_manager_->get_public_model_name(
          server_.model_manager_->resolve_model_name(model));
      server_.model_manager_->get_model_info(model);
      if (op == "policy") {
        const auto policy = body.at("policy").get<std::string>();
        if (policy != "hot" && policy != "on-demand" && policy != "disabled")
          throw std::invalid_argument("Unknown loading policy");
        if (policy != "disabled" && is_omni_collection_recipe(
                server_.model_manager_->get_model_info(model).recipe))
          throw std::invalid_argument("Agent collections are outside Juiced model services");
        std::lock_guard<std::mutex> effects(policy_effect_mutex_);
        uint64_t revision;
        {
          std::lock_guard<std::mutex> lock(mutex_);
          if (body.contains("revision") &&
              body["revision"] != state_["revision"]) {
            reply(res,
                  {{"error", "Policy changed elsewhere; refresh and retry"}},
                  409);
            return;
          }
          if (operations_.contains(model) &&
              operations_[model].value("state", "") == "removing") {
            reply(res, {{"error", "Wait for model removal to finish"}}, 409);
            return;
          }
          set_policy_locked(model, policy);
          revision = policy_revision_locked(model);
        }
        if (policy_effect_barrier_) policy_effect_barrier_();
        apply_policy_pin_locked(model, revision);
        reply(res, {{"accepted", true}});
        return;
      }
      const auto action = body.at("action").get<std::string>();
      if (action != "load" && action != "unload" && action != "reload")
        throw std::invalid_argument("Unknown operation");
      if (action == "reload") {
        if (!allows(model) || !server_.router_->is_model_loaded(model)) {
          reply(res, {{"error", "Enable and load this model before reloading"}},
                409);
          return;
        }
        for (const auto &loaded : server_.router_->get_all_loaded_models())
          if (loaded.value("model_name", "") == model &&
              loaded.value("is_busy", false)) {
            reply(res,
                  {{"error",
                    "Model is busy; retry after active requests finish"}},
                  409);
            return;
          }
      }
      {
        std::lock_guard<std::mutex> lock(mutex_);
        if (action == "unload" &&
            state_["policies"].value(model, "disabled") == "hot") {
          reply(res,
                {{"error", "Change Hot policy to On demand before unloading"}},
                409);
          return;
        }
      }
      enqueue({{"model", model}, {"action", action}});
      reply(res, {{"accepted", true}}, 202);
      return;
    }
    if (op == "threads" || op == "thread") {
      reply(res,
            {{"error", "Juiced does not store conversations. Downstream "
                       "clients own chat history."}},
            410);
      return;
    }
    if (op == "cache" || op == "cache/forget" || op == "cache/profile" ||
        op == "cache/qualify" || op == "cache/retain" || op == "cache/recover") {
      cache_api(req, res, op);
      return;
    }
    if (op == "transfers") {
      std::lock_guard<std::mutex> lock(mutex_);
      json jobs = json::array();
      for (const auto &[id, j] : state_["transfers"].items())
        jobs.push_back(j);
      reply(res, {{"data", jobs}});
      return;
    }
    if (op == "imports") {
      std::lock_guard<std::mutex> lock(mutex_);
      reply(res, {{"data", state_["imports"]}});
      return;
    }
    reply(res, {{"error", "Unknown operation"}}, 404);
  } catch (const AdmissionError& e) {
    if (e.retry_after()) res.set_header("Retry-After", std::to_string(e.retry_after()));
    reply(res, e.to_json(), e.status);
  } catch (const json::exception &e) {
    reply(res, {{"error", e.what()}}, 400);
  } catch (const std::invalid_argument &e) {
    reply(res, {{"error", e.what()}}, 400);
  } catch (const std::exception &e) {
    reply(res, {{"error", e.what()}}, 500);
  }
}
void JuicedService::publish_import(const std::string& id) {
  auto writer = server_.router_->admission_view().write_scope("import-publication");
  auto admission = server_.router_->admit_management(id, "import-publication");
  json job;
  { std::lock_guard<std::mutex> lock(mutex_); job = state_["imports"].at(id); }
  const auto intent = job.at("publication");
  auto mutation = server_.model_manager_->model_mutation(intent.at("name").get<std::string>(), false);
  if (!mutation.owns_lock()) throw AdmissionError("model_mutation_busy", "Wait for the current model mutation to finish", 409);
  const auto source = directory_ / "imports" / (id + ".part");
  const auto destination = std::filesystem::path(intent.at("destination").get<std::string>());
  const auto valid = [&](const std::filesystem::path& path) {
    std::error_code ec;
    const auto bytes = std::filesystem::file_size(path, ec);
    return !ec && bytes == job.at("size").get<uint64_t>() && file_digest(path) == intent.at("checksum").get<std::string>();
  };
  if (!valid(destination)) {
    if (!valid(source)) throw std::runtime_error("Import publication file needs recovery; ownership retained");
    std::filesystem::create_directories(destination.parent_path());
    std::error_code ec;
    if (!utils::atomic_replace_file(source, destination, ec))
      throw std::runtime_error("Cannot publish imported artifact: " + ec.message());
  }
  if (import_publication_barrier_) import_publication_barrier_("renamed");
  try {
    server_.model_manager_->register_user_model(intent.at("name"), intent.at("definition"), "local_path", false, id);
  } catch (const ModelRegistrationConflict& error) {
    std::lock_guard<std::mutex> lock(mutex_);
    state_["imports"][id]["publication"]["conflict"] = true;
    state_["imports"][id]["last_error"] = error.what();
    persist_locked();
    throw;
  }
  if (import_publication_barrier_) import_publication_barrier_("registered");
  const auto registered = server_.model_manager_->get_model_info(intent.at("name").get<std::string>());
  if (!registered.extras.count("juiced_import") || registered.extras.at("juiced_import") != id ||
      registered.checkpoint() != intent.at("destination").get<std::string>())
    throw std::runtime_error("Import registration changed before final publication; ownership retained");
  std::lock_guard<std::mutex> lock(mutex_);
  if (state_["imports"].at(id).at("publication") != intent || state_["imports"].at(id).at("status") != "publishing")
    throw std::runtime_error("Import intent changed before final publication");
  state_["imports"][id]["status"] = "complete";
  state_["imports"][id]["model"] = intent.at("name");
  state_["imports"][id].erase("last_error");
  persist_locked();
}
void JuicedService::reconcile_imports() {
  json jobs;
  { std::lock_guard<std::mutex> lock(mutex_); jobs = state_["imports"]; }
  for (const auto& [id, job] : jobs.items()) {
    const auto status = job.value("status", "");
    if (status == "publishing" && job.at("publication").value("conflict", false)) continue;
    if (status != "publishing" && !(status == "cancelled" && job.value("cleanup_pending", false))) continue;
    httplib::Request request; request.method = "POST"; request.body = json{{"id", id}}.dump();
    httplib::Response response;
    import_file(request, response, status == "publishing" ? "finish" : "cancel");
  }
}
void JuicedService::import_file(const httplib::Request &req,
                                httplib::Response &res, const std::string &op) {
  MutationClaim claim{mutex_, active_imports_};
  const auto published_response = [&](const std::string& name, const std::string& path) {
    std::string policy;
    { std::lock_guard<std::mutex> lock(mutex_); policy = state_["policies"].value(name, "disabled"); }
    reply(res, {{"model", name}, {"policy", policy}, {"loaded", server_.router_->is_model_loaded(name)}, {"path", path}}, 201);
  };
  try {
    auto writer = server_.router_->admission_view().write_scope("import-" + op);
    ResourceAdmission::Lease admission;
    if (op != "cancel") admission = server_.router_->admit_management("import", "import-" + op);
    if (op == "chunk") {
      const auto id = req.get_param_value("id");
      if (!safe_name(id)) throw std::invalid_argument("Invalid upload identity");
      const auto raw_offset = req.get_param_value("offset");
      if (raw_offset.empty() || raw_offset.size() > 12 ||
          !std::all_of(raw_offset.begin(), raw_offset.end(), [](char c) { return c >= '0' && c <= '9'; }))
        throw std::invalid_argument("Offset must be a nonnegative upload byte position");
      const auto offset = std::stoull(raw_offset);
      if (offset > 100ULL * 1024 * 1024 * 1024) throw std::invalid_argument("Offset exceeds the upload size limit");
      if (req.body.empty() || req.body.size() > 8 * 1024 * 1024)
        throw std::invalid_argument("Use chunks between 1 byte and 8 MiB");
      std::lock_guard<std::mutex> lock(mutex_);
      if (!state_["imports"].contains(id)) throw std::invalid_argument("Unknown upload");
      auto& job = state_["imports"][id];
      if (active_imports_.count(id)) { reply(res, {{"error", "Import publication is in progress"}}, 409); return; }
      if (job.value("status", "") != "uploading") throw std::invalid_argument("Upload is not writable");
      auto file = directory_ / "imports" / (id + ".part");
      const auto actual = std::filesystem::exists(file) ? std::filesystem::file_size(file) : 0;
      if (offset != actual) { reply(res, {{"error", "Offset mismatch"}, {"offset", actual}}, 409); return; }
      if (actual + req.body.size() > job.at("size").get<uint64_t>()) throw std::invalid_argument("Chunk exceeds declared file size");
      std::ofstream f(file, std::ios::binary | std::ios::app);
      f.write(req.body.data(), req.body.size()); f.close();
      if (!f) throw std::runtime_error("Could not store upload chunk");
      job["received"] = actual + req.body.size(); persist_locked(); reply(res, job); return;
    }
    auto body = json::parse(req.body);
    if (op == "start") {
      const auto filename = body.at("filename").get<std::string>();
      if (!safe_name(filename) || std::filesystem::path(filename).extension() != ".gguf")
        throw std::invalid_argument("Upload a GGUF filename without directories");
      const auto size = body.at("size").get<uint64_t>();
      const auto free = std::filesystem::space(directory_).available;
      if (size < 24 || size > 100ULL * 1024 * 1024 * 1024 || free < size + 1024ULL * 1024 * 1024)
        throw std::invalid_argument("Invalid size or insufficient free disk");
      const auto id = identifier();
      json job = {{"id", id}, {"filename", filename}, {"size", size}, {"received", 0}, {"status", "uploading"}, {"created", now()}};
      {
        std::lock_guard<std::mutex> lock(mutex_);
        uint64_t reserved = 0;
        for (const auto& [key, previous] : state_["imports"].items())
          if (previous.value("status", "") == "uploading")
            reserved += previous.value("size", uint64_t(0)) - previous.value("received", uint64_t(0));
        if (reserved > free || size + 1024ULL * 1024 * 1024 > free - reserved)
          throw std::invalid_argument("Insufficient free disk after pending uploads");
        state_["imports"][id] = job; persist_locked();
      }
      reply(res, job, 201); return;
    }
    std::string id;
    json job;
    if (op == "finish" || op == "cancel") {
      id = body.at("id").get<std::string>();
      if (!safe_name(id)) throw std::invalid_argument("Invalid upload identity");
      if (!claim.acquire(id)) { reply(res, {{"error", "Import mutation is in progress; retry"}}, 409); return; }
      { std::lock_guard<std::mutex> lock(mutex_); job = state_["imports"].at(id); }
    }
    if (op == "cancel") {
      const auto status = job.value("status", "");
      if (status == "complete" || status == "publishing") {
        reply(res, {{"error", "Published import intent cannot be cancelled; finish recovery, then use model removal"}}, 409); return;
      }
      {
        std::lock_guard<std::mutex> lock(mutex_);
        state_["imports"][id]["status"] = "cancelled";
        state_["imports"][id]["cleanup_pending"] = true;
        persist_locked();
      }
      std::error_code ec;
      std::filesystem::remove(directory_ / "imports" / (id + ".part"), ec);
      {
        std::lock_guard<std::mutex> lock(mutex_);
        state_["imports"][id]["cleanup_pending"] = bool(ec);
        persist_locked();
        reply(res, state_["imports"][id], ec ? 202 : 200);
      }
      return;
    }
    if (op != "finish" && op != "server") { reply(res, {{"error", "Unknown import operation"}}, 404); return; }
    if (op == "finish" && (job.value("status", "") == "publishing" || job.value("status", "") == "complete")) {
      auto intent = job.at("publication");
      const auto committed_recipe = intent.at("definition").at("recipe").get<std::string>();
      if ((body.contains("recipe") && body.at("recipe") != intent.at("definition").at("recipe")) ||
          (body.contains("purpose") && import_labels(committed_recipe, body.at("purpose").get<std::string>()) != intent.at("definition").at("labels")))
        throw std::invalid_argument("Retry must retain the committed import recipe and purpose");
      if (job.at("status") == "publishing" && intent.value("conflict", false) && body.contains("name") && body.at("name") != intent.at("name")) {
        const auto name = body.at("name").get<std::string>();
        if (!safe_name(name)) throw std::invalid_argument("Use a model name without path separators");
        intent["name"] = name;
        intent.erase("conflict");
        std::lock_guard<std::mutex> lock(mutex_);
        state_["imports"][id]["publication"] = intent;
        persist_locked();
      }
      if (body.contains("name") && body.at("name") != intent.at("name"))
        throw std::invalid_argument("Retry must retain the committed import definition");
      if (job.at("status") == "publishing") publish_import(id);
      else {
        const auto name = intent.at("name").get<std::string>();
        if (!server_.model_manager_->model_exists(name) || !std::filesystem::is_regular_file(intent.at("destination").get<std::string>())) {
          reply(res, {{"error", "The completed import was removed or its artifact is unavailable"}}, 410);
          return;
        }
        const auto current = server_.model_manager_->get_model_info(name);
        if (!current.extras.count("juiced_import") || current.extras.at("juiced_import") != id) {
          reply(res, {{"error", "The model registration changed after this import completed"}}, 409);
          return;
        }
      }
      published_response(intent.at("name"), intent.at("destination"));
      return;
    }
    std::filesystem::path file;
    if (op == "finish") {
      file = directory_ / "imports" / (id + ".part");
      if (job.value("status", "") != "uploading" || std::filesystem::file_size(file) != job.at("size").get<uint64_t>())
        throw std::invalid_argument("Upload is incomplete");
    } else {
      std::error_code ec;
      file = std::filesystem::canonical(body.at("path").get<std::string>(), ec);
      if (ec) throw std::invalid_argument("Import an existing regular file beneath /library");
      const auto allowed = std::filesystem::canonical("/library", ec);
      if (ec) throw std::invalid_argument("Server library imports are unavailable on this deployment");
      const auto rel = file.lexically_relative(allowed);
      if (rel.empty() || *rel.begin() == ".." || !std::filesystem::is_regular_file(file))
        throw std::invalid_argument("Import only regular files beneath /library");
    }
    GgufMetadata metadata;
    if (!read_gguf_metadata(metadata, file.string()) || metadata.architecture.empty())
      throw std::invalid_argument("Incomplete or unsupported GGUF metadata; use a valid complete model");
    const auto recipe = body.value("recipe", "llamacpp");
    const auto purpose = body.value("purpose", "chat");
    const auto labels = import_labels(recipe, purpose);
    const auto name = body.value("name", op == "finish" ? std::filesystem::path(job.at("filename").get<std::string>()).stem().string() : file.stem().string());
    if (!safe_name(name)) throw std::invalid_argument("Use the proper model name without path separators");
    const auto destination = op == "finish" ? std::filesystem::absolute(directory_ / "artifacts" / id / job.at("filename").get<std::string>()) : file;
    const json definition = {{"recipe", recipe}, {"source", "local_path"}, {"checkpoints", {{"main", destination.string()}}},
                             {"labels", labels}, {"size", std::filesystem::file_size(file) / 1e9}};
    if (op == "finish") {
      if (import_publication_barrier_) import_publication_barrier_("validated");
      const auto checksum = file_digest(file);
      {
        std::lock_guard<std::mutex> lock(mutex_);
        state_["imports"][id]["status"] = "publishing";
        state_["imports"][id]["publication"] = {{"destination", destination.string()}, {"name", name}, {"definition", definition}, {"checksum", checksum}};
        persist_locked();
      }
      if (import_publication_barrier_) import_publication_barrier_("sealed");
      publish_import(id);
    } else server_.model_manager_->register_user_model(name, definition, "local_path", false);
    wake_.notify_all();
    published_response(name, destination.string());
  } catch (const AdmissionError& error) {
      if (error.retry_after()) res.set_header("Retry-After", std::to_string(error.retry_after()));
      reply(res, error.to_json(), error.status);
    }
    catch (const ModelRegistrationConflict& error) { reply(res, {{"error", error.what()}, {"retry", "Choose a distinct model name when finishing this import"}}, 409); }
    catch (const std::invalid_argument& error) { reply(res, {{"error", error.what()}}, 400); }
    catch (const json::exception& error) { reply(res, {{"error", error.what()}}, 400); }
    catch (const std::exception& error) {
      try {
        std::lock_guard<std::mutex> lock(mutex_);
        if (!claim.id.empty() && state_["imports"].at(claim.id).value("status", "") == "publishing" &&
            state_["imports"].at(claim.id).value("last_error", "") != error.what()) {
          state_["imports"][claim.id]["last_error"] = error.what();
          persist_locked();
        }
      } catch (...) {}
      reply(res, {{"error", error.what()}}, 500);
    }
}
} // namespace lemon
