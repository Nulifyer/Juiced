#include <atomic>
#include <cstdlib>
#include <new>
namespace publication_allocation {
thread_local bool eligible = false;
std::atomic<bool> armed{false};
std::atomic<int> faults{0};
} // namespace publication_allocation
void *operator new(std::size_t size) {
  if (publication_allocation::eligible &&
      publication_allocation::armed.exchange(false)) {
    ++publication_allocation::faults;
    throw std::bad_alloc();
  }
  if (void *result = std::malloc(size ? size : 1))
    return result;
  throw std::bad_alloc();
}
void *operator new[](std::size_t size) { return ::operator new(size); }
void operator delete(void *value) noexcept { std::free(value); }
void operator delete[](void *value) noexcept { std::free(value); }
void operator delete(void *value, std::size_t) noexcept { std::free(value); }
void operator delete[](void *value, std::size_t) noexcept { std::free(value); }
#include "support/router_fresh_fixture.h"
namespace lemon {
class PublicationWorker : public FreshWorker {
public:
  using FreshWorker::FreshWorker;
  void load(const std::string &name, const ModelInfo &info,
            const RecipeOptions &options, bool upgrade) override {
    publication_allocation::eligible = true;
    FreshWorker::load(name, info, options, upgrade);
  }
};
} // namespace lemon
int main() {
  using namespace lemon;
  using namespace std::chrono_literals;
  json rows = json::array();
  for (bool fail_stop : {false, true}) {
    auto settings = ConfigFile::base_defaults();
    settings["offline"] = true;
    settings["auto_evict"] = false;
    settings["disable_model_filtering"] = true;
    settings["auto_check_model_updates"] = false;
    settings["max_loaded_models"] = 2;
    RuntimeConfig config(settings);
    RuntimeConfig::set_global(&config);
    {
      auto control = std::make_shared<FreshControl>();
      control->destroying.release();
      control->fail_stop = fail_stop;
      ModelManager models;
      BackendManager backends;
      Router router(&config, &models, &backends);
      RoutingHelperTestHook::stop(router);
      auto ready = std::make_shared<RetirementWorker>("Allocation-Ready");
      ready->release();
      RoutingHelperTestHook::add(router, ready);
      RoutingHelperTestHook::factory(router, [&](const ModelInfo &) {
        return std::make_unique<PublicationWorker>(control);
      });
      ModelInfo info;
      info.model_name = "Publication-Fresh";
      info.recipe = "llamacpp";
      info.checkpoints["main"] = "fixture";
      info.recipe_options = RecipeOptions("llamacpp", {{"ctx_size", 2048}});
      publication_allocation::armed = false;
      const int prior_faults = publication_allocation::faults.load();
      auto loading = std::async(std::launch::async, [&] {
        try {
          router.load_model(info.model_name, info, info.recipe_options, false,
                            false);
        } catch (const std::bad_alloc &) {
          return std::string("bad_alloc");
        } catch (const std::exception &error) {
          return std::string(error.what());
        }
        return std::string("success");
      });
      const bool started = control->load.await();
      bool sealed = false;
      {
        auto owner = RoutingHelperTestHook::registry_owner(router);
        control->load.release();
        const auto deadline = std::chrono::steady_clock::now() + 2s;
        while (router.admission_snapshot()["resident"] != 2 &&
               std::chrono::steady_clock::now() < deadline)
          std::this_thread::yield();
        sealed = router.admission_snapshot()["resident"] == 2;
        publication_allocation::armed = true;
      }
      const bool cleanup_entered = control->stopped.await();
      const bool ready_before_cleanup = router.is_model_loaded(info.model_name);
      control->stopped.release();
      const auto result = loading.get();
      publication_allocation::armed = false;
      const bool retained = RoutingHelperTestHook::pending(
          router, info.model_name, control->generation);
      const auto held = router.admission_snapshot();
      const int starts = control->starts.load();
      const bool slot_clear = !RoutingHelperTestHook::loading(router);
      control->fail_stop = false;
      if (retained)
        RoutingHelperTestHook::retry_cleanup(router, info.model_name,
                                             control->generation);
      else if (router.get_model_generation(info.model_name))
        router.unload_model(info.model_name);
      const auto after = router.admission_snapshot();
      const bool okay = started && sealed && cleanup_entered &&
                        !ready_before_cleanup && result == "bad_alloc" &&
                        retained == fail_stop && starts == 1 && slot_clear &&
                        publication_allocation::faults == prior_faults + 1 &&
                        router.generation_was_stopped(control->generation) &&
                        after["resident"] == 1 && after["active"] == 0 &&
                        after["uncertain"] == 0;
      rows.push_back({{"stop_failure", fail_stop},
                      {"startup_residency_confirmed", sealed},
                      {"publication_allocation_faults",
                       publication_allocation::faults.load() - prior_faults},
                      {"cleanup_entered", cleanup_entered},
                      {"false_ready_publication", ready_before_cleanup},
                      {"result", result},
                      {"retained_exact_generation", retained},
                      {"dispatches", starts},
                      {"loading_slot_released", slot_clear},
                      {"held_ledger", held},
                      {"passed", okay}});
    }
    RuntimeConfig::set_global(nullptr);
  }
  const int failures =
      std::count_if(rows.begin(), rows.end(),
                    [](const auto &row) { return row["passed"] == false; });
  std::printf("Publication allocation: %s\n", rows.dump().c_str());
  std::printf("Publication allocation: %zu cases, %d failures\n", rows.size(),
              failures);
  return failures ? 1 : 0;
}
