#include "lemon/juiced/service.h"
#include "lemon/config_file.h"
#include "lemon/gguf_reader.h"
#include "lemon/model_manager.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "lemon/utils/http_client.h"
#include "lemon/utils/path_utils.h"
#include <algorithm>
#include <chrono>
#include <fstream>
#include <iomanip>
#include <random>
#include <set>
#include <sstream>

namespace lemon {
namespace {
using json = nlohmann::json;
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
    if (!f)
      throw std::runtime_error("Could not save state");
  }
  std::filesystem::rename(tmp, p);
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
  if (!state_.contains("transfers"))
    state_["transfers"] = json::object();
  for (auto &[id, j] : state_["transfers"].items())
    if (j.value("running", false)) {
      j["running"] = false;
      j["status"] = "interrupted";
    }
  const char *snapshots = std::getenv("JUICED_SNAPSHOT_DIR");
  snapshot_directory_ =
      snapshots ? std::filesystem::path(snapshots) : directory_ / "snapshots";
  std::filesystem::create_directories(snapshot_directory_);
  if (!state_.contains("snapshots"))
    state_["snapshots"] = json::object();
  if (!state_.contains("qualified_profiles"))
    state_["qualified_profiles"] = json::object();
  if (!state_.contains("preparations")) state_["preparations"] = json::object();
  for (auto &[id, job] : state_["preparations"].items())
    if (job.value("pending", false)) {
      job["pending"] = false;
      job["phase"] = "interrupted";
    }
  if (!state_.contains("execution_trials")) state_["execution_trials"] = json::object();
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
  worker_ = std::thread([this] { run(); });
  preparation_worker_ = std::thread([this] { preparation_run(); });
}
JuicedService::~JuicedService() { stop(); }
void JuicedService::stop() {
  stopping_ = true;
  wake_.notify_all();
  if (worker_.joinable())
    worker_.join();
  if (preparation_worker_.joinable()) preparation_worker_.join();
  voice_process_.reset();
}
void JuicedService::persist_locked() {
  state_["revision"] = state_.value("revision", 0) + 1;
  atomic_json(directory_ / "state.json", state_);
}
bool JuicedService::allows(const std::string &requested) const {
  auto model = server_.model_manager_->get_public_model_name(
      server_.model_manager_->resolve_model_name(requested));
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
  auto model = server_.model_manager_->get_public_model_name(
      server_.model_manager_->resolve_model_name(requested));
  server_.model_manager_->get_model_info(model);
  server_.model_manager_->update_saved_model_options(model,
                                                     {{"pinned", pinned}});
  std::lock_guard<std::mutex> lock(mutex_);
  if (pinned || state_["policies"].value(model, "disabled") != "disabled")
    state_["policies"][model] = pinned ? "hot" : "on-demand";
  persist_locked();
  wake_.notify_all();
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
      last == "classify" || last == "generations" || last == "speech";
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
  std::lock_guard<std::mutex> lock(mutex_);
  auto m = command.at("model").get<std::string>();
  if (operations_.contains(m) && operations_[m].value("pending", false))
    throw std::invalid_argument("A model operation is already pending");
  pending_.push_back(command);
  operations_[m] = {
      {"state", command.value("action", "") == "load" ? "queued" : "draining"},
      {"pending", true},
      {"updated", now()}};
  wake_.notify_all();
}
void JuicedService::refresh() {
  auto all = server_.model_manager_->get_supported_models();
  auto loaded = server_.router_->get_all_loaded_models();
  json result = json::array();
  for (const auto &[id, info] : all) {
    auto m = server_.model_info_to_json(id, info);
    m["service"] = service(m);
    m["execution_profile"] = server_.model_manager_->get_execution_profiles(id).value("selected", "registered");
    m["loaded"] = false;
    m["state"] = "unloaded";
    for (const auto &l : loaded)
      if (l.value("model_name", "") == m.value("id", "")) {
        m["loaded"] = true;
        m["state"] = l.value("is_busy", false) ? "busy" : "ready";
        m["runtime"] = {{"pid", l.value("pid", 0LL)},
                        {"backend_url", l.value("backend_url", "")},
                        {"is_busy", l.value("is_busy", false)},
                        {"pinned", l.value("pinned", false)}};
        auto applied = server_.router_->get_model_recipe_options(m["id"])
                           .to_resolved_json();
        auto desired = server_.router_
                           ->resolve_effective_options(
                               info, RecipeOptions(info.recipe, json::object()))
                           .to_resolved_json();
        applied.erase("pinned");
        desired.erase("pinned");
        if (desired.value("ctx_size", json(nullptr)) == -1) {
          applied.erase("ctx_size");
          desired.erase("ctx_size");
        }
        m["runtime"]["settings_pending"] = applied != desired;
        break;
      }
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
  const auto name = command.at("model").get<std::string>();
  const auto action = command.at("action").get<std::string>();
  json previous_profile;
  bool profile_changed = false, trial_started = false, was_loaded = false;
  try {
    {
      std::lock_guard<std::mutex> lock(mutex_);
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
          policy == "hot", LoadPurpose::UserInference, &stopping_);
    } else if (action == "unload") {
      auto active = server_.router_->get_all_loaded_models();
      for (const auto &l : active)
        if (l.value("model_name", "") == name && l.value("is_busy", false))
          throw std::runtime_error(
              "Model is busy; retry after active requests finish");
      server_.router_->unload_model(name);
    }
    {
      std::lock_guard<std::mutex> lock(mutex_);
      operations_.erase(name);
      if (trial_started) { state_["execution_trials"].erase(name); persist_locked(); }
    }
  } catch (const std::exception &e) {
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
  json policies, catalog, interrupted;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    policies = state_["policies"];
    catalog = catalog_;
    interrupted = state_["execution_trials"];
  }
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
    bool loaded = false, downloaded = false;
    for (const auto &m : catalog)
      if (m.value("id", "") == name) {
        loaded = m.value("loaded", false);
        if (loaded &&
            m.value("runtime", json::object()).value("pinned", false) !=
                (p == "hot"))
          server_.router_->set_model_pinned(name, p == "hot");
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
    if (!pending && retry && p == "hot" && !loaded && downloaded)
      enqueue({{"model", name}, {"action", "load"}});
    if (!pending && retry && p == "disabled" && loaded)
      enqueue({{"model", name}, {"action", "unload"}});
  }
}
void JuicedService::run() {
  while (!stopping_) {
    try {
      json cmd;
      {
        std::lock_guard<std::mutex> lock(mutex_);
        if (!pending_.empty()) {
          cmd = pending_.front();
          pending_.pop_front();
        }
      }
      if (!cmd.is_null())
        execute(cmd);
      refresh();
      {
        std::lock_guard<std::mutex> lock(mutex_);
        auto count = state_["snapshots"].size();
        prune_cache_locked();
        if (count != state_["snapshots"].size())
          persist_locked();
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
    http.Get(prefix + "juiced/preparation", [this](const auto &q, auto &r) { preparation_api(q, r); });
    http.Post(prefix + "juiced/preparation", [this](const auto &q, auto &r) { preparation_api(q, r); });
    http.Get(prefix + "juiced/execution", [this](const auto &q, auto &r) { execution_api(q, r); });
    http.Post(prefix + "juiced/execution", [this](const auto &q, auto &r) { execution_api(q, r); });
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
          "cache/profile", "cache/qualify", "cache/retain", "removal"})
      http.Post(prefix + "juiced/" + operation,
                [this, operation](const auto &q, auto &r) {
                  handle(q, r, operation);
                });
    for (const std::string operation :
         {"start", "chunk", "finish", "cancel", "server"})
      http.Post(prefix + "juiced/import/" + operation,
                [this, operation](const auto &q, auto &r) {
                  import_file(q, r, operation);
                });
  }
  http.Get("/.well-known/juiced.json", [](const auto &, auto &r) {
    reply(r, {{"name", "Juiced"},
              {"version", "0.1.0"},
              {"catalog", "/v1/juiced/catalog"},
              {"protocols", "/v1/juiced/protocols"},
              {"inference", "/v1"},
              {"ui", "/"}});
  });
}
void JuicedService::handle(const httplib::Request &req, httplib::Response &res,
                           const std::string &op) {
  try {
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
      reply(res, {{"version", "0.1.0"},
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
          state_["policies"][model] = policy;
          persist_locked();
        }
        server_.model_manager_->update_saved_model_options(
            model, {{"pinned", policy == "hot"}});
        if (server_.router_->is_model_loaded(model))
          server_.router_->set_model_pinned(model, policy == "hot");
        wake_.notify_all();
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
        op == "cache/qualify" || op == "cache/retain") {
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
  } catch (const json::exception &e) {
    reply(res, {{"error", e.what()}}, 400);
  } catch (const std::invalid_argument &e) {
    reply(res, {{"error", e.what()}}, 400);
  } catch (const std::exception &e) {
    reply(res, {{"error", e.what()}}, 500);
  }
}
void JuicedService::import_file(const httplib::Request &req,
                                httplib::Response &res, const std::string &op) {
  try {
    if (op == "chunk") {
      const auto id = req.get_param_value("id");
      if (!safe_name(id))
        throw std::invalid_argument("Invalid upload identity");
      const auto offset = std::stoull(req.get_param_value("offset"));
      if (req.body.empty() || req.body.size() > 8 * 1024 * 1024)
        throw std::invalid_argument("Use chunks between 1 byte and 8 MiB");
      std::lock_guard<std::mutex> lock(mutex_);
      if (!state_["imports"].contains(id))
        throw std::invalid_argument("Unknown upload");
      auto &job = state_["imports"][id];
      if (job.value("status", "") != "uploading")
        throw std::invalid_argument("Upload is not writable");
      auto file = directory_ / "imports" / (id + ".part");
      const auto actual =
          std::filesystem::exists(file) ? std::filesystem::file_size(file) : 0;
      if (offset != actual) {
        reply(res, {{"error", "Offset mismatch"}, {"offset", actual}}, 409);
        return;
      }
      if (actual + req.body.size() > job.at("size").get<uint64_t>())
        throw std::invalid_argument("Chunk exceeds declared file size");
      std::ofstream f(file, std::ios::binary | std::ios::app);
      f.write(req.body.data(), req.body.size());
      f.close();
      if (!f)
        throw std::runtime_error("Could not store upload chunk");
      job["received"] = actual + req.body.size();
      persist_locked();
      reply(res, job);
      return;
    }
    auto body = json::parse(req.body);
    if (op == "start") {
      const auto filename = body.at("filename").get<std::string>();
      if (!safe_name(filename) ||
          std::filesystem::path(filename).extension() != ".gguf")
        throw std::invalid_argument(
            "Upload a GGUF filename without directories");
      const auto size = body.at("size").get<uint64_t>();
      auto free = std::filesystem::space(directory_).available;
      if (size < 24 || size > 100ULL * 1024 * 1024 * 1024 ||
          free < size + 1024ULL * 1024 * 1024)
        throw std::invalid_argument("Invalid size or insufficient free disk");
      const auto id = identifier();
      json job = {{"id", id},      {"filename", filename},  {"size", size},
                  {"received", 0}, {"status", "uploading"}, {"created", now()}};
      {
        std::lock_guard<std::mutex> lock(mutex_);
        uint64_t reserved = 0;
        for (const auto &[key, previous] : state_["imports"].items())
          if (previous.value("status", "") == "uploading")
            reserved += previous.value("size", uint64_t(0)) -
                        previous.value("received", uint64_t(0));
        if (reserved > free || size + 1024ULL * 1024 * 1024 > free - reserved)
          throw std::invalid_argument(
              "Insufficient free disk after pending uploads");
        state_["imports"][id] = job;
        persist_locked();
      }
      reply(res, job, 201);
      return;
    }
    if (op == "cancel") {
      const auto id = body.at("id").get<std::string>();
      if (!safe_name(id))
        throw std::invalid_argument("Invalid upload identity");
      std::lock_guard<std::mutex> lock(mutex_);
      if (!state_["imports"].contains(id))
        throw std::invalid_argument("Unknown upload");
      auto &j = state_["imports"][id];
      if (j.value("status", "") == "complete")
        throw std::invalid_argument(
            "Completed models cannot be removed through cancel");
      std::filesystem::remove(directory_ / "imports" / (id + ".part"));
      j["status"] = "cancelled";
      persist_locked();
      reply(res, j);
      return;
    }
    if (op == "finish" || op == "server") {
      std::filesystem::path file;
      std::string id;
      if (op == "finish") {
        id = body.at("id").get<std::string>();
        if (!safe_name(id))
          throw std::invalid_argument("Invalid upload identity");
        std::lock_guard<std::mutex> lock(mutex_);
        auto j = state_["imports"].at(id);
        file = directory_ / "imports" / (id + ".part");
        if (j.value("status", "") != "uploading" ||
            std::filesystem::file_size(file) != j.at("size").get<uint64_t>())
          throw std::invalid_argument("Upload is incomplete");
      } else {
        file = std::filesystem::canonical(body.at("path").get<std::string>());
        auto allowed = std::filesystem::canonical("/library");
        auto rel = file.lexically_relative(allowed);
        if (rel.empty() || *rel.begin() == ".." ||
            !std::filesystem::is_regular_file(file))
          throw std::invalid_argument(
              "Import only regular files beneath /library");
      }
      {
        std::ifstream f(file, std::ios::binary);
        char magic[4];
        f.read(magic, 4);
        if (!f || std::string(magic, 4) != "GGUF")
          throw std::invalid_argument("The file does not have a GGUF header");
      }
      GgufMetadata metadata;
      if (!read_gguf_metadata(metadata, file.string()) ||
          metadata.architecture.empty())
        throw std::invalid_argument("Incomplete or unsupported GGUF metadata; "
                                    "use a valid complete model");
      const auto recipe = body.value("recipe", "llamacpp");
      if (recipe != "llamacpp" && recipe != "asr4all" && recipe != "parakeet" &&
          recipe != "whispercpp")
        throw std::invalid_argument("Unsupported GGUF backend");
      const auto purpose = body.value("purpose", "chat");
      if (recipe == "llamacpp" && purpose != "chat" && purpose != "embedding" &&
          purpose != "reranking")
        throw std::invalid_argument(
            "Choose Chat, Embeddings or Rerank for llama.cpp imports");
      auto name = body.value("name", file.stem().string());
      if (!safe_name(name))
        throw std::invalid_argument(
            "Use the proper model name without path separators");
      auto stored = read_json(std::filesystem::path(server_.config_dir_) /
                                  "user_models.json",
                              json::object());
      if (server_.model_manager_->model_exists(name) || stored.contains(name))
        throw std::invalid_argument(
            "That model name is already registered. Use a distinct "
            "publisher/quant variant name; working models are not overwritten "
            "by import.");
      if (op == "finish") {
        auto dir = directory_ / "artifacts" / id;
        std::filesystem::create_directories(dir);
        json j;
        {
          std::lock_guard<std::mutex> lock(mutex_);
          j = state_["imports"][id];
        }
        auto dest = dir / j.at("filename").get<std::string>();
        std::filesystem::rename(file, dest);
        file = dest;
      }
      auto labels = recipe == "llamacpp" ? json::array({purpose})
                                         : json::array({"transcription"});
      json model = {{"recipe", recipe},
                    {"source", "local_path"},
                    {"checkpoints", {{"main", file.string()}}},
                    {"labels", labels},
                    {"size", std::filesystem::file_size(file) / 1e9}};
      try {
        server_.model_manager_->register_user_model(name, model, "local_path");
      } catch (...) {
        if (op == "finish")
          std::filesystem::rename(file,
                                  directory_ / "imports" / (id + ".part"));
        throw;
      }
      if (op == "finish") {
        std::lock_guard<std::mutex> lock(mutex_);
        auto &j = state_["imports"][id];
        j["status"] = "complete";
        j["model"] = name;
        persist_locked();
      }
      wake_.notify_all();
      reply(res,
            {{"model", name},
             {"policy", "disabled"},
             {"loaded", false},
             {"path", file.string()}},
            201);
      return;
    }
    reply(res, {{"error", "Unknown import operation"}}, 404);
  } catch (const std::exception &e) {
    reply(res, {{"error", e.what()}}, 400);
  }
}
} // namespace lemon
