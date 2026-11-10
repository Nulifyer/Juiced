#pragma once

#include "router_retirement_fixture.h"

// Controlled startup, stop and final destruction, shared by fresh cleanup and
// allocation tests.
namespace lemon {
struct FreshGate {
  std::mutex mutex;
  std::condition_variable changed;
  bool entered = false, released = false;
  void enter() {
    std::lock_guard lock(mutex);
    entered = true;
    changed.notify_all();
  }
  bool await(std::chrono::milliseconds budget = std::chrono::seconds(2)) {
    std::unique_lock lock(mutex);
    return changed.wait_for(lock, budget, [&] { return entered; });
  }
  void wait() {
    std::unique_lock lock(mutex);
    changed.wait(lock, [&] { return released; });
  }
  void release() {
    std::lock_guard lock(mutex);
    released = true;
    changed.notify_all();
  }
  void rearm() {
    std::lock_guard lock(mutex);
    entered = false;
    released = false;
  }
};
struct FreshControl {
  FreshGate load, stopped, destroying, cancellation_seen;
  std::atomic<bool> alive{false}, fail_start{false}, fail_stop{false};
  std::atomic<int> starts{0}, stops{0}, destructors{0};
  std::atomic<uint64_t> generation{0};
  std::string error_code;
  int error_status = 0;
  bool nonstandard_error = false;
};
class FreshProcess : public ServerProcess {
public:
  explicit FreshProcess(std::shared_ptr<FreshControl> control)
      : ServerProcess({}), control_(std::move(control)) {}
  std::string start(const ServerCommand &) override {
    control_->alive = true;
    ++control_->starts;
    return "127.0.0.1";
  }
  void stop() override {
    ++control_->stops;
    control_->stopped.enter();
    control_->stopped.wait();
    if (control_->fail_stop)
      throw std::runtime_error("controlled fresh stop unconfirmed");
    control_->alive = false;
  }
  bool running() const override { return control_->alive; }
  bool exited() const override { return !control_->alive.load(); }

private:
  std::shared_ptr<FreshControl> control_;
};
class FreshWorker : public WrappedServer {
public:
  explicit FreshWorker(std::shared_ptr<FreshControl> control)
      : WrappedServer("fresh-fixture", "error"), control_(std::move(control)) {
    control_->generation = get_instance_id();
  }
  ~FreshWorker() override {
    ++control_->destructors;
    control_->destroying.enter();
    control_->destroying.wait();
    stop_for_destruction();
  }
  DeviceType effective_device(const RecipeOptions &) const override {
    return DEVICE_CPU;
  }
  SlotPolicy effective_slot_policy(const RecipeOptions &) const override {
    return SlotPolicy::Standard;
  }
  bool observes_inference_completion() const override { return true; }
  void load(const std::string &, const ModelInfo &, const RecipeOptions &,
            bool) override {
    ServerCommand command;
    command.program = "controlled-fresh";
    start_owned_process(std::make_unique<FreshProcess>(control_), command);
    control_->load.enter();
    {
      std::unique_lock lock(control_->load.mutex);
      while (!control_->load.released) {
        if (load_cancelled())
          control_->cancellation_seen.enter();
        control_->load.changed.wait_for(lock, std::chrono::milliseconds(1));
      }
    }
    if (control_->nonstandard_error)
      throw 42;
    if (!control_->error_code.empty())
      throw AdmissionError(control_->error_code,
                           "controlled typed load failure",
                           control_->error_status);
    if (control_->fail_start)
      throw std::runtime_error("controlled fresh startup failure");
  }

private:
  std::shared_ptr<FreshControl> control_;
};
} // namespace lemon
