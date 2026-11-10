#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "lemon/request_identity.h"
#include "lemon/utils/content_hash.h"
#include "lemon/utils/http_client.h"
#include "lemon/utils/path_utils.h"
#include "lemon/utils/custom_args.h"
#include <algorithm>
#include <fstream>
#include <iomanip>
#include <mbedtls/md.h>
#include <sstream>
namespace lemon {
namespace {
using json = nlohmann::json;
long long timestamp() {
  return std::chrono::duration_cast<std::chrono::seconds>(
             std::chrono::system_clock::now().time_since_epoch())
      .count();
}
struct PromptStateClaim {
  std::mutex &mutex;
  std::map<std::string, size_t> &operations;
  std::set<std::string> &snapshots;
  const std::string model;
  std::string key;
  bool model_claimed = false, session_claimed = false;
  std::atomic<bool> released{false};

  PromptStateClaim(std::mutex &m, std::map<std::string, size_t> &active,
                   std::set<std::string> &sessions, std::string name,
                   std::string snapshot)
      : mutex(m), operations(active), snapshots(sessions), model(std::move(name)),
        key(std::move(snapshot)) {}
  bool release() {
    if (released.exchange(true)) return false;
    std::lock_guard<std::mutex> lock(mutex);
    if (model_claimed) {
      const auto it = operations.find(model);
      if (it != operations.end() && --it->second == 0) operations.erase(it);
    }
    if (session_claimed) snapshots.erase(key);
    return true;
  }
  ~PromptStateClaim() { release(); }
};

std::string digest(const std::string &data) {
  unsigned char out[32];
  if (mbedtls_md(mbedtls_md_info_from_type(MBEDTLS_MD_SHA256),
                 reinterpret_cast<const unsigned char *>(data.data()),
                 data.size(), out) != 0)
    throw std::runtime_error("Cannot fingerprint prompt state");
  std::ostringstream s;
  for (auto c : out)
    s << std::hex << std::setw(2) << std::setfill('0') << int(c);
  return s.str();
}
void output(httplib::Response &r, const json &j, int code = 200) {
  r.status = code;
  r.set_content(j.dump(), "application/json");
  r.set_header("Cache-Control", "no-store");
}
json post_engine(const std::string &address, const std::string &path,
                 const json &body, WrappedServer::InferenceLease& worker,
                 const std::function<bool()>& disconnected, bool* dispatched = nullptr) {
  if (worker.deadline_expired() || (disconnected && disconnected()))
    throw AdmissionError("state_operation_cancelled", "State operation was cancelled before dispatch", 499);
  if (!worker.endpoint_owned())
    throw AdmissionError("backend_listener_unverified", "The state worker no longer owns its listener", 503);
  if (!worker.runtime_inputs_current())
    throw AdmissionError("runtime_inputs_unverified", "The state worker runtime files no longer match its captured inputs", 503);
  const auto payload = body.dump();
  worker.resources_started();
  utils::HttpResponse r;
  try {
    if (dispatched) *dispatched = true;
    r = utils::HttpClient::post(address + path, payload,
                                 {{"Content-Type", "application/json"}}, 120,
                                 utils::HttpSecurityPolicy::TrustedLoopback,
                                 {nullptr, [&] { return worker.deadline_expired() || (disconnected && disconnected()); }});
  } catch (const std::exception&) {
    throw AdmissionError("state_operation_unresolved", "Engine state operation termination could not be confirmed");
  }
  worker.resources_completed();
  if (!worker.endpoint_owned())
    throw AdmissionError("backend_listener_unverified", "State response listener ownership could not be verified", 503);
  if (!worker.runtime_inputs_current())
    throw AdmissionError("runtime_inputs_unverified", "State response runtime-file association could not be verified", 503);
  if (r.status_code != 200)
    throw std::runtime_error("Engine state operation failed (" +
                             std::to_string(r.status_code) +
                             "): " + r.body.substr(0, 400));
  return json::parse(r.body);
}
json snapshot_owned_files(const std::string& pending, const json& entry) {
  if (entry.contains("files")) return entry.at("files");
  json files = {{pending, entry.value("bytes", uint64_t(0))}};
  if (entry.value("state", "") == "publishing") {
    files[entry.at("destination").get<std::string>()] = entry.at("candidate").value("bytes", uint64_t(0));
    if (!entry.at("previous").empty())
      files[entry.at("backup").get<std::string>()] = entry.at("previous").value("bytes", uint64_t(0));
  }
  return files;
}
std::string prefix_hash(const json &messages, size_t n) {
  json prefix = json::array();
  for (size_t i = 0; i < std::min(n, messages.size()); i++)
    prefix.push_back(messages[i]);
  return digest(prefix.dump());
}
} // namespace
std::string JuicedService::file_digest(const std::filesystem::path &file) {
  return utils::content_sha256(file);
}
JuicedService::json JuicedService::mutate_prompt_slot(
    const std::string& model, int slot, const std::function<json()>& invoke) {
  auto claim = std::make_shared<PromptStateClaim>(mutex_, active_cache_operations_, active_snapshots_, model, "");
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (slot < 0 || cache_mode_locked(model) == "persistent")
      throw AdmissionError("prompt_state_busy", "Manual slot mutation is unavailable while persistent caching owns slots", 409);
    ++active_cache_operations_[model];
    claim->model_claimed = true;
  }
  return invoke();
}

std::string JuicedService::cache_mode_locked(const std::string &model) const {
  const auto &settings = state_.at("cache");
  if (settings.contains("model_modes") &&
      settings["model_modes"].contains(model))
    return settings["model_modes"][model].get<std::string>();
  return settings.value("mode", "memory");
}
bool JuicedService::requires_prompt_state_lease(const std::string &requested) const {
  json request = {{"model", requested}};
  server_.normalize_and_resolve_request_model(request);
  const auto model = server_.model_manager_->get_public_model_name(
      server_.model_manager_->resolve_model_name(request.at("model").get<std::string>()));
  const bool routed = model.empty() || (server_.model_manager_->model_exists(model) &&
      is_router_collection_recipe(server_.model_manager_->get_model_info(model).recipe));
  std::lock_guard<std::mutex> lock(mutex_);
  if (routed) {
    // Classifiers can run before the final target is known. Until routed slot
    // admission is qualified, reject rather than enter any protected worker.
    for (const auto &[fingerprint, profile] : state_["qualified_profiles"].items())
      if (cache_mode_locked(profile.value("model", "")) == "persistent") return true;
    return false;
  }
  if (cache_mode_locked(model) != "persistent") return false;
  for (const auto &[fingerprint, profile] : state_["qualified_profiles"].items())
    if (profile.value("model", "") == model) return true;
  return false;
}
bool JuicedService::unleased_protocol(const httplib::Request &req,
                                      httplib::Response &res) {
  try {
    const auto body = json::parse(req.body);
    if (requires_prompt_state_lease(body.value("model", ""))) {
      output(res,
          {{"error", {{"message", "This deployment uses persistent session slots. Use "
                         "Chat Completions, or select memory cache mode before using this protocol."},
                       {"type", "invalid_request_error"}}}}, 409);
      return true;
    }
  } catch (const std::exception &) {
  }
  return false;
}
json JuicedService::cache_profile(const std::string &requested, WrappedServer::InferenceLease& worker) {
  const auto context = current_request_context();
  const auto cancelled = [&] { return worker.deadline_expired() ||
      (context && context->cancelled && context->cancelled()); };
  if (cancelled()) throw AdmissionError("state_operation_cancelled", "State inspection was cancelled", 499);
  const auto loaded = worker.execution_snapshot();
  auto identity = loaded.value("identity", json::object());
  if (identity.empty() || identity.value("recipe", "") != "llamacpp")
    throw std::invalid_argument("Persistent prompt state requires a captured llama.cpp execution identity");
  const auto name = server_.model_manager_->get_public_model_name(loaded.at("model").get<std::string>());
  identity["profile_schema"] = 4;
  identity["definition"] = loaded.at("definition");
  identity["model"] = name;
  identity["options"] = loaded.at("options");
  identity["options"].erase("pinned");
  auto address = loaded.at("address").get<std::string>();
  if (address.empty())
    throw std::invalid_argument(
        "Load the model before qualifying its prompt-state profile");
  if (address.size() >= 3 && address.substr(address.size() - 3) == "/v1")
    address.resize(
        address.size() -
        3); // Backend inference base includes /v1; state APIs are at its root.
  if (cancelled()) throw AdmissionError("state_operation_cancelled", "State inspection was cancelled before dispatch", 499);
  if (!worker.endpoint_owned())
    throw AdmissionError("backend_listener_unverified", "The inspected worker no longer owns its listener", 503);
  worker.resources_started();
  auto props = utils::HttpClient::get(
      address + "/props", {}, 10, utils::HttpSecurityPolicy::TrustedLoopback, {nullptr, cancelled});
  worker.resources_completed();
  if (cancelled()) throw AdmissionError("state_operation_cancelled", "State inspection was cancelled", 499);
  if (!worker.endpoint_owned())
    throw AdmissionError("backend_listener_unverified", "Inspection response listener ownership could not be verified", 503);
  if (props.status_code != 200)
    throw std::runtime_error("Cannot inspect the engine state format");
  const auto observed_build = json::parse(props.body).value("build_info", std::string("unknown"));
  if (identity.value("engine", std::string("unknown")) != "unknown" &&
      identity.at("engine") != observed_build)
    throw std::runtime_error("Engine build changed after execution identity publication");
  const auto args = utils::build_custom_args_map(utils::parse_custom_args(
      identity["options"].value("llamacpp_args", std::string())));
  const auto directory = args.find("--slot-save-path");
  bool correct_directory = false;
  if (directory != args.end() && directory->second.size() == 1 && directory->second.front().size() == 1) {
    std::error_code ec;
    correct_directory = std::filesystem::equivalent(
        std::filesystem::path(directory->second.front().front()), snapshot_directory_, ec) && !ec;
  }
  const bool runtime_current = worker.runtime_inputs_current();
  if (cancelled()) throw AdmissionError("state_operation_cancelled", "Runtime image inspection was cancelled", 499);
  const bool configured = identity.value("captured", false) && runtime_current && correct_directory && identity["engine"].get<std::string>().find("dde879b") != std::string::npos;
  const std::string configuration_reason = configured ? "" : !identity.value("captured", false)
      ? identity.value("reason", "Execution inputs could not be captured")
      : !runtime_current ? "Runtime executable or loaded-module association changed; reload before qualifying prompt state"
      : !correct_directory ? "Load options must use the Juiced snapshot directory"
      : "This engine build has no qualified prompt-state format";
  auto fingerprint = digest(identity.dump());
  bool qualified;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    qualified =
        configured && state_["qualified_profiles"].contains(fingerprint);
  }
  return {{"model", name},          {"fingerprint", fingerprint},
          {"address", address},     {"configured", configured},
          {"qualified", qualified}, {"configuration_reason", configuration_reason}, {"identity", identity}, {"worker_generation", worker.generation()}};
}
JuicedService::json JuicedService::cache_disk_usage_locked() const {
  uint64_t saved = 0, cleanup = 0, writes = 0;
  const auto budget = uint64_t(state_.at("cache").value("disk_budget_gib", 8.0) * 1024 * 1024 * 1024);
  bool known = true;
  const auto file_bytes = [&](const std::filesystem::path& file, uint64_t declared) {
    std::error_code ec;
    const auto size = std::filesystem::file_size(file, ec);
    if (!ec) return (std::max)(declared, uint64_t(size));
    if (ec == std::errc::no_such_file_or_directory) return declared;
    known = false;
    return (std::max)(declared, budget + 1);
  };
  for (const auto& [id, entry] : state_.at("snapshots").items())
    saved += file_bytes(snapshot_directory_ / (id + ".bin"), entry.value("bytes", uint64_t(0)));
  for (const auto& [id, entry] : state_.at("snapshot_cleanup").items())
    cleanup += file_bytes(snapshot_directory_ / (id + ".bin"), entry.value("bytes", uint64_t(0)));
  for (const auto& [pending, entry] : state_.at("snapshot_writes").items()) {
    const auto files = snapshot_owned_files(pending, entry);
    for (const auto& [file, declared] : files.items())
      writes += file_bytes(snapshot_directory_ / file, declared.get<uint64_t>());
  }
  return {{"saved_bytes", saved}, {"pending_cleanup_bytes", cleanup}, {"reserved_write_bytes", writes},
          {"used_bytes", saved + cleanup + writes},
          {"budget_bytes", budget}, {"usage_known", known}};
}
uint64_t JuicedService::cache_disk_bytes_locked() const { return cache_disk_usage_locked().at("used_bytes"); }

void JuicedService::invalidate_snapshot_write_bound_locked(const std::string& pending, const json& entry) {
  const auto profile = entry.value("profile", "");
  if (profile.empty()) return;
  std::error_code ec;
  const auto bytes = std::filesystem::file_size(snapshot_directory_ / pending, ec);
  if (!ec && bytes > entry.value("bytes", uint64_t(0)) && state_["qualified_profiles"].contains(profile))
    state_["qualified_profiles"][profile]["write_bound_invalid"] = true;
}
void JuicedService::stage_snapshot_publication_locked(const std::string& pending, const json& candidate) {
  const auto key = candidate.at("id").get<std::string>();
  auto& intent = state_["snapshot_writes"].at(pending);
  intent["state"] = "publishing";
  intent["destination"] = key + ".bin";
  intent["backup"] = key + ".previous.bin";
  intent["candidate"] = candidate;
  intent["previous"] = state_["snapshots"].value(key, json::object());
  state_["snapshots"].erase(key);
  persist_locked();
}
void JuicedService::publish_snapshot_write(const std::string& pending, const json& intent) {
  MutationClaim claim(mutex_, active_snapshot_publications_);
  if (!claim.acquire(pending)) return;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!state_["snapshot_writes"].contains(pending) || state_["snapshot_writes"][pending] != intent) return;
  }
  if (snapshot_verification_barrier_) snapshot_verification_barrier_();
  const auto source = snapshot_directory_ / pending;
  const auto destination = snapshot_directory_ / intent.at("destination").get<std::string>();
  const auto backup = snapshot_directory_ / intent.at("backup").get<std::string>();
  const auto& candidate = intent.at("candidate");
  const auto& previous = intent.at("previous");
  const auto matches = [&](const std::filesystem::path& path, const json& metadata) {
    if (metadata.empty() || !metadata.contains("checksum")) return false;
    std::error_code ec;
    const auto bytes = std::filesystem::file_size(path, ec);
    if (ec == std::errc::no_such_file_or_directory) return false;
    if (ec) throw std::runtime_error("Cannot verify snapshot publication file: " + ec.message());
    return bytes == metadata.at("bytes").get<uint64_t>() && file_digest(path) == metadata.at("checksum").get<std::string>();
  };
  const bool source_ready = matches(source, candidate);
  const bool destination_ready = !source_ready && matches(destination, candidate);
  const bool previous_backup = !source_ready && !destination_ready && matches(backup, previous);
  const bool previous_destination = !source_ready && !destination_ready && !previous_backup && matches(destination, previous);
  std::lock_guard<std::mutex> lock(mutex_);
  if (!state_["snapshot_writes"].contains(pending) || state_["snapshot_writes"][pending] != intent) return;
  const auto key = candidate.at("id").get<std::string>();
  if (!source_ready && !destination_ready) {
    // Sealed intent follows completed provider I/O; only this owner can still publish its files.
    auto cleanup = intent;
    cleanup["state"] = "cleanup";
    cleanup["files"] = snapshot_owned_files(pending, intent);
    if (previous_backup || previous_destination) {
      if (previous_backup) {
        std::error_code ec;
        if (!utils::atomic_replace_file(backup, destination, ec))
          throw std::runtime_error("Cannot restore previous snapshot: " + ec.message());
      }
      state_["snapshots"][key] = previous;
      cleanup["files"].erase(intent.at("destination").get<std::string>());
    }
    state_["snapshot_writes"][pending] = std::move(cleanup);
    persist_locked();
    cleanup_snapshot_writes_locked();
    return;
  }
  if (source_ready) {
    std::error_code ec;
    if (!previous.empty() && !std::filesystem::exists(backup) && std::filesystem::exists(destination)) {
      if (!utils::atomic_replace_file(destination, backup, ec))
        throw std::runtime_error("Cannot retain previous snapshot: " + ec.message());
    }
    if (!utils::atomic_replace_file(source, destination, ec))
      throw std::runtime_error("Cannot atomically replace snapshot: " + ec.message());
  }
  if (snapshot_publication_barrier_) snapshot_publication_barrier_();
  state_["snapshots"][key] = candidate;
  if (!previous.empty()) state_["snapshot_cleanup"][key + ".previous"] = previous;
  if (source_ready || !std::filesystem::exists(source)) state_["snapshot_writes"].erase(pending);
  else {
    auto cleanup = intent;
    cleanup["state"] = "cleanup";
    cleanup["files"] = {{pending, intent.at("bytes")}};
    state_["snapshot_writes"][pending] = std::move(cleanup);
  }
  persist_locked();
  cleanup_snapshot_writes_locked();
  cleanup_snapshots_locked();
}
void JuicedService::cleanup_snapshot_writes_locked() {
  const auto writes = state_["snapshot_writes"];
  bool changed = false;
  for (const auto& [file, entry] : writes.items()) {
    if (entry.value("state", "") != "cleanup") continue;
    invalidate_snapshot_write_bound_locked(file, entry);
    // Persist disproven bounds before deleting the evidence, even after a failed response.
    if (state_ != committed_state_) persist_locked();
    auto files = snapshot_owned_files(file, entry);
    const auto owned = files;
    for (const auto& [path, bytes] : owned.items()) {
      std::error_code ec;
      std::filesystem::remove(snapshot_directory_ / path, ec);
      if (!ec) { files.erase(path); changed = true; }
    }
    if (files.empty()) state_["snapshot_writes"].erase(file);
    else state_["snapshot_writes"][file]["files"] = std::move(files);
  }
  if (changed) {
    try { persist_locked(); }
    catch (const std::exception& error) {
      LOG(WARNING, "Juiced") << "Snapshot write cleanup remains charged: " << error.what() << std::endl;
    }
  }
}
void JuicedService::reconcile_snapshot_writes() {
  json writes;
  { std::lock_guard<std::mutex> lock(mutex_); writes = state_["snapshot_writes"]; }
  for (const auto& [file, entry] : writes.items()) {
    if (entry.value("state", "") == "publishing") {
      try { publish_snapshot_write(file, entry); }
      catch (const std::exception& error) {
        LOG(WARNING, "Juiced") << "Snapshot publication remains charged: " << error.what() << std::endl;
      }
      continue;
    }
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (state_["snapshot_writes"].contains(file) && state_["snapshot_writes"][file] == entry) {
        invalidate_snapshot_write_bound_locked(file, entry);
        if (state_ != committed_state_) persist_locked();
      }
    }
    if (entry.value("owner", "") != cache_run_id_ ||
        !server_.router_->generation_was_stopped(entry.value("generation", uint64_t(0)))) continue;
    std::lock_guard<std::mutex> lock(mutex_);
    if (state_["snapshot_writes"].contains(file) && state_["snapshot_writes"][file] == entry) {
      state_["snapshot_writes"][file]["state"] = "cleanup";
      persist_locked();
    }
  }
}
bool JuicedService::reserve_snapshot_write_locked(const std::string& pending, const std::string& model,
                                                  uint64_t generation, uint64_t bound, const std::string& profile) {
  for (const auto& [file, entry] : state_["snapshot_writes"].items())
    invalidate_snapshot_write_bound_locked(file, entry);
  if (state_ != committed_state_) persist_locked();
  if (!profile.empty() && state_["qualified_profiles"].value(profile, json::object()).value("write_bound_invalid", false))
    return false;
  prune_cache_locked();
  const auto budget = cache_disk_usage_locked().at("budget_bytes").get<uint64_t>();
  const auto key = pending.size() >= 8 ? pending.substr(0, pending.size() - 8) : pending;
  if (!bound || bound > budget || state_["snapshot_writes"].contains(pending) || state_["snapshot_cleanup"].contains(key) ||
      state_["snapshot_cleanup"].contains(key + ".previous") || std::filesystem::exists(snapshot_directory_ / (key + ".previous.bin"))) return false;
  auto fits = [&] { const auto used = cache_disk_bytes_locked(); return used <= budget && bound <= budget - used; };
  if (!fits()) {
    std::vector<std::pair<std::string, json>> order;
    for (const auto& [id, entry] : state_["snapshots"].items())
      if (!entry.value("retained", false) && !active_snapshots_.count(id)) order.emplace_back(id, entry);
    std::sort(order.begin(), order.end(), [](const auto& a, const auto& b) { return a.second.value("last_used", 0LL) < b.second.value("last_used", 0LL); });
    for (const auto& [id, entry] : order) {
      forget_snapshots_locked({id});
      if (fits()) break;
    }
  }
  if (!fits()) return false;
  uint64_t reserved = bound;
  for (const auto& [file, entry] : state_["snapshot_writes"].items()) reserved += entry.value("bytes", uint64_t(0));
  std::error_code space_error;
  const auto space = std::filesystem::space(snapshot_directory_, space_error);
  if (space_error || space.available < reserved + 1024ULL * 1024 * 1024) return false;
  state_["snapshot_writes"][pending] = {{"bytes", bound}, {"model", model}, {"generation", generation},
                                        {"owner", cache_run_id_}, {"state", "writing"}, {"profile", profile}};
  persist_locked();
  return true;
}
void JuicedService::cleanup_snapshots_locked() {
  bool changed = false;
  const auto pending = state_["snapshot_cleanup"];
  for (const auto& [id, entry] : pending.items()) {
    if (active_snapshots_.count(id) || state_["snapshots"].contains(id)) continue;
    std::error_code ec;
    std::filesystem::remove(snapshot_directory_ / (id + ".bin"), ec);
    if (ec) continue;
    state_["snapshot_cleanup"].erase(id);
    changed = true;
  }
  if (changed) {
    try { persist_locked(); }
    catch (const std::exception& error) {
      LOG(WARNING, "Juiced") << "Snapshot cleanup will retry its durable tombstone: " << error.what() << std::endl;
    }
  }
}
void JuicedService::forget_snapshots_locked(const std::vector<std::string>& ids) {
  bool changed = false;
  for (const auto& id : ids) {
    if (active_snapshots_.count(id)) throw std::runtime_error("Snapshot is in use");
    if (state_["snapshots"].contains(id)) {
      state_["snapshot_cleanup"][id] = state_["snapshots"][id];
      state_["snapshots"].erase(id);
      changed = true;
    }
  }
  // A failed commit restores metadata before any saved computation is removed.
  if (changed) persist_locked();
  cleanup_snapshots_locked();
}
void JuicedService::prune_cache_locked() {
  cleanup_snapshot_writes_locked();
  cleanup_snapshots_locked();
  auto budget = static_cast<uint64_t>(
      state_["cache"].value("disk_budget_gib", 8.0) * 1024 * 1024 * 1024);
  auto ttl = state_["cache"].value("retention_days", 7) * 86400LL;
  std::vector<std::pair<std::string, json>> entries;
  for (const auto &[id, j] : state_["snapshots"].items())
    entries.emplace_back(id, j);
  std::sort(entries.begin(), entries.end(), [](const auto &a, const auto &b) {
    return a.second.value("last_used", 0LL) < b.second.value("last_used", 0LL);
  });
  std::vector<std::string> remove;
  for (const auto &[id, j] : entries) {
    if (active_snapshots_.count(id))
      continue;
    auto file = snapshot_directory_ / (id + ".bin");
    if (!std::filesystem::exists(file) ||
        (!j.value("retained", false) &&
         timestamp() - j.value("last_used", 0LL) > ttl)) {
      remove.push_back(id);
    }
  }
  if (!remove.empty()) forget_snapshots_locked(remove);
  for (const auto& [id, entry] : entries) {
    if (cache_disk_bytes_locked() <= budget) break;
    if (active_snapshots_.count(id) || entry.value("retained", false) || !state_["snapshots"].contains(id)) continue;
    forget_snapshots_locked({id});
  }
}
void JuicedService::cache_api(const httplib::Request &req,
                              httplib::Response &res, const std::string &op) {
  auto body = req.body.empty() ? json::object() : json::parse(req.body);
  if (body.contains("model_modes")) {
    if (!body["model_modes"].is_object())
      throw std::invalid_argument("Model cache modes must be an object");
    for (const auto &[model, mode] : body["model_modes"].items()) {
      if (!mode.is_string() || (mode != "memory" && mode != "persistent"))
        throw std::invalid_argument("Model cache mode must be memory or persistent");
      const auto canonical = server_.model_manager_->get_public_model_name(
          server_.model_manager_->resolve_model_name(model));
      if (!server_.model_manager_->model_exists(model) || canonical != model)
        throw std::invalid_argument("Use an existing canonical model name");
    }
  }
  if (op == "cache/recover") {
    if (!body.is_object() || !body.empty()) throw std::invalid_argument("Recovery takes no file paths or overrides");
    reconcile_snapshot_writes();
    std::lock_guard<std::mutex> lock(mutex_);
    prune_cache_locked();
    output(res, {{"pending_writes", state_["snapshot_writes"]}, {"disk", cache_disk_usage_locked()}});
    return;
  }
  if (op == "cache/profile" || op == "cache/qualify") {
    auto ownership = server_.router_->acquire_model_state_operation(body.at("model").get<std::string>());
    if (!ownership) throw std::runtime_error("The prompt-state worker is not ready");
    auto profile = cache_profile(body.at("model").get<std::string>(), ownership);
    if (op == "cache/qualify") {
      if (!profile.value("configured", false))
        throw std::invalid_argument(profile.at("configuration_reason").get<std::string>());
      if (body.value("fingerprint", "") != profile.value("fingerprint", ""))
        throw std::invalid_argument(
            "The tested profile changed; repeat acceptance");
      if (body.value("evidence", "").empty())
        throw std::invalid_argument(
            "Record the exact-profile save/restart/restore evidence before "
            "qualification");
      if (body.contains("max_snapshot_bytes") &&
          (!body["max_snapshot_bytes"].is_number_unsigned() && !body["max_snapshot_bytes"].is_number_integer()))
        throw std::invalid_argument("Snapshot write bound must be a positive integer");
      if (body.contains("max_snapshot_bytes") &&
          (body["max_snapshot_bytes"] <= 0 || body["max_snapshot_bytes"] > 256ULL * 1024 * 1024 * 1024))
        throw std::invalid_argument("Snapshot write bound must be positive and at most 256 GiB");
      std::lock_guard<std::mutex> lock(mutex_);
      state_["qualified_profiles"]
            [profile.at("fingerprint").get<std::string>()] = {
                {"model", profile["model"]},
                {"evidence", body["evidence"]},
                {"qualified_at", timestamp()},
                {"max_snapshot_bytes", body.value("max_snapshot_bytes", uint64_t(0))}};
      persist_locked();
      profile["qualified"] = true;
    }
    output(res, profile);
    return;
  }
  if (op == "cache/retain" || op == "cache/forget") {
    const auto snapshot = body.value("snapshot", "");
    const auto session = body.value("session", body.value("thread", ""));
    if (snapshot.empty() && session.empty())
      throw std::invalid_argument("Specify a snapshot or consumer session");
    if (op == "cache/retain" &&
        (!body.contains("retained") || !body["retained"].is_boolean()))
      throw std::invalid_argument("retained must be a boolean");
    std::lock_guard<std::mutex> lock(mutex_);
    for (const auto& [file, intent] : state_["snapshot_writes"].items()) {
      if (intent.value("state", "") != "publishing") continue;
      const auto& candidate = intent.at("candidate");
      if ((!snapshot.empty() && candidate.at("id") == snapshot) ||
          (snapshot.empty() && candidate.value("session", "") == session)) {
        output(res, {{"error", "Snapshot publication requires recovery before mutation"}}, 409);
        return;
      }
    }
    std::vector<std::string> ids;
    for (const auto &[id, j] : state_["snapshots"].items()) {
      if ((!snapshot.empty() && id == snapshot) ||
          (snapshot.empty() &&
           j.value("session", j.value("thread", "")) == session)) {
        if (active_snapshots_.count(id)) {
          output(res,
                 {{"error",
                   "Session cache is in use; retry after generation finishes"}},
                 409);
          return;
        }
        ids.push_back(id);
      }
    }
    if (op == "cache/forget") {
      for (const auto& [id, entry] : state_["snapshot_cleanup"].items())
        if ((!snapshot.empty() && id == snapshot) ||
            (snapshot.empty() && entry.value("session", entry.value("thread", "")) == session))
          ids.push_back(id);
    }
    if (ids.empty()) {
      output(res, {{"error", "Snapshot not found"}}, 404);
      return;
    }
    if (op == "cache/retain") {
      for (const auto& id : ids) state_["snapshots"][id]["retained"] = body.at("retained");
      persist_locked();
    } else {
      forget_snapshots_locked(ids);
    }
    output(res, {{op == "cache/retain" ? "updated" : "forgotten", ids.size()},
                 {"cleanup_pending", state_["snapshot_cleanup"].size()},
                 {"memory_note",
                  "Snapshot operations affect saved computation. Live engine "
                  "prefixes may remain until replaced or unloaded."}});
    return;
  }
  const auto loaded = req.method != "GET"
                          ? server_.router_->get_all_loaded_models()
                          : json::array();
  std::lock_guard<std::mutex> lock(mutex_);
  if (req.method != "GET") {
    auto mode = body.value("mode", state_["cache"].value("mode", "memory"));
    if (mode != "memory" && mode != "off" && mode != "persistent")
      throw std::invalid_argument("Invalid cache mode");
    if (mode != state_["cache"].value("mode", "memory")) {
      for (const auto &[model, count] : active_cache_operations_)
        if (count > 0) {
          output(
              res,
              {{"error",
                "Finish or stop active requests before changing cache mode"}},
              409);
          return;
        }
    }
    if (body.contains("model_modes")) {
      for (const auto &[model, count] : active_cache_operations_) {
        const auto next = body["model_modes"].value(model, mode);
        if (count > 0 && next != cache_mode_locked(model)) {
          output(res,
                 {{"error", "Finish or stop active requests before changing model cache mode"}},
                 409);
          return;
        }
      }
    }
    const auto overrides = body.value(
        "model_modes", state_["cache"].value("model_modes", json::object()));
    for (const auto &model : loaded) {
      const auto name = model.value("model_name", "");
      if (model.value("is_busy", false) &&
          overrides.value(name, mode) != cache_mode_locked(name)) {
        output(res, {{"error", "Finish or stop active requests before changing model cache mode"}}, 409);
        return;
      }
    }
    if (mode == "persistent" && state_["qualified_profiles"].empty()) {
      output(
          res,
          {{"error",
            "No deployment has passed exact-profile useful-reuse acceptance"}},
          422);
      return;
    }
    if (body.contains("retention_days") &&
        (!body["retention_days"].is_number_integer() ||
         body["retention_days"] < 1 || body["retention_days"] > 365))
      throw std::invalid_argument("Retention must be between 1 and 365 days");
    if (body.contains("disk_budget_gib") &&
        (!body["disk_budget_gib"].is_number() || body["disk_budget_gib"] < 1 ||
         body["disk_budget_gib"] > 256))
      throw std::invalid_argument("Disk budget must be between 1 and 256 GiB");
    for (const std::string k : {"mode", "retention_days", "disk_budget_gib",
                                "model_modes"})
      if (body.contains(k))
        state_["cache"][k] = body[k];
    persist_locked();
    wake_.notify_all();
  }
  json entries = json::array();
  for (const auto &[id, j] : state_["snapshots"].items()) {
    auto entry = j;
    entry["session"] = j.value("session", j.value("thread", ""));
    entry.erase("thread");
    entries.push_back(entry);
  }
  output(res, {{"settings", state_["cache"]},
               {"persistent_supported", !state_["qualified_profiles"].empty()},
               {"reason",
                state_["qualified_profiles"].empty()
                    ? "No exact model/engine profile has passed "
                      "save/restart/restore useful-reuse acceptance"
                    : "Persistent mode applies only to qualified unchanged "
                      "profiles; other requests use memory caching"},
               {"disk", cache_disk_usage_locked()},
               {"pending_writes", state_["snapshot_writes"]},
               {"snapshots", entries},
               {"qualified_profiles", state_["qualified_profiles"]}});
}
void JuicedService::chat(const httplib::Request &original,
                         httplib::Response &res) {
  std::optional<RequestContextScope> native_context;
  if (!current_request_context()) native_context.emplace(make_request_context(
      original, server_.api_key_, server_.admin_api_key_, server_.authentication_epoch_));
  json request;
  httplib::Request normalized = original;
  std::string model_name;
  try {
    request = json::parse(original.body);
    server_.normalize_and_resolve_request_model(request);
    model_name = server_.model_manager_->get_public_model_name(
        server_.model_manager_->resolve_model_name(request.value("model", "")));
    request["model"] = model_name;
    if ((request.contains("cache_prompt") && !request["cache_prompt"].is_boolean()) ||
        (request.contains("messages") && !request["messages"].is_array())) {
      output(res, {{"error", {{"message", "cache_prompt must be boolean and messages must be an array"},
                              {"type", "invalid_request_error"}}}}, 400);
      return;
    }
    normalized.body = request.dump();
  } catch (const std::exception &) {
    server_.handle_chat_completions(original, res);
    return;
  }
  if ((model_name.empty() || (server_.model_manager_->model_exists(model_name) &&
      is_router_collection_recipe(server_.model_manager_->get_model_info(model_name).recipe))) &&
      unleased_protocol(normalized, res)) return;
  bool persistent = false;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    persistent = cache_mode_locked(model_name) == "persistent";
  }
  if (!persistent) {
    server_.handle_chat_completions(normalized, res);
    return;
  }
  bool inference_started = false;
  try {
    const auto requested_model = model_name;
    if (server_.model_manager_->get_model_info(requested_model).recipe != "llamacpp") {
      server_.handle_chat_completions(normalized, res);
      return;
    }
    server_.auto_load_model_if_needed(requested_model);
    auto ownership = server_.router_->acquire_model_state_operation(requested_model, true);
    if (!ownership) throw std::runtime_error("The prompt-state worker is not ready");
    auto worker = std::make_shared<WrappedServer::InferenceLease>(std::move(ownership));
    const auto request_context = current_request_context();
    const auto disconnected = request_context ? request_context->cancelled : std::function<bool()>{};
    const auto deadline = worker->completion_deadline();
    const auto cancelled = [deadline, disconnected] {
      return std::chrono::steady_clock::now() >= deadline || (disconnected && disconnected());
    };
    auto guard = std::make_shared<PromptStateClaim>(
        mutex_, active_cache_operations_, active_snapshots_, requested_model, "");
    const auto prior_cancel = WrappedServer::current_request_cancel_context();
    WrappedServer::RequestCancelScope state_deadline({prior_cancel.flag, [prior_cancel, cancelled] {
      return prior_cancel.cancelled() || cancelled();
    }});
    {
      std::lock_guard<std::mutex> lock(mutex_);
      persistent = cache_mode_locked(requested_model) == "persistent";
      if (persistent) {
        ++active_cache_operations_[requested_model];
        guard->model_claimed = true;
      }
    }
    if (!persistent) {
      worker->release();
      server_.handle_chat_completions(normalized, res);
      return;
    }
    auto profile = cache_profile(requested_model, *worker);
    if (cancelled()) throw AdmissionError("state_operation_cancelled", "State inspection was cancelled", 499);
    const auto model = profile.at("model").get<std::string>();
    if (!profile.value("qualified", false)) {
      res.set_header("X-Juiced-Cache", "unqualified-profile");
      guard->release();
      worker->release();
      server_.handle_chat_completions(normalized, res);
      return;
    }
    const auto address = profile.at("address").get<std::string>();
    worker->resources_started();
    auto sr = utils::HttpClient::get(
        address + "/slots", {}, 10, utils::HttpSecurityPolicy::TrustedLoopback,
        {nullptr, [worker, disconnected] { return worker->deadline_expired() || (disconnected && disconnected()); }});
    worker->resources_completed();
    if (sr.status_code != 200)
      throw std::runtime_error("Cannot reserve a prompt-state slot");
    auto slots = json::parse(sr.body);
    if (!slots.is_array())
      throw std::runtime_error("Unsupported slot inventory");
    const auto context = current_request_context();
    const auto thread = context ? context->session : request_session_id(original);
    const bool has_thread = !thread.empty();
    const bool reuse_enabled = request.value("cache_prompt", true);
    const bool identified = has_thread && reuse_enabled;
    const auto principal = context ? context->identity
        : resolve_request_identity(original, server_.api_key_, server_.admin_api_key_, server_.authentication_epoch_);
    const auto identity = digest(json::array({"principal-v1", principal.principal,
        principal.credential_epoch, "chat.completions"}).dump());
    json semantics = json::object();
    for (const std::string key : {"tools", "tool_choice", "response_format",
                                  "chat_template_kwargs", "reasoning"})
      if (request.contains(key))
        semantics[key] = request[key];
    const auto key = digest(
        json::array({identity, thread, profile.at("fingerprint"), semantics})
            .dump());
    guard->key = key;
    auto release = [guard, worker] {
      if (guard->release()) worker->release();
    };
    const auto reserved_slot = worker->execution_slot();
    if (!reserved_slot) throw AdmissionError("execution_slots_unknown", "The cache operation has no physical slot lease");
    const int slot = *reserved_slot;
    const auto candidate = std::find_if(slots.begin(), slots.end(), [slot](const auto& item) {
      return item.is_object() && item.contains("id") && item.at("id") == slot;
    });
    if (candidate == slots.end() || candidate->value("is_processing", true))
      throw AdmissionError("execution_slot_unavailable", "The reserved backend slot is not idle; retry after reconciliation");
    json saved;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (has_thread && active_snapshots_.count(key)) {
        output(res,
               {{"error",
                 {{"message",
                   "This consumer session already has a request in progress"},
                  {"type", "session_busy"}}}},
               409);
        return;
      }
      prune_cache_locked();
      if (has_thread) {
        active_snapshots_.insert(key);
        guard->session_claimed = true;
      }
      const auto write = state_["snapshot_writes"].value(key + ".pending", json::object());
      const bool publishing = !write.empty() && (write.value("state", "") != "cleanup" ||
          snapshot_owned_files(key + ".pending", write).contains(key + ".bin"));
      if (identified && !state_["snapshot_cleanup"].contains(key) &&
          !publishing && state_["snapshots"].contains(key))
        saved = state_["snapshots"][key];
    }
    auto messages = request.value("messages", json::array());
    std::string restore = reuse_enabled ? "cold" : "off";
    if (!saved.is_null()) {
      auto count = saved.value("prefix_messages", size_t(0));
      auto file = snapshot_directory_ / (key + ".bin");
      if (count > 0 && count <= messages.size() &&
          saved.value("prefix_digest", "") == prefix_hash(messages, count) &&
          std::filesystem::exists(file)) {
        try {
          if (file_digest(file) != saved.value("checksum", ""))
            throw std::runtime_error("Snapshot checksum mismatch");
          auto restored = post_engine(
              address, "/slots/" + std::to_string(slot) + "?action=restore",
              {{"filename", key + ".bin"}}, *worker, disconnected);
          restore = "restored";
          res.set_header("X-Juiced-Restored-Tokens",
                         std::to_string(restored.value("n_restored", 0)));
        } catch (const AdmissionError&) {
          throw;
        } catch (const std::exception &) {
          restore = "rebuild-invalid-snapshot";
        }
      } else
        restore = "rebuild-changed-prefix";
    }
    res.set_header("X-Juiced-Cache", restore);
    request["id_slot"] = slot;
    // An identified cold session must not inherit the previous slot owner's
    // memory prefix, even when its prompt happens to match.
    request["cache_prompt"] = reuse_enabled && (!identified || restore == "restored");
    auto req = std::make_shared<httplib::Request>(original);
    req->body = request.dump();
    auto finish = [this, guard, worker, identified, key, thread, model, profile,
                   address, slot, messages, disconnected](bool success) {
      if (!success || !identified)
        return;
      const auto pending = key + ".pending";
      auto file = snapshot_directory_ / pending;
      bool reserved = false, dispatched = false, response_valid = false;
      uint64_t budget = 0, bound = 0;
      try {
        {
          std::lock_guard<std::mutex> lock(mutex_);
          if (cache_mode_locked(model) != "persistent") return;
          budget = uint64_t(state_["cache"].value("disk_budget_gib", 8.0) * 1024 * 1024 * 1024);
          const auto qualification = state_["qualified_profiles"].value(profile.at("fingerprint").get<std::string>(), json::object());
          bound = qualification.value("max_snapshot_bytes", uint64_t(0));
          if (!bound) bound = budget;
          if (qualification.value("write_bound_invalid", false))
            throw std::runtime_error("Snapshot profile exceeded its write bound; repeat qualification before saving");
          if (!reserve_snapshot_write_locked(pending, model, worker->generation(), bound, profile.at("fingerprint").get<std::string>()))
            throw std::runtime_error("Snapshot save skipped: disk budget or headroom is unavailable");
          reserved = true;
        }
        if (std::filesystem::space(snapshot_directory_).available <
            bound + 1024ULL * 1024 * 1024)
          throw std::runtime_error(
              "Insufficient free disk for a snapshot temporary file");
        if (snapshot_save_barrier_) snapshot_save_barrier_();
        auto result = post_engine(
            address, "/slots/" + std::to_string(slot) + "?action=save",
            {{"filename", pending}}, *worker, disconnected, &dispatched);
        response_valid = true;
        auto size = std::filesystem::file_size(file);
        if (size > bound)
          throw std::runtime_error(
              "Snapshot exceeds retained-state disk budget");
        const auto checksum = file_digest(file);
        json intent;
        {
          std::lock_guard<std::mutex> lock(mutex_);
          if (cache_disk_bytes_locked() > budget)
            throw std::runtime_error("Snapshot save exceeds disk budget including pending writes and cleanup");
          const bool retained = state_["snapshots"].contains(key) &&
                                state_["snapshots"][key].value("retained", false);
          const json candidate = {
              {"id", key},
              {"session", thread},
              {"model", model},
              {"profile", profile["fingerprint"]},
              {"bytes", size},
              {"tokens", result.value("n_saved", 0)},
              {"checksum", checksum},
              {"prefix_messages", messages.size()},
              {"prefix_digest", prefix_hash(messages, messages.size())},
              {"last_used", timestamp()},
              {"retained", retained},
              {"state", "saved"}};
          stage_snapshot_publication_locked(pending, candidate);
          intent = state_["snapshot_writes"].at(pending);
        }
        publish_snapshot_write(pending, intent);
      } catch (const std::exception &e) {
        std::lock_guard<std::mutex> lock(mutex_);
        if (reserved && state_["snapshot_writes"].contains(pending)) {
          auto& write = state_["snapshot_writes"][pending];
          if (write.value("state", "") != "publishing")
            write["state"] = !dispatched || response_valid ? "cleanup" : "uncertain";
          invalidate_snapshot_write_bound_locked(pending, write);
        }
        state_["cache"]["last_error"] = e.what();
        try {
          persist_locked();
          cleanup_snapshot_writes_locked();
        } catch (...) {
        }
      }
    };
    inference_started = true;
    server_.handle_chat_completions(*req, res);
    if (res.content_provider_) {
      struct StreamStatus {
        std::string pending, terminal;
        bool complete = false, failed = false, transport_failed = false,
             done = false;
        httplib::Headers trailers;
      };
      auto status = std::make_shared<StreamStatus>();
      auto provider = std::move(res.content_provider_);
      res.content_provider_ = [provider, status, finish, request_context, prior_cancel, cancelled,
                               release](size_t offset, size_t length,
                                        httplib::DataSink &sink) {
        std::optional<RequestContextScope> context;
        if (request_context) context.emplace(*request_context);
        WrappedServer::RequestCancelScope deadline_scope({prior_cancel.flag, [prior_cancel, cancelled] {
          return prior_cancel.cancelled() || cancelled();
        }});
        httplib::DataSink observed;
        observed.write = [&sink, status](const char *data, size_t size) {
          status->pending.append(data, size);
          for (;;) {
            auto cut = status->pending.find("\n\n");
            auto crlf = status->pending.find("\r\n\r\n");
            size_t delimiter = 2;
            if (crlf != std::string::npos &&
                (cut == std::string::npos || crlf < cut)) {
              cut = crlf;
              delimiter = 4;
            }
            if (cut == std::string::npos)
              break;
            auto frame = status->pending.substr(0, cut + delimiter);
            status->pending.erase(0, cut + delimiter);
            bool terminal = false;
            std::istringstream lines(frame);
            std::string line;
            while (std::getline(lines, line)) {
              if (line.rfind("data:", 0) != 0)
                continue;
              auto payload = line.substr(5);
              auto first = payload.find_first_not_of(" \t");
              if (first == std::string::npos)
                continue;
              payload.erase(0, first);
              if (!payload.empty() && payload.back() == '\r')
                payload.pop_back();
              if (payload == "[DONE]") {
                status->complete = true;
                terminal = true;
              } else {
                auto event = json::parse(payload, nullptr, false);
                if (event.is_object() && event.contains("error"))
                  status->failed = true;
              }
            }
            if (terminal)
              status->terminal += frame;
            else if (!sink.write(frame.data(), frame.size())) {
              status->transport_failed = true;
              return false;
            }
          }
          if (status->pending.size() > 128 * 1024) {
            status->failed = true;
            if (!sink.write(status->pending.data(), status->pending.size())) {
              status->transport_failed = true;
              return false;
            }
            status->pending.clear();
          }
          return true;
        };
        observed.is_writable = [&sink] {
          return !sink.is_writable || sink.is_writable();
        };
        observed.done = [status] { status->done = true; };
        observed.done_with_trailer = [status](const httplib::Headers &headers) {
          status->done = true;
          status->trailers = headers;
        };
        const bool ok = provider(offset, length, observed);
        // The turn settles only after its snapshot and lease do. Clients may
        // immediately send another turn when they receive the terminal SSE
        // event.
        finish(status->complete && !status->failed &&
               !status->transport_failed);
        release();
        if (!status->pending.empty() &&
            !sink.write(status->pending.data(), status->pending.size()))
          return false;
        if (!status->terminal.empty() &&
            !sink.write(status->terminal.data(), status->terminal.size()))
          return false;
        if (status->done) {
          if (!status->trailers.empty() && sink.done_with_trailer)
            sink.done_with_trailer(status->trailers);
          else if (sink.done)
            sink.done();
        }
        return ok && !status->transport_failed;
      };
      auto previous = std::move(res.content_provider_resource_releaser_);
      res.content_provider_resource_releaser_ = [previous, release, guard, worker,
                                                 req](bool ok) {
        if (previous)
          previous(ok);
        release();
      };
    } else
      finish(res.status == -1 || (res.status >= 200 && res.status < 300));
  } catch (const std::exception &e) {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      state_["cache"]["last_error"] = e.what();
      try {
        persist_locked();
      } catch (...) {
      }
    }
    if (const auto* admission = dynamic_cast<const AdmissionError*>(&e)) {
      if (admission->retry_after()) res.set_header("Retry-After", std::to_string(admission->retry_after()));
      output(res, admission->to_json(), admission->status);
    } else if (!inference_started) {
      // A miss or invalid snapshot already rebuilds inside its slot lease.
      // If admission itself fails, retry instead of racing a leased engine
      // slot.
      res.set_header("Retry-After", "1");
      output(
          res,
          {{"error",
            {{"message", e.what()}, {"type", "cache_admission_unavailable"}}}},
          503);
    } else {
      output(res,
             {{"error", {{"message", e.what()}, {"type", "inference_error"}}}},
             500);
    }
  }
}
} // namespace lemon
