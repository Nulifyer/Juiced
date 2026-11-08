#pragma once
#include "lemon/server_process.h"
#include "lemon/wrapped_server.h"
#include <atomic>
#include <condition_variable>
#include <deque>
#include <filesystem>
#include <httplib.h>
#include <map>
#include <memory>
#include <mutex>
#include <nlohmann/json.hpp>
#include <set>
#include <thread>
namespace lemon {
class Server;
struct ModelInfo;
class JuicedService {
public:
  explicit JuicedService(Server &server);
  ~JuicedService();
  void stop();
  void routes(httplib::Server &http);
  bool allows(const std::string &model) const;
  bool hot(const std::string &model) const;
  void set_hot(const std::string &model, bool pinned);
  bool inference_guard(const httplib::Request &, httplib::Response &);
  bool requires_prompt_state_lease(const std::string &model) const;
  bool unleased_protocol(const httplib::Request &, httplib::Response &);
  void request_started();
  void capture_response_usage(const httplib::Response &);
  void request_finished(const httplib::Request &, const httplib::Response &);

private:
  friend class Server;
  friend struct StatePublicationTestHook;
  using json = nlohmann::json;
  Server &server_;
  std::filesystem::path directory_;
  mutable std::mutex mutex_;
  // Commits and their effects are ordered without holding mutex_ through Router.
  std::mutex policy_effect_mutex_;
  std::function<void()> policy_effect_barrier_, reconciliation_barrier_;
  std::condition_variable wake_;
  json committed_state_;
  json state_, catalog_ = json::array(), operations_ = json::object();
  std::deque<json> pending_;
  std::mutex requests_mutex_;
  std::deque<json> requests_;
  unsigned long long request_sequence_ = 0;
  std::atomic<bool> stopping_{false};
  std::thread worker_, preparation_worker_;
  std::deque<json> preparation_pending_;
  std::set<std::string> preparation_cancelled_;
  std::unique_ptr<NativeProcess> voice_process_;
  std::string voice_signature_;
  long long voice_retry_after_ = 0;
  json voice_status_ = {{"enabled", false}, {"process_running", false}};
  std::map<std::string, std::set<int>> active_slots_;
  std::set<std::string> active_snapshots_;
  std::filesystem::path snapshot_directory_;
  std::string cache_mode_locked(const std::string &model) const;
  json cache_profile(const std::string &model, WrappedServer::InferenceLease& worker);
  void cache_api(const httplib::Request &, httplib::Response &,
                 const std::string &);
  json mutate_prompt_slot(const std::string& model, int slot,
                          const std::function<json()>& invoke);
  void prune_cache_locked();
  void forget_snapshots_locked(const std::vector<std::string>& ids);
  void cleanup_snapshots_locked();
  json save_state_locked(json candidate);
  void persist_locked();
  void set_policy_locked(const std::string& model, const std::string& policy);
  uint64_t policy_revision_locked(const std::string& model) const;
  void apply_policy_pin_locked(const std::string& model, uint64_t revision,
                               uint64_t generation = 0);
  void reconcile();
  void reconcile_voice();
  void compatibility_routes(httplib::Server &http);
  json prepared_artifacts();
  json preparation_plan(const std::string& model);
  void preparation_api(const httplib::Request &, httplib::Response &);
  void preparation_run();
  json execution_plan(const std::string& model);
  void execution_api(const httplib::Request &, httplib::Response &);
  void execution_devices(const httplib::Request &, httplib::Response &);
  void image_compatibility(const httplib::Request &, httplib::Response &,
                           const std::string &operation);
  json protocols();
  std::string service_version() const;
  json model_capabilities(const ModelInfo &, const json &);
  void capabilities_api(const httplib::Request &, httplib::Response &);
  std::string image_model(const std::string &requested);
  void refresh();
  void execute(const json &command);
  void enqueue(const json &command);
  void run();
  void handle(const httplib::Request &, httplib::Response &,
              const std::string &operation);
  json removal_plan(const std::string &model);
  void remove_model(const httplib::Request &, httplib::Response &);
  void import_file(const httplib::Request &, httplib::Response &,
                   const std::string &operation);
  void chat(const httplib::Request &, httplib::Response &);
};
} // namespace lemon
