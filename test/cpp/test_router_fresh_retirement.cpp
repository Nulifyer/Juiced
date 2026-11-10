#include "support/router_fresh_fixture.h"

lemon::json fresh_http() {
  using namespace lemon;
  using namespace std::chrono_literals;
  json rows = json::array();
  utils::unset_environment_variable_utf8("LEMONADE_API_KEY");
  utils::unset_environment_variable_utf8("LEMONADE_ADMIN_API_KEY");
  for (bool retry : {false, true}) {
    const auto directory = test::make_scratch_dir(retry ? "fresh-retry-http"
                                                        : "fresh-initial-http");
    utils::set_environment_variable_utf8("HF_HOME",
                                         utils::path_to_utf8(directory / "hf"));
    utils::set_environment_variable_utf8(
        "JUICED_SNAPSHOT_DIR", utils::path_to_utf8(directory / "snapshots"));
    auto settings = ConfigFile::base_defaults();
    settings["offline"] = true;
    settings["host"] = "127.0.0.1";
    settings["broadcast"] = false;
    settings["auto_evict"] = false;
    settings["auto_check_model_updates"] = false;
    settings["disable_model_filtering"] = true;
    settings["max_loaded_models"] = 2;
    settings["websocket_port"] = utils::ProcessManager::find_free_port(18000);
    auto config = std::make_shared<RuntimeConfig>(settings);
    RuntimeConfig::set_global(config.get());
    {
      auto first = std::make_shared<FreshControl>();
      auto second = std::make_shared<FreshControl>();
      auto target = retry ? second : first;
      if (retry) {
        first->fail_start = true;
        first->load.release();
        first->stopped.release();
        first->destroying.release();
      }
      Server server(config, directory.u8string(), directory.u8string());
      StatePublicationTestHook::stop_background(server);
      auto &router = StatePublicationTestHook::router(server);
      RoutingHelperTestHook::stop(router);
      struct ReleaseAll {
        std::shared_ptr<FreshControl> first, second;
        ~ReleaseAll() {
          for (auto &control : {first, second}) {
            control->fail_stop = false;
            control->load.release();
            control->stopped.release();
            control->destroying.release();
          }
        }
      } release{first, second};
      StatePublicationTestHook::register_model(server, "user.HttpReady");
      StatePublicationTestHook::register_model(server, "user.HttpFresh");
      auto &models = StatePublicationTestHook::models(server);
      const auto ready_info = models.get_model_info("user.HttpReady");
      const auto ready_name = models.resolve_model_name("user.HttpReady");
      auto fresh_info = models.get_model_info("user.HttpFresh");
      const auto fresh_name = models.resolve_model_name("user.HttpFresh");
      // Explicit legacy fixture only: selected native profiles deliberately do
      // not retry.
      if (retry)
        fresh_info.extras.erase("execution_profile");
      auto ready = std::make_shared<RetirementWorker>(ready_name);
      ready->set_model_metadata(ready_name, ready_info.checkpoint(),
                                ModelType::LLM, DEVICE_CPU,
                                ready_info.recipe_options);
      ready->release();
      RoutingHelperTestHook::add(router, ready);
      int factories = 0;
      RoutingHelperTestHook::factory(router, [&](const ModelInfo &) {
        return std::make_unique<FreshWorker>(factories++ == 0 ? first : second);
      });
      const auto helper_revision =
          RoutingHelperTestHook::helper_revision(router);
      router.reconcile_routing_helpers({fresh_name}, helper_revision);
      auto &front = StatePublicationTestHook::front(server);
      const int port = front.bind_to_any_port("127.0.0.1");
      if (port <= 0)
        throw std::runtime_error("Could not bind fresh HTTP fixture");
      StatePublicationTestHook::serving(server);
      struct Listener {
        Server &server;
        std::thread thread;
        ~Listener() {
          server.stop();
          if (thread.joinable())
            thread.join();
        }
      } listener{server, std::thread([&] { front.listen_after_bind(); })};
      front.wait_until_ready();
      auto loading = std::async(std::launch::async, [&] {
        try {
          router.load_model("user.HttpFresh", fresh_info,
                            fresh_info.recipe_options, false, false, false,
                            LoadPurpose::RoutingDependency);
        } catch (const std::exception &error) {
          return std::string(error.what());
        }
        return std::string("success");
      });
      const bool started = target->load.await();
      auto reconciliation = std::async(std::launch::async, [&] {
        router.reconcile_routing_helpers({}, helper_revision + 1);
      });
      const auto deadline = std::chrono::steady_clock::now() + 2s;
      while (RoutingHelperTestHook::helper_needed(router, fresh_name) &&
             std::chrono::steady_clock::now() < deadline)
        std::this_thread::yield();
      target->load.release();
      const bool stopping = target->stopped.await();
      const auto request = [&] {
        httplib::Client client("127.0.0.1", port);
        client.set_connection_timeout(2, 0);
        client.set_read_timeout(5, 0);
        const auto response = client.Post(
            "/v1/chat/completions",
            json{{"model", "user.HttpReady"},
                 {"messages",
                  json::array({{{"role", "user"}, {"content", "fixture"}}})},
                 {"max_tokens", 4}}
                .dump(),
            "application/json");
        return response ? response->status : 0;
      };
      auto during_stop = std::async(std::launch::async, request);
      const bool stop_response_ready =
          during_stop.wait_for(500ms) == std::future_status::ready;
      target->stopped.release();
      const bool destroying = target->destroying.await();
      auto during_destruction = std::async(std::launch::async, request);
      const bool destructor_response_ready =
          during_destruction.wait_for(500ms) == std::future_status::ready;
      target->destroying.release();
      const auto load_result = loading.get();
      reconciliation.get();
      const auto stop_status = during_stop.get();
      const auto destructor_status = during_destruction.get();
      const bool okay = started && stopping && destroying &&
                        stop_response_ready && destructor_response_ready &&
                        stop_status == 200 && destructor_status == 200 &&
                        ready->requests == 2 && load_result == "success" &&
                        router.generation_was_stopped(target->generation);
      rows.push_back(
          {{"mode", retry ? "legacy-retry-http" : "initial-http"},
           {"load_started", started},
           {"stop_entered", stopping},
           {"destructor_entered", destroying},
           {"http_completed_during_stop", stop_response_ready},
           {"http_completed_during_destructor", destructor_response_ready},
           {"stop_http_status", stop_status},
           {"destructor_http_status", destructor_status},
           {"provider_calls", ready->requests.load()},
           {"loading_result", load_result},
           {"passed", okay}});
    }
    RuntimeConfig::set_global(nullptr);
  }
  return rows;
}

lemon::json captured_retirement_references() {
  using namespace lemon;
  using namespace std::chrono_literals;
  json rows = json::array();
  for (int path : {0, 1, 2}) {
    const bool transition = path != 0;
    const bool native_load = path == 2;
    auto settings = ConfigFile::base_defaults();
    settings["offline"] = true;
    settings["disable_model_filtering"] = true;
    settings["auto_evict"] = false;
    settings["auto_check_model_updates"] = false;
    settings["max_loaded_models"] = 1;
    RuntimeConfig config(settings);
    RuntimeConfig::set_global(&config);
    {
      auto control = std::make_shared<FreshControl>();
      control->load.release();
      control->stopped.release();
      ModelManager models;
      BackendManager backends;
      Router router(&config, &models, &backends);
      RoutingHelperTestHook::stop(router);
      auto first = std::make_shared<RetirementWorker>("Captured-First");
      if (!transition)
        first->set_residency_class(ResidencyClass::RoutingHelper);
      auto candidate = std::make_shared<FreshWorker>(control);
      ModelInfo info;
      info.model_name = "Captured-Candidate";
      info.recipe = "llamacpp";
      info.checkpoints["main"] = "fixture";
      info.recipe_options = RecipeOptions("llamacpp", {{"ctx_size", 2048}});
      candidate->set_model_metadata(info.model_name, "fixture", ModelType::LLM,
                                    DEVICE_CPU, info.recipe_options);
      candidate->load(info.model_name, info, info.recipe_options, false);
      candidate->set_state(ModelState::READY);
      candidate->set_residency_class(ResidencyClass::RoutingHelper);
      RoutingHelperTestHook::add(router, first);
      RoutingHelperTestHook::add(router, candidate);
      candidate.reset();
      if (native_load)
        router.reconcile_routing_helpers({info.model_name}, 1);
      std::atomic<int> replacement_starts{0};
      RoutingHelperTestHook::factory(router, [&](const ModelInfo &next) {
        return std::make_unique<RetirementLoadWorker>(
            next.model_name, SlotPolicy::Standard, replacement_starts,
            DEVICE_CPU);
      });
      auto operation = std::async(std::launch::async, [&] {
        try {
          if (native_load)
            router.load_model(info.model_name, info, info.recipe_options, false,
                              false);
          else if (transition)
            router.ensure_loaded_model_residency(info.model_name,
                                                 LoadPurpose::UserInference);
          else
            router.reconcile_routing_helpers({}, 1);
        } catch (const std::exception &) {
          return std::string("conflict");
        }
        return std::string("success");
      });
      const bool held = first->await();
      router.unload_model(info.model_name);
      const bool still_retained = control->destructors == 0;
      first->release();
      const bool destroying = control->destroying.await();
      auto query = std::async(std::launch::async, [&] {
        return router.get_model_generation(info.model_name);
      });
      const bool responsive =
          query.wait_for(250ms) == std::future_status::ready;
      control->destroying.release();
      const auto result = operation.get();
      const auto generation = query.get();
      const bool okay = held && still_retained && destroying && responsive &&
                        generation == 0 && control->destructors == 1 &&
                        router.generation_was_stopped(control->generation) &&
                        result == (transition ? "conflict" : "success") &&
                        replacement_starts == 0;
      rows.push_back(
          {{"mode", native_load  ? "removed-native-transition-target"
                    : transition ? "removed-transition-target"
                                 : "skipped-retired-candidate"},
           {"first_stop_held", held},
           {"captured_reference_retained_after_removal", still_retained},
           {"destructor_entered", destroying},
           {"query_responsive_during_destructor", responsive},
           {"generation_after_removal", generation},
           {"operation_result", result},
           {"replacement_starts", replacement_starts.load()},
           {"passed", okay}});
    }
    RuntimeConfig::set_global(nullptr);
  }
  return rows;
}

lemon::json admission_boundaries() {
  using namespace lemon;
  json rows = json::array();
  for (const std::string mode :
       {"paused", "profile-mismatch", "factory-nonstandard",
        "factory-bad-alloc", "cancel-before-dispatch", "obsolete-policy"}) {
    auto settings = ConfigFile::base_defaults();
    settings["offline"] = true;
    settings["auto_evict"] = false;
    settings["disable_model_filtering"] = true;
    settings["auto_check_model_updates"] = false;
    RuntimeConfig config(settings);
    RuntimeConfig::set_global(&config);
    {
      auto control = std::make_shared<FreshControl>();
      control->load.release();
      control->stopped.release();
      control->destroying.release();
      ModelManager models;
      BackendManager backends;
      Router router(&config, &models, &backends);
      RoutingHelperTestHook::stop(router);
      ModelInfo info;
      info.model_name = "Pre-Fresh";
      info.recipe = "llamacpp";
      info.checkpoints["main"] = "fixture";
      info.recipe_options = RecipeOptions("llamacpp", {{"ctx_size", 2048}});
      if (mode == "paused")
        router.pause_admission(true);
      if (mode == "profile-mismatch")
        router.configure_admission(
            {{"mode", "legacy"},
             {"models",
              {{info.model_name,
                {{"checkpoint", "fixture"},
                 {"options", {{"ctx_size", 4096}}},
                 {"source", "controlled-rejection"}}}}}});
      if (mode == "obsolete-policy")
        router.publish_residency_policy(info.model_name, 2, false);
      int factories = 0;
      RoutingHelperTestHook::factory(
          router, [&](const ModelInfo &) -> std::unique_ptr<WrappedServer> {
            ++factories;
            if (mode == "factory-nonstandard")
              throw 42;
            if (mode == "factory-bad-alloc")
              throw std::bad_alloc();
            return std::make_unique<FreshWorker>(control);
          });
      std::atomic<bool> cancel{mode == "cancel-before-dispatch"};
      std::string result = "success";
      int status = 0;
      try {
        router.load_model(info.model_name, info, info.recipe_options, false,
                          false, false, LoadPurpose::UserInference, &cancel,
                          mode == "obsolete-policy" ? std::optional<uint64_t>(1)
                                                    : std::nullopt);
      } catch (const AdmissionError &error) {
        result = error.code;
        status = error.status;
      } catch (const std::bad_alloc &) {
        result = "bad_alloc";
      } catch (int value) {
        result = "nonstandard:" + std::to_string(value);
      }
      const auto before = router.admission_snapshot();
      const bool slot_clear = !RoutingHelperTestHook::loading(router);
      const bool no_dispatch = control->starts == 0 && control->stops == 0;
      const bool no_ownership =
          before["active"] == 0 && before["resident"] == 0 &&
          before["uncertain"] == 0 && before["model_cache"]["references"] == 0;
      const bool expected =
          mode == "paused" ? (result == "admission_paused" && status == 503)
          : mode == "profile-mismatch"
              ? (result == "admission_profile_mismatch" && status == 409)
          : mode == "factory-nonstandard" ? result == "nonstandard:42"
          : mode == "factory-bad-alloc"   ? result == "bad_alloc"
          : mode == "obsolete-policy"
              ? (result == "policy_intent_changed" && status == 409)
              : (result == "request_cancelled" && status == 499);
      router.pause_admission(false);
      router.configure_admission({{"mode", "legacy"}});
      auto recovered = std::make_shared<FreshControl>();
      recovered->load.release();
      recovered->stopped.release();
      recovered->destroying.release();
      RoutingHelperTestHook::factory(router, [&](const ModelInfo &) {
        return std::make_unique<FreshWorker>(recovered);
      });
      router.load_model(info.model_name, info, info.recipe_options, false,
                        false);
      const bool usable =
          router.is_model_loaded(info.model_name) && recovered->starts == 1;
      router.unload_model(info.model_name);
      const auto after = router.admission_snapshot();
      const bool okay = expected && slot_clear && no_dispatch && no_ownership &&
                        usable && after["active"] == 0 &&
                        after["resident"] == 0 && after["uncertain"] == 0;
      rows.push_back({{"mode", mode},
                      {"result", result},
                      {"status", status},
                      {"factory_calls", factories},
                      {"no_process_dispatch", no_dispatch},
                      {"loading_slot_released", slot_clear},
                      {"no_retained_charge", no_ownership},
                      {"successful_retry_usable", usable},
                      {"passed", okay}});
    }
    RuntimeConfig::set_global(nullptr);
  }
  return rows;
}

lemon::json transition_reuse_stress() {
  using namespace lemon;
  json rows = json::array();
  for (bool native_load : {false, true}) {
    auto settings = ConfigFile::base_defaults();
    settings["offline"] = true;
    settings["auto_evict"] = false;
    settings["disable_model_filtering"] = true;
    settings["auto_check_model_updates"] = false;
    settings["max_loaded_models"] = 2;
    RuntimeConfig config(settings);
    RuntimeConfig::set_global(&config);
    {
      ModelManager models;
      BackendManager backends;
      Router router(&config, &models, &backends);
      RoutingHelperTestHook::stop(router);
      ModelInfo info;
      info.model_name = "Reuse-Fresh";
      info.recipe = "llamacpp";
      info.checkpoints["main"] = "fixture";
      info.recipe_options = RecipeOptions("llamacpp", {{"ctx_size", 2048}});
      router.reconcile_routing_helpers({info.model_name}, 1);
      std::atomic<int> created{0};
      RoutingHelperTestHook::factory(router, [&](const ModelInfo &) {
        ++created;
        auto control = std::make_shared<FreshControl>();
        control->load.release();
        control->stopped.release();
        control->destroying.release();
        return std::make_unique<FreshWorker>(control);
      });
      int successes = 0, conflicts = 0, missing = 0, removal_conflicts = 0;
      bool okay = true;
      for (int attempt = 0; attempt < 72; ++attempt) {
        auto control = std::make_shared<FreshControl>();
        control->load.release();
        control->stopped.release();
        control->destroying.release();
        auto worker = std::make_shared<FreshWorker>(control);
        worker->set_model_metadata(info.model_name, "fixture", ModelType::LLM,
                                   DEVICE_CPU, info.recipe_options);
        worker->load(info.model_name, info, info.recipe_options, false);
        worker->set_state(ModelState::READY);
        worker->set_residency_class(ResidencyClass::RoutingHelper);
        RoutingHelperTestHook::add(router, worker);
        worker.reset();
        std::promise<void> release;
        auto gate = release.get_future().share();
        std::atomic<bool> reuse_finished{false};
        auto reuse = std::async(std::launch::async, [&] {
          gate.wait();
          struct Finished {
            std::atomic<bool> &value;
            ~Finished() { value = true; }
          } finished{reuse_finished};
          try {
            if (native_load) {
              router.load_model(info.model_name, info, info.recipe_options,
                                false, false);
              return 1;
            }
            return router.ensure_loaded_model_residency(
                       info.model_name, LoadPurpose::UserInference)
                       ? 1
                       : 0;
          } catch (const RouterResidencyConflictException &) {
            return 2;
          } catch (const AdmissionError &) {
            return 2;
          }
        });
        auto removal = std::async(std::launch::async, [&] {
          gate.wait();
          // Keep every original concurrent round, then prove warm reuse in
          // ordered rounds. A scheduler may otherwise let unload win all races.
          if (attempt >= 48) {
            while (!reuse_finished.load())
              std::this_thread::yield();
          } else if (native_load && attempt % 2 == 0) {
            while (!RoutingHelperTestHook::standard_role(router,
                                                         info.model_name) &&
                   !reuse_finished.load())
              std::this_thread::yield();
          }
          try {
            router.unload_model(info.model_name);
            return 1;
          } catch (const ModelNotLoadedException &) {
            return 0;
          } catch (const RouterResidencyConflictException &) {
            return 2;
          } catch (const AdmissionError &) {
            return 2;
          }
        });
        release.set_value();
        const int result = reuse.get();
        const auto removed = removal.get();
        if (removed == 2)
          ++removal_conflicts;
        if (result == 1)
          ++successes;
        else if (result == 2)
          ++conflicts;
        else
          ++missing;
        if (router.get_model_generation(info.model_name))
          router.unload_model(info.model_name);
        okay &= control->destructors == 1 &&
                !RoutingHelperTestHook::loading(router);
      }
      auto snapshot = router.admission_snapshot();
      const int warm_successes =
          native_load ? successes - created.load() : successes;
      okay &= snapshot["active"] == 0 && snapshot["resident"] == 0 &&
              snapshot["uncertain"] == 0 && warm_successes >= 24;
      rows.push_back({{"mode", native_load ? "native-reuse-unload-stress"
                                           : "residency-reuse-unload-stress"},
                      {"rounds", 72},
                      {"successful_reuse", successes},
                      {"retryable_conflicts", conflicts},
                      {"already_absent", missing},
                      {"replacement_generations", created.load()},
                      {"warm_progress_gated_rounds", native_load ? 24 : 0},
                      {"reuse_completed_before_unload_rounds", 24},
                      {"simultaneous_start_rounds", native_load ? 24 : 48},
                      {"successful_warm_reuse", warm_successes},
                      {"removal_conflicts", removal_conflicts},
                      {"passed", okay}});
    }
    RuntimeConfig::set_global(nullptr);
  }
  return rows;
}

int main(int argc, char **argv) {
  using namespace lemon;
  using namespace std::chrono_literals;
  json rows = json::array();
  int failures = 0;
  const std::string group = argc > 1 ? argv[1] : "base";
  if (group != "base" && group != "faults")
    return 2;
  for (const std::string mode :
       {"initial-error", "retry-error", "initial-helper", "retry-helper",
        "initial-cancel", "retry-cancel", "initial-typed-deadline",
        "retry-typed-deadline", "initial-policy", "retry-policy",
        "initial-publish-policy", "retry-publish-policy", "initial-deadline",
        "initial-nonstandard", "retry-nonstandard"})
    for (bool fail_stop : {false, true}) {
      const bool basic = mode == "initial-error" || mode == "retry-error" ||
                         mode == "initial-helper" || mode == "retry-helper" ||
                         mode == "initial-cancel" || mode == "retry-cancel";
      if ((group == "base") != basic)
        continue;
      auto settings = ConfigFile::base_defaults();
      settings["offline"] = true;
      settings["disable_model_filtering"] = true;
      settings["auto_evict"] = false;
      settings["auto_check_model_updates"] = false;
      settings["max_loaded_models"] = 2;
      RuntimeConfig config(settings);
      RuntimeConfig::set_global(&config);
      {
        const bool retry = mode.find("retry") == 0;
        const bool helper = mode.find("helper") != std::string::npos;
        const bool cancel = mode.find("cancel") != std::string::npos;
        const bool policy = mode.find("policy") != std::string::npos;
        const bool publication = mode.find("publish") != std::string::npos;
        const bool deadline = mode == "initial-deadline";
        const bool typed_deadline =
            mode.find("typed-deadline") != std::string::npos;
        const bool nonstandard = mode.find("nonstandard") != std::string::npos;
        auto first = std::make_shared<FreshControl>();
        auto second = std::make_shared<FreshControl>();
        auto target = retry ? second : first;
        if (retry) {
          first->fail_start = true;
          first->load.release();
          first->stopped.release();
          first->destroying.release();
        }
        target->fail_start = mode.find("error") != std::string::npos;
        target->fail_stop = fail_stop;
        target->nonstandard_error = nonstandard;
        if (typed_deadline) {
          target->error_code = "completion_deadline_exceeded";
          target->error_status = 504;
        }
        ModelManager models;
        BackendManager backends;
        Router router(&config, &models, &backends);
        RoutingHelperTestHook::stop(router);
        const std::string name = "Fresh-Generation";
        if (helper)
          router.reconcile_routing_helpers({name}, 1);
        if (policy)
          router.publish_residency_policy(name, 1, true);
        ModelInfo info;
        info.model_name = name;
        info.recipe = "llamacpp";
        info.type = ModelType::LLM;
        info.checkpoints["main"] = "fixture";
        info.recipe_options = RecipeOptions("llamacpp", {{"ctx_size", 2048}});
        if (mode == "initial-error")
          info.extras["execution_trial"] = true;
        if (deadline) {
          auto effective =
              router.resolve_effective_options(info, info.recipe_options)
                  .to_json();
          effective.erase("pinned");
          router.configure_admission(
              {{"mode", "legacy"},
               {"models",
                {{name,
                  {{"checkpoint", "fixture"},
                   {"options", effective},
                   {"source", "controlled-native-deadline"},
                   {"completion_ms", 50}}}}}});
        }

        struct ReleaseAll {
          std::shared_ptr<FreshControl> first, second;
          ~ReleaseAll() {
            for (auto &control : {first, second}) {
              control->fail_stop = false;
              control->load.release();
              control->stopped.release();
              control->destroying.release();
            }
          }
        } release{first, second};
        auto ready = std::make_shared<RetirementWorker>("Ready-Fresh-Test");
        ready->release();
        RoutingHelperTestHook::add(router, ready);
        int factories = 0;
        RoutingHelperTestHook::factory(router, [&](const ModelInfo &) {
          return std::make_unique<FreshWorker>(factories++ == 0 ? first
                                                                : second);
        });
        std::atomic<bool> cancelled{false};
        int error_status = 0;
        auto loading = std::async(std::launch::async, [&] {
          try {
            router.load_model(
                name, info, info.recipe_options, false, false, false,
                helper ? LoadPurpose::RoutingDependency
                       : LoadPurpose::UserInference,
                cancel ? &cancelled : nullptr,
                policy ? std::optional<uint64_t>(1) : std::nullopt);
          } catch (const AdmissionError &error) {
            error_status = error.status;
            return error.code;
          } catch (const std::exception &error) {
            return std::string(error.what());
          } catch (int value) {
            return std::string("nonstandard:") + std::to_string(value);
          }
          return std::string("success");
        });
        const bool entered = target->load.await();
        std::future<void> reconciliation;
        if (helper) {
          reconciliation = std::async(std::launch::async, [&] {
            router.reconcile_routing_helpers({}, 2);
          });
          const auto deadline = std::chrono::steady_clock::now() + 2s;
          while (RoutingHelperTestHook::helper_needed(router, name) &&
                 std::chrono::steady_clock::now() < deadline)
            std::this_thread::yield();
        }
        const bool cleanup_slot_reserved =
            RoutingHelperTestHook::cleanup_slot_reserved(router);
        bool publication_reached = !publication;
        if (publication) {
          auto owner = RoutingHelperTestHook::registry_owner(router);
          target->load.release();
          const auto limit = std::chrono::steady_clock::now() + 2s;
          while (router.admission_snapshot()["resident"] != 2 &&
                 std::chrono::steady_clock::now() < limit)
            std::this_thread::yield();
          publication_reached = router.admission_snapshot()["resident"] == 2;
          RoutingHelperTestHook::policy_while_owned(router, owner, name, 2,
                                                    false);
        } else if (policy)
          router.publish_residency_policy(name, 2, false);
        if (cancel) {
          cancelled = true;
          target->cancellation_seen.await();
        }
        if (deadline)
          target->cancellation_seen.await();
        target->load.release();
        const bool stopping = target->stopped.await();
        auto query = std::async(std::launch::async, [&] {
          return router.get_model_generation(ready->get_model_name());
        });
        const bool responsive =
            query.wait_for(250ms) == std::future_status::ready;
        const auto snapshot = router.admission_snapshot();
        target->stopped.release();
        const bool destroying = target->destroying.await(250ms);
        auto destruction_query = std::async(std::launch::async, [&] {
          return router.get_model_generation(ready->get_model_name());
        });
        const bool destruction_responsive =
            destruction_query.wait_for(250ms) == std::future_status::ready;
        target->destroying.release();
        const auto result = loading.get();
        if (reconciliation.valid())
          reconciliation.get();
        query.get();
        destruction_query.get();
        const auto generation = target->generation.load();
        const bool retained =
            RoutingHelperTestHook::pending(router, name, generation);
        const auto held = router.admission_snapshot();
        const bool charges_retained =
            !fail_stop ||
            (held["active"].get<int>() + held["resident"].get<int>() == 2 &&
             held["uncertain"] == 0 && held["model_cache"]["references"] == 1);
        const auto stopped_before_retry =
            router.generation_was_stopped(generation);
        const auto destroyed_before_retry = target->destructors.load();
        const auto starts_before_retry =
            first->starts.load() + second->starts.load();
        const bool loading_slot_released =
            !RoutingHelperTestHook::loading(router);
        const bool initiating_error_preserved =
            cancel ? (result == "request_cancelled" && error_status == 499)
            : (deadline || typed_deadline)
                ? (result == "completion_deadline_exceeded" &&
                   error_status == 504)
            : policy
                ? (result == "policy_intent_changed" && error_status == 409)
            : nonstandard ? result == "nonstandard:42"
                          : true;
        target->fail_stop = false;
        bool retry_destructor_responsive = true,
             retry_destructor_entered = true;
        if (retained) {
          target->destroying.rearm();
          auto cleanup = std::async(std::launch::async, [&] {
            RoutingHelperTestHook::retry_cleanup(router, name, generation);
          });
          retry_destructor_entered = target->destroying.await();
          auto during_retry = std::async(std::launch::async, [&] {
            return router.get_model_generation(ready->get_model_name());
          });
          retry_destructor_responsive =
              during_retry.wait_for(250ms) == std::future_status::ready;
          target->destroying.release();
          cleanup.get();
          during_retry.get();
        }
        const bool reconciled = router.generation_was_stopped(generation);
        const auto after = router.admission_snapshot();
        const bool okay = entered && stopping && responsive &&
                          destruction_responsive && cleanup_slot_reserved &&
                          publication_reached && loading_slot_released &&
                          initiating_error_preserved && retained == fail_stop &&
                          charges_retained && retry_destructor_entered &&
                          retry_destructor_responsive &&
                          stopped_before_retry == !fail_stop &&
                          destroyed_before_retry == (fail_stop ? 0 : 1) &&
                          reconciled && after["resident"] == 1 &&
                          after["active"] == 0 && after["uncertain"] == 0 &&
                          after["model_cache"]["references"] == 0 &&
                          target->destructors == 1 &&
                          starts_before_retry == (retry ? 2 : 1) &&
                          (helper ? (result == "success") == !fail_stop
                                  : result != "success");
        rows.push_back(
            {{"mode", mode},
             {"stop_failure", fail_stop},
             {"load_entered", entered},
             {"stop_entered", stopping},
             {"query_responsive_during_stop", responsive},
             {"query_responsive_during_destructor", destruction_responsive},
             {"destructor_entered_before_retry", destroying},
             {"result", result},
             {"error_status", error_status},
             {"initiating_error_preserved", initiating_error_preserved},
             {"cleanup_slot_reserved_at_startup", cleanup_slot_reserved},
             {"successful_publication_boundary_reached", publication_reached},
             {"loading_slot_released", loading_slot_released},
             {"retained_exact_generation", retained},
             {"charges_retained", charges_retained},
             {"held_ledger", held},
             {"stopped_before_retry", stopped_before_retry},
             {"destructors_before_retry", destroyed_before_retry},
             {"starts_before_retry", starts_before_retry},
             {"exact_retry_reconciled", reconciled},
             {"retry_destructor_entered", retry_destructor_entered},
             {"query_responsive_during_retry_destructor",
              retry_destructor_responsive},
             {"final_ledger", after},
             {"passed", okay}});
        if (!okay)
          ++failures;
      }
      RuntimeConfig::set_global(nullptr);
    }
  if (group == "base")
    for (auto &row : captured_retirement_references()) {
      if (row["passed"] == false)
        ++failures;
      rows.push_back(std::move(row));
    }
  if (group == "base")
    for (auto &row : fresh_http()) {
      if (row["passed"] == false)
        ++failures;
      rows.push_back(std::move(row));
    }
  if (group == "faults") {
    for (auto &row : admission_boundaries()) {
      if (row["passed"] == false)
        ++failures;
      rows.push_back(std::move(row));
    }
    for (auto &row : transition_reuse_stress()) {
      if (row["passed"] == false)
        ++failures;
      rows.push_back(std::move(row));
    }
  }
  std::printf("Fresh retirement: %s\n", rows.dump().c_str());
  std::printf("Fresh retirement: %zu cases, %d failures\n", rows.size(),
              failures);
  return failures ? 1 : 0;
}
