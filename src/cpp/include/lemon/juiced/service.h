#pragma once
#include "lemon/server_process.h"
#include "lemon/resource_admission.h"
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
  struct MutationClaim {
    std::mutex& mutex;
    std::set<std::string>& active;
    std::string id;
    MutationClaim(std::mutex& owner, std::set<std::string>& claims) : mutex(owner), active(claims) {}
    MutationClaim(const MutationClaim&) = delete;
    MutationClaim& operator=(const MutationClaim&) = delete;
    bool acquire(const std::string& requested) {
      std::string candidate = requested;
      std::lock_guard<std::mutex> lock(mutex);
      if (!active.insert(candidate).second) return false;
      id.swap(candidate);
      return true;
    }
    ~MutationClaim() {
      if (id.empty()) return;
      std::lock_guard<std::mutex> lock(mutex);
      active.erase(id);
    }
  };
  Server &server_;
  std::filesystem::path directory_;
  mutable std::mutex mutex_;
  // Commits and their effects are ordered without holding mutex_ through Router.
  std::mutex policy_effect_mutex_;
  std::function<void()> policy_effect_barrier_, reconciliation_barrier_;
  std::condition_variable wake_;
  json committed_state_;
  json state_, catalog_ = json::array(), operations_ = json::object();
  struct QueuedCommand {
    json command;
    ResourceAdmission::WriteToken writer;
  };
  std::deque<QueuedCommand> pending_;
  std::mutex requests_mutex_;
  std::deque<json> requests_;
  unsigned long long request_sequence_ = 0;
  std::atomic<bool> stopping_{false};
  std::thread worker_, preparation_worker_;
  struct PreparationTask {
    json job;
    ResourceAdmission::Lease admission;
    ResourceAdmission::Lease operation;
    std::unique_ptr<ServerProcess> process;
    json outcome = json::object();
  };
  std::unique_ptr<PreparationTask> preparation_active_;
  std::set<std::string> preparation_cancelled_, prepared_cleanup_active_;
  std::unique_ptr<ServerProcess> voice_process_;
  ResourceAdmission::Lease voice_admission_;
  std::function<std::unique_ptr<ServerProcess>()> voice_process_factory_;
  bool voice_recovery_required_locked();
  bool finish_voice_cleanup() noexcept;
  std::string voice_signature_;
  long long voice_retry_after_ = 0;
  json voice_status_ = {{"enabled", false}, {"process_running", false}};
  std::map<std::string, size_t> active_cache_operations_;
  std::set<std::string> active_snapshots_;
  std::filesystem::path snapshot_directory_;
  std::string cache_run_id_;
  std::set<std::string> active_imports_, active_snapshot_publications_;
  std::function<void(const std::string&)> import_publication_barrier_;
  std::function<void()> removal_claim_barrier_;
  void publish_import(const std::string& id);
  void reconcile_imports();
  static std::string file_digest(const std::filesystem::path& file);
  std::string cache_mode_locked(const std::string &model) const;
  json cache_profile(const std::string &model, WrappedServer::InferenceLease& worker);
  void cache_api(const httplib::Request &, httplib::Response &,
                 const std::string &);
  json mutate_prompt_slot(const std::string& model, int slot,
                          const std::function<json()>& invoke);
  void prune_cache_locked();
  void forget_snapshots_locked(const std::vector<std::string>& ids);
  void cleanup_snapshots_locked();
  json cache_disk_usage_locked() const;
  uint64_t cache_disk_bytes_locked() const;
  bool reserve_snapshot_write_locked(const std::string& pending, const std::string& model,
                                     uint64_t generation, uint64_t bound, const std::string& profile = "", const json& actor = json::object());
  void stage_snapshot_publication_locked(const std::string& pending, const json& candidate);
  void publish_snapshot_write(const std::string& pending, const json& intent);
  void invalidate_snapshot_write_bound_locked(const std::string& pending, const json& entry);
  std::function<void()> snapshot_publication_barrier_, snapshot_save_barrier_, snapshot_verification_barrier_;
  void cleanup_snapshot_writes_locked();
  void reconcile_snapshot_writes();
  void bind_snapshot_storage();
  json save_state_locked(json candidate);
  void persist_locked();
  void set_policy_locked(const std::string& model, const std::string& policy);
  uint64_t policy_revision_locked(const std::string& model) const;
  void apply_policy_pin_locked(const std::string& model, uint64_t revision,
                               uint64_t generation = 0);
  void reconcile();
  void reconcile_voice();
  void compatibility_routes(httplib::Server &http);
  void preparation_api(const httplib::Request &, httplib::Response &);
  void preparation_run();
  bool finish_preparation_cleanup();
  json execution_plan(const std::string& model);
  json model_suitability(const std::string& model);
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
  bool execute_pending();
  void enqueue(const json &command);
  void run();
  void handle(const httplib::Request &, httplib::Response &,
              const std::string &operation);
  json removal_plan(const std::string &model);
  bool import_owns_model_locked(const std::string& name) const;
  void guard_model_removal_locked(const std::string& name) const;
  void with_model_removal(const std::string& model, const std::function<void()>& remove);
  void remove_model(const httplib::Request &, httplib::Response &);
  void import_file(const httplib::Request &, httplib::Response &,
                   const std::string &operation);
  void chat(const httplib::Request &, httplib::Response &);
};
} // namespace lemon
