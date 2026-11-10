#pragma once

// Shared controlled process, worker and native HTTP access for Router
// retirement tests.
#include "lemon/config_file.h"
#include "lemon/error_types.h"
#include "lemon/eviction_engine.h"
#include "lemon/global_vram_monitor.h"
#include "lemon/juiced/service.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "lemon/server_process.h"
#include "lemon/utils/path_utils.h"
#include "test_state_isolation.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <fstream>
#include <functional>
#include <future>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

namespace lemon {
class ControlledRetirementProcess : public ServerProcess {
public:
  ControlledRetirementProcess(std::function<void()> stop,
                              std::function<bool()> running,
                              std::function<bool()> exited)
      : ServerProcess({}), stop_(std::move(stop)), running_(std::move(running)),
        exited_(std::move(exited)) {}
  std::string start(const ServerCommand &) override { return "127.0.0.1"; }
  void stop() override { stop_(); }
  bool running() const override { return running_(); }
  bool exited() const override { return exited_(); }

private:
  std::function<void()> stop_;
  std::function<bool()> running_, exited_;
};
class RetirementWorker : public WrappedServer {
public:
  explicit RetirementWorker(const std::string &name)
      : WrappedServer("fixture", "error") {
    set_model_metadata(name, "retained-fixture", ModelType::LLM, DEVICE_CPU,
                       RecipeOptions("llamacpp", {{"ctx_size", 2048}}));
    ServerCommand command;
    command.port = 19342;
    start_owned_process(std::make_unique<ControlledRetirementProcess>(
                            [this] { controlled_stop(); },
                            [this] { return alive && !detached; },
                            [this] { return !alive.load(); }),
                        command);
    set_state(ModelState::READY);
  }
  ~RetirementWorker() override {
    release();
    fail = false;
    try {
      WrappedServer::unload();
    } catch (...) {
    }
  }
  void load(const std::string &, const ModelInfo &, const RecipeOptions &,
            bool) override {}
  bool is_backend_alive() const override {
    return WrappedServer::is_backend_alive();
  }
  bool observes_inference_completion() const override { return true; }
  json chat_completion(const json &) override {
    ++requests;
    note_request_started();
    note_request_completed();
    return {{"choices", json::array({{{"message",
                                       {{"role", "assistant"},
                                        {"content", "served fixture"}}}}})}};
  }
  void unload() override { WrappedServer::unload(); }
  void controlled_stop() {
    ++stops;
    detached = true;
    std::unique_lock lock(mutex);
    entered = true;
    changed.notify_all();
    changed.wait(lock, [&] { return released; });
    if (fail)
      throw std::runtime_error("controlled unresolved child");
    alive = false;
  }
  bool await() {
    std::unique_lock lock(mutex);
    return changed.wait_for(lock, std::chrono::seconds(2),
                            [&] { return entered; });
  }
  void release() {
    std::lock_guard lock(mutex);
    released = true;
    changed.notify_all();
  }
  std::atomic<bool> alive{true}, fail{false}, detached{false};
  std::atomic<int> stops{0}, requests{0};

private:
  std::mutex mutex;
  std::condition_variable changed;
  bool entered = false, released = false;
};
struct RoutingHelperTestHook {
  static std::unique_lock<std::mutex> registry_owner(Router &router) {
    return std::unique_lock<std::mutex>(router.load_mutex_);
  }
  static void policy_while_owned(Router &router,
                                 std::unique_lock<std::mutex> &owner,
                                 const std::string &model, uint64_t revision,
                                 bool pinned) {
    router.publish_residency_policy_locked(owner, model, revision, pinned, 0);
  }
  static bool standard_role(Router &router, const std::string &name) {
    std::lock_guard lock(router.load_mutex_);
    auto *worker = router.find_server_by_model_name(name);
    return worker && worker->get_residency_class() == ResidencyClass::Standard;
  }
  static bool loading(Router &router) {
    std::lock_guard lock(router.load_mutex_);
    return router.is_loading_;
  }
  static bool cleanup_slot_reserved(Router &router) {
    std::lock_guard lock(router.load_mutex_);
    return router.loaded_servers_.capacity() > router.loaded_servers_.size();
  }
  static uint64_t helper_revision(Router &router) {
    std::lock_guard lock(router.load_mutex_);
    return router.last_reconcile_generation_ + 1;
  }
  static bool helper_needed(Router &router, const std::string &model) {
    std::lock_guard lock(router.load_mutex_);
    return router.needed_helper_models_.count(model) != 0;
  }
  static bool pending(Router &router, const std::string &model,
                      uint64_t generation) {
    std::lock_guard lock(router.load_mutex_);
    auto *worker = router.find_server_by_model_name(model);
    return worker && worker->get_instance_id() == generation &&
           worker->cleanup_pending();
  }
  static void retry_cleanup(Router &router, const std::string &model,
                            uint64_t generation) {
    router.evict_if_committed(model, generation);
  }
  static void
  factory(Router &router,
          std::function<std::unique_ptr<WrappedServer>(const ModelInfo &)>
              factory) {
    router.backend_factory_override_ = std::move(factory);
  }
  static void npu(Router &router) {
    std::unique_lock lock(router.load_mutex_);
    router.evict_all_npu_servers(lock, ResidencyClass::Standard, [] {});
  }
  static void evict(Router &router,
                    const std::shared_ptr<RetirementWorker> &worker) {
    router.evict_if_committed(worker->get_model_name(),
                              worker->get_instance_id());
  }
  static void stop(Router &router) {
    router.eviction_engine_->stop();
    router.vram_monitor_->stop();
  }
  static void add(Router &router,
                  const std::shared_ptr<WrappedServer> &worker) {
    ResourceAdmission::Work work;
    work.model = worker->get_model_name();
    work.generation = worker->get_instance_id();
    work.operation = ResourceAdmission::Operation::Load;
    auto residency = router.admission_.acquire(work);
    residency.started();
    residency.retain_residency();
    worker->retain_resource_residency(std::move(residency));
    std::lock_guard lock(router.load_mutex_);
    router.loaded_servers_.push_back(worker);
  }
};
struct StatePublicationTestHook {
  static Router &router(Server &server) { return *server.router_; }
  static ModelManager &models(Server &server) { return *server.model_manager_; }
  static UpgradableFrontServer &front(Server &server) {
    return *server.http_front_;
  }
  static void stop_background(Server &server) { server.juiced_->stop(); }
  static void serving(Server &server) { server.running_ = true; }
  static void register_model(Server &server, const std::string &name) {
    const auto file =
        utils::path_from_utf8(server.config_dir_) / "retirement-fixture.gguf";
    if (!std::filesystem::exists(file)) {
      char header[24] = {'G', 'G', 'U', 'F', 3};
      std::ofstream output(file, std::ios::binary);
      output.write(header, sizeof(header));
    }
    server.model_manager_->register_user_model(
        name, {{"recipe", "llamacpp"},
               {"source", "local_path"},
               {"checkpoints", {{"main", file.u8string()}}},
               {"recipe_options",
                {{"ctx_size", 2048}, {"llamacpp_backend", "cpu"}}}});
    httplib::Request request;
    request.method = "POST";
    request.path = "/v1/juiced/policy";
    request.body = json{{"model", name}, {"policy", "on-demand"}}.dump();
    httplib::Response response;
    server.juiced_->handle(request, response, "policy");
    if (response.status != 200)
      throw std::runtime_error("Could not enable the HTTP fixture model");
  }
};
} // namespace lemon

namespace lemon {
class RetirementLoadWorker : public RetirementWorker {
public:
  RetirementLoadWorker(const std::string &name, SlotPolicy policy,
                       std::atomic<int> &starts, DeviceType device = DEVICE_NPU)
      : RetirementWorker(name), policy_(policy), starts_(starts),
        device_(device) {
    release();
  }
  DeviceType effective_device(const RecipeOptions &) const override {
    return device_;
  }
  SlotPolicy effective_slot_policy(const RecipeOptions &) const override {
    return policy_;
  }
  void load(const std::string &, const ModelInfo &, const RecipeOptions &,
            bool) override {
    ++starts_;
  }

private:
  SlotPolicy policy_;
  std::atomic<int> &starts_;
  DeviceType device_;
};
} // namespace lemon
