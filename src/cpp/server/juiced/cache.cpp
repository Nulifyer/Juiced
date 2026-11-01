#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "lemon/utils/http_client.h"
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
std::string file_digest(const std::filesystem::path &file) {
  mbedtls_md_context_t ctx;
  mbedtls_md_init(&ctx);
  struct Cleanup {
    mbedtls_md_context_t *c;
    ~Cleanup() { mbedtls_md_free(c); }
  } cleanup{&ctx};
  if (mbedtls_md_setup(&ctx, mbedtls_md_info_from_type(MBEDTLS_MD_SHA256), 0) !=
          0 ||
      mbedtls_md_starts(&ctx) != 0)
    throw std::runtime_error("Cannot checksum prompt state");
  std::ifstream in(file, std::ios::binary);
  char b[65536];
  while (in) {
    in.read(b, sizeof(b));
    if (in.gcount() &&
        mbedtls_md_update(&ctx, reinterpret_cast<const unsigned char *>(b),
                          in.gcount()) != 0)
      throw std::runtime_error("Snapshot checksum failed");
  }
  if (!in.eof())
    throw std::runtime_error("Cannot read snapshot");
  unsigned char out[32];
  if (mbedtls_md_finish(&ctx, out) != 0)
    throw std::runtime_error("Snapshot checksum failed");
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
                 const json &body) {
  auto r = utils::HttpClient::post(address + path, body.dump(),
                                   {{"Content-Type", "application/json"}}, 120,
                                   utils::HttpSecurityPolicy::TrustedLoopback);
  if (r.status_code != 200)
    throw std::runtime_error("Engine state operation failed (" +
                             std::to_string(r.status_code) +
                             "): " + r.body.substr(0, 400));
  return json::parse(r.body);
}
std::string prefix_hash(const json &messages, size_t n) {
  json prefix = json::array();
  for (size_t i = 0; i < std::min(n, messages.size()); i++)
    prefix.push_back(messages[i]);
  return digest(prefix.dump());
}
} // namespace
bool JuicedService::unleased_protocol(const httplib::Request &req,
                                      httplib::Response &res) {
  try {
    auto body = json::parse(req.body);
    auto requested = body.value("model", "");
    if (requested.size() > 7 &&
        requested.substr(requested.size() - 7) == ":latest")
      requested.resize(requested.size() - 7);
    auto name = server_.model_manager_->get_public_model_name(
        server_.model_manager_->resolve_model_name(requested));
    std::lock_guard<std::mutex> lock(mutex_);
    bool qualified = false;
    for (const auto &[fingerprint, profile] :
         state_["qualified_profiles"].items())
      if (profile.value("model", "") == name)
        qualified = true;
    if (state_["cache"].value("mode", "memory") == "persistent" && qualified) {
      output(
          res,
          {{"error",
            {{"message", "This deployment uses persistent session slots. Use "
                         "Chat Completions, or select memory cache mode "
                         "before using Responses/Completions."},
             {"type", "invalid_request_error"}}}},
          409);
      return true;
    }
  } catch (const std::exception &) {
  }
  return false;
}
json JuicedService::cache_profile(const std::string &requested) {
  auto info = server_.model_manager_->get_model_info(requested);
  if (info.recipe != "llamacpp")
    throw std::invalid_argument(
        "Persistent prompt state requires a qualified llama.cpp deployment");
  const auto name = server_.model_manager_->get_public_model_name(
      server_.model_manager_->resolve_model_name(requested));
  json identity = {{"profile_schema", 1},   {"model", name},
                   {"recipe", info.recipe}, {"checkpoints", info.checkpoints},
                   {"engine", "unknown"},   {"files", json::object()}};
  for (const auto &[role, path] : info.resolved_paths) {
    auto canonical = std::filesystem::canonical(path);
    identity["files"][role] = {
        {"path", canonical.string()},
        {"size", std::filesystem::file_size(canonical)},
        {"mtime", std::filesystem::last_write_time(canonical)
                      .time_since_epoch()
                      .count()}};
  }
  std::string address;
  for (const auto &l : server_.router_->get_all_loaded_models())
    if (l.value("model_name", "") == name) {
      address = l.value("backend_url", "");
      identity["options"] = l.value("recipe_options", json::object());
      identity["options"].erase(
          "pinned"); // Residency policy does not alter engine state format.
      break;
    }
  if (address.empty())
    throw std::invalid_argument(
        "Load the model before qualifying its prompt-state profile");
  if (address.size() >= 3 && address.substr(address.size() - 3) == "/v1")
    address.resize(
        address.size() -
        3); // Backend inference base includes /v1; state APIs are at its root.
  auto props = utils::HttpClient::get(
      address + "/props", {}, 10, utils::HttpSecurityPolicy::TrustedLoopback);
  if (props.status_code != 200)
    throw std::runtime_error("Cannot inspect the engine state format");
  identity["engine"] = json::parse(props.body).value("build_info", "unknown");
  auto options = identity["options"].dump();
  bool configured =
      options.find(snapshot_directory_.string()) != std::string::npos &&
      identity["engine"].get<std::string>().find("dde879b") !=
          std::string::npos;
  auto fingerprint = digest(identity.dump());
  bool qualified;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    qualified =
        configured && state_["qualified_profiles"].contains(fingerprint);
  }
  return {{"model", name},          {"fingerprint", fingerprint},
          {"address", address},     {"configured", configured},
          {"qualified", qualified}, {"identity", identity}};
}
void JuicedService::prune_cache_locked() {
  auto budget = static_cast<uint64_t>(
      state_["cache"].value("disk_budget_gib", 8.0) * 1024 * 1024 * 1024);
  auto ttl = state_["cache"].value("retention_days", 7) * 86400LL;
  uint64_t total = 0;
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
      std::filesystem::remove(file);
      remove.push_back(id);
    } else
      total += std::filesystem::file_size(file);
  }
  for (const auto &[id, j] : entries) {
    if (total <= budget)
      break;
    if (active_snapshots_.count(id) || j.value("retained", false) ||
        std::find(remove.begin(), remove.end(), id) != remove.end())
      continue;
    auto file = snapshot_directory_ / (id + ".bin");
    if (std::filesystem::exists(file)) {
      total -= std::filesystem::file_size(file);
      std::filesystem::remove(file);
    }
    remove.push_back(id);
  }
  for (const auto &id : remove)
    state_["snapshots"].erase(id);
}
void JuicedService::cache_api(const httplib::Request &req,
                              httplib::Response &res, const std::string &op) {
  auto body = req.body.empty() ? json::object() : json::parse(req.body);
  if (op == "cache/profile" || op == "cache/qualify") {
    auto profile = cache_profile(body.at("model").get<std::string>());
    if (op == "cache/qualify") {
      if (!profile.value("configured", false))
        throw std::invalid_argument(
            "Load options must use the Juiced snapshot directory");
      if (body.value("fingerprint", "") != profile.value("fingerprint", ""))
        throw std::invalid_argument(
            "The tested profile changed; repeat acceptance");
      if (body.value("evidence", "").empty())
        throw std::invalid_argument(
            "Record the exact-profile save/restart/restore evidence before "
            "qualification");
      std::lock_guard<std::mutex> lock(mutex_);
      state_["qualified_profiles"]
            [profile.at("fingerprint").get<std::string>()] = {
                {"model", profile["model"]},
                {"evidence", body["evidence"]},
                {"qualified_at", timestamp()}};
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
    if (ids.empty()) {
      output(res, {{"error", "Snapshot not found"}}, 404);
      return;
    }
    for (const auto &id : ids) {
      if (op == "cache/retain")
        state_["snapshots"][id]["retained"] = body.at("retained");
      else {
        std::filesystem::remove(snapshot_directory_ / (id + ".bin"));
        state_["snapshots"].erase(id);
      }
    }
    persist_locked();
    output(res, {{op == "cache/retain" ? "updated" : "forgotten", ids.size()},
                 {"memory_note",
                  "Snapshot operations affect saved computation. Live engine "
                  "prefixes may remain until replaced or unloaded."}});
    return;
  }
  std::lock_guard<std::mutex> lock(mutex_);
  if (req.method != "GET") {
    auto mode = body.value("mode", state_["cache"].value("mode", "memory"));
    if (mode != "memory" && mode != "off" && mode != "persistent")
      throw std::invalid_argument("Invalid cache mode");
    if (mode != state_["cache"].value("mode", "memory")) {
      for (const auto &[model, slots] : active_slots_)
        if (!slots.empty()) {
          output(
              res,
              {{"error",
                "Finish or stop active requests before changing cache mode"}},
              409);
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
    for (const std::string k : {"mode", "retention_days", "disk_budget_gib"})
      if (body.contains(k))
        state_["cache"][k] = body[k];
    prune_cache_locked();
    persist_locked();
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
               {"snapshots", entries},
               {"qualified_profiles", state_["qualified_profiles"]}});
}
void JuicedService::chat(const httplib::Request &original,
                         httplib::Response &res) {
  bool persistent = false;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    persistent = state_["cache"].value("mode", "memory") == "persistent";
  }
  if (!persistent) {
    server_.handle_chat_completions(original, res);
    return;
  }
  bool inference_started = false;
  try {
    auto request = json::parse(original.body);
    const auto requested_model = request.at("model").get<std::string>();
    if (server_.model_manager_->get_model_info(requested_model).recipe !=
        "llamacpp") {
      server_.handle_chat_completions(original, res);
      return;
    }
    server_.auto_load_model_if_needed(requested_model);
    auto profile = cache_profile(requested_model);
    const auto model = profile.at("model").get<std::string>();
    if (!profile.value("qualified", false)) {
      res.set_header("X-Juiced-Cache", "unqualified-profile");
      server_.handle_chat_completions(original, res);
      return;
    }
    const auto address = profile.at("address").get<std::string>();
    auto sr = utils::HttpClient::get(
        address + "/slots", {}, 10, utils::HttpSecurityPolicy::TrustedLoopback);
    if (sr.status_code != 200)
      throw std::runtime_error("Cannot reserve a prompt-state slot");
    auto slots = json::parse(sr.body);
    if (!slots.is_array())
      throw std::runtime_error("Unsupported slot inventory");
    int slot = -1;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      for (const auto &s : slots) {
        int id = s.at("id").get<int>();
        if (!active_slots_[model].count(id) &&
            !s.value("is_processing", false)) {
          slot = id;
          active_slots_[model].insert(id);
          break;
        }
      }
    }
    if (slot < 0) {
      res.set_header("Retry-After", "1");
      output(res,
             {{"error",
               {{"message", "All prompt-state slots are busy; retry shortly"},
                {"type", "capacity_exceeded"}}}},
             429);
      return;
    }
    const auto requested_session =
        original.get_header_value("X-Juiced-Session-Id");
    const auto thread = requested_session.empty()
                            ? original.get_header_value("X-Juiced-Thread-Id")
                            : requested_session;
    const bool has_thread =
        !thread.empty() && thread.size() < 160 &&
        thread.find_first_not_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRS"
                                 "TUVWXYZ0123456789._-") == std::string::npos;
    const bool reuse_enabled = request.value("cache_prompt", true);
    const bool identified = has_thread && reuse_enabled;
    const auto identity = digest(original.get_header_value("Authorization"));
    json semantics = json::object();
    for (const std::string key : {"tools", "tool_choice", "response_format",
                                  "chat_template_kwargs", "reasoning"})
      if (request.contains(key))
        semantics[key] = request[key];
    const auto key = digest(
        json::array({identity, thread, profile.at("fingerprint"), semantics})
            .dump());
    json saved;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      if (has_thread && active_snapshots_.count(key)) {
        active_slots_[model].erase(slot);
        output(res,
               {{"error",
                 {{"message",
                   "This consumer session already has a request in progress"},
                  {"type", "session_busy"}}}},
               409);
        return;
      }
      prune_cache_locked();
      if (has_thread)
        active_snapshots_.insert(key);
      if (identified && state_["snapshots"].contains(key))
        saved = state_["snapshots"][key];
    }
    auto released = std::make_shared<std::atomic<bool>>(false);
    auto release = [this, model, slot, key, has_thread, released] {
      if (released->exchange(true))
        return;
      std::lock_guard<std::mutex> lock(mutex_);
      active_slots_[model].erase(slot);
      if (has_thread)
        active_snapshots_.erase(key);
    };
    // Shared RAII guard survives the streaming content-provider lifetime.
    auto guard = std::shared_ptr<int>(new int(slot), [release](int *p) {
      release();
      delete p;
    });
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
              {{"filename", key + ".bin"}});
          restore = "restored";
          res.set_header("X-Juiced-Restored-Tokens",
                         std::to_string(restored.value("n_restored", 0)));
        } catch (const std::exception &) {
          restore = "rebuild-invalid-snapshot";
        }
      } else
        restore = "rebuild-changed-prefix";
    }
    res.set_header("X-Juiced-Cache", restore);
    request["id_slot"] = slot;
    request["cache_prompt"] = reuse_enabled;
    auto req = std::make_shared<httplib::Request>(original);
    req->body = request.dump();
    auto finish = [this, guard, identified, key, thread, model, profile,
                   address, slot, messages](bool success) {
      if (!success || !identified)
        return;
      const auto pending = key + ".pending";
      auto file = snapshot_directory_ / pending;
      try {
        uint64_t budget;
        {
          std::lock_guard<std::mutex> lock(mutex_);
          if (state_["cache"].value("mode", "") != "persistent")
            return;
          budget = uint64_t(state_["cache"].value("disk_budget_gib", 8.0) *
                            1024 * 1024 * 1024);
        }
        if (std::filesystem::space(snapshot_directory_).available <
            budget + 1024ULL * 1024 * 1024)
          throw std::runtime_error(
              "Insufficient free disk for a snapshot temporary file");
        auto result = post_engine(
            address, "/slots/" + std::to_string(slot) + "?action=save",
            {{"filename", pending}});
        auto size = std::filesystem::file_size(file);
        if (size > budget)
          throw std::runtime_error(
              "Snapshot exceeds retained-state disk budget");
        const auto checksum = file_digest(file);
        std::lock_guard<std::mutex> lock(mutex_);
        prune_cache_locked();
        uint64_t total = size;
        for (const auto &[id, j] : state_["snapshots"].items())
          if (id != key)
            total += j.value("bytes", uint64_t(0));
        if (total > budget) {
          auto entries = state_["snapshots"];
          std::vector<std::pair<std::string, json>> order;
          for (const auto &[id, j] : entries.items())
            if (id != key && !j.value("retained", false) &&
                !active_snapshots_.count(id))
              order.emplace_back(id, j);
          std::sort(order.begin(), order.end(),
                    [](const auto &a, const auto &b) {
                      return a.second.value("last_used", 0LL) <
                             b.second.value("last_used", 0LL);
                    });
          for (const auto &[id, j] : order) {
            if (total <= budget)
              break;
            std::filesystem::remove(snapshot_directory_ / (id + ".bin"));
            total -= j.value("bytes", uint64_t(0));
            state_["snapshots"].erase(id);
          }
        }
        if (total > budget)
          throw std::runtime_error("Retained snapshots fill the budget; change "
                                   "retention or forget a snapshot");
        std::filesystem::rename(file, snapshot_directory_ / (key + ".bin"));
        const bool retained = state_["snapshots"].contains(key) &&
                              state_["snapshots"][key].value("retained", false);
        state_["snapshots"][key] = {
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
        persist_locked();
      } catch (const std::exception &e) {
        std::error_code ec;
        std::filesystem::remove(file, ec);
        std::lock_guard<std::mutex> lock(mutex_);
        state_["cache"]["last_error"] = e.what();
        try {
          persist_locked();
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
      res.content_provider_ = [provider, status, finish,
                               release](size_t offset, size_t length,
                                        httplib::DataSink &sink) {
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
      res.content_provider_resource_releaser_ = [previous, release, guard,
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
    if (!inference_started) {
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
