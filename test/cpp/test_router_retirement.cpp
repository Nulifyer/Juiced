#include "lemon/config_file.h"
#include "lemon/error_types.h"
#include "lemon/eviction_engine.h"
#include "lemon/global_vram_monitor.h"
#include "lemon/juiced/service.h"
#include "lemon/router.h"
#include "lemon/server_process.h"
#include "lemon/server.h"
#include "lemon/utils/path_utils.h"
#include "support/test_state_isolation.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <functional>
#include <future>
#include <fstream>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

namespace lemon {
class ControlledRetirementProcess : public ServerProcess {
public:
    ControlledRetirementProcess(std::function<void()> stop, std::function<bool()> running, std::function<bool()> exited)
        : ServerProcess({}), stop_(std::move(stop)), running_(std::move(running)), exited_(std::move(exited)) {}
    std::string start(const ServerCommand&) override { return "127.0.0.1"; }
    void stop() override { stop_(); }
    bool running() const override { return running_(); }
    bool exited() const override { return exited_(); }
private:
    std::function<void()> stop_;
    std::function<bool()> running_, exited_;
};
class RetirementWorker : public WrappedServer {
public:
    explicit RetirementWorker(const std::string& name) : WrappedServer("fixture", "error") {
        set_model_metadata(name, "retained-fixture", ModelType::LLM, DEVICE_CPU,
                           RecipeOptions("llamacpp", {{"ctx_size", 2048}}));
        ServerCommand command; command.port = 19342;
        start_owned_process(std::make_unique<ControlledRetirementProcess>(
            [this] { controlled_stop(); }, [this] { return alive && !detached; }, [this] { return !alive.load(); }), command);
        set_state(ModelState::READY);
    }
    ~RetirementWorker() override { release(); fail = false; try { WrappedServer::unload(); } catch (...) {} }
    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override {}
    bool is_backend_alive() const override { return WrappedServer::is_backend_alive(); }
    bool observes_inference_completion() const override { return true; }
    json chat_completion(const json&) override {
        ++requests;
        note_request_started(); note_request_completed();
        return {{"choices", json::array({{{"message", {{"role", "assistant"}, {"content", "served fixture"}}}}})}};
    }
    void unload() override { WrappedServer::unload(); }
    void controlled_stop() {
        ++stops;
        detached = true;
        std::unique_lock lock(mutex);
        entered = true;
        changed.notify_all();
        changed.wait(lock, [&] { return released; });
        if (fail) throw std::runtime_error("controlled unresolved child");
        alive = false;
    }
    bool await() {
        std::unique_lock lock(mutex);
        return changed.wait_for(lock, std::chrono::seconds(2), [&] { return entered; });
    }
    void release() { std::lock_guard lock(mutex); released = true; changed.notify_all(); }
    std::atomic<bool> alive{true}, fail{false}, detached{false};
    std::atomic<int> stops{0}, requests{0};
private:
    std::mutex mutex;
    std::condition_variable changed;
    bool entered = false, released = false;
};
struct RoutingHelperTestHook {
    static void factory(Router& router, std::function<std::unique_ptr<WrappedServer>(const ModelInfo&)> factory) {
        router.backend_factory_override_ = std::move(factory);
    }
    static void npu(Router& router) {
        std::unique_lock lock(router.load_mutex_);
        router.evict_all_npu_servers(lock, ResidencyClass::Standard, [] {});
    }
    static void evict(Router& router, const std::shared_ptr<RetirementWorker>& worker) {
        router.evict_if_committed(worker->get_model_name(), worker->get_instance_id());
    }
    static void stop(Router& router) { router.eviction_engine_->stop(); router.vram_monitor_->stop(); }
    static void add(Router& router, const std::shared_ptr<RetirementWorker>& worker) {
        ResourceAdmission::Work work;
        work.model = worker->get_model_name();
        work.generation = worker->get_instance_id();
        work.operation = ResourceAdmission::Operation::Load;
        auto residency = router.admission_.acquire(work);
        residency.started(); residency.retain_residency();
        worker->retain_resource_residency(std::move(residency));
        std::lock_guard lock(router.load_mutex_);
        router.loaded_servers_.push_back(worker);
    }
};
struct StatePublicationTestHook {
    static Router& router(Server& server) { return *server.router_; }
    static ModelManager& models(Server& server) { return *server.model_manager_; }
    static UpgradableFrontServer& front(Server& server) { return *server.http_front_; }
    static void stop_background(Server& server) { server.juiced_->stop(); }
    static void serving(Server& server) { server.running_ = true; }
    static void register_model(Server& server, const std::string& name) {
        const auto file = utils::path_from_utf8(server.config_dir_) / "retirement-fixture.gguf";
        if (!std::filesystem::exists(file)) {
            char header[24] = {'G', 'G', 'U', 'F', 3};
            std::ofstream output(file, std::ios::binary); output.write(header, sizeof(header));
        }
        server.model_manager_->register_user_model(name, {{"recipe", "llamacpp"}, {"source", "local_path"},
            {"checkpoints", {{"main", file.u8string()}}}, {"recipe_options", {{"ctx_size", 2048}, {"llamacpp_backend", "cpu"}}}});
        httplib::Request request; request.method = "POST"; request.path = "/v1/juiced/policy";
        request.body = json{{"model", name}, {"policy", "on-demand"}}.dump();
        httplib::Response response; server.juiced_->handle(request, response, "policy");
        if (response.status != 200) throw std::runtime_error("Could not enable the HTTP fixture model");
    }
};
}

namespace lemon {
class RetirementLoadWorker : public RetirementWorker {
public:
    RetirementLoadWorker(const std::string& name, SlotPolicy policy, std::atomic<int>& starts, DeviceType device = DEVICE_NPU)
        : RetirementWorker(name), policy_(policy), starts_(starts), device_(device) { release(); }
    DeviceType effective_device(const RecipeOptions&) const override { return device_; }
    SlotPolicy effective_slot_policy(const RecipeOptions&) const override { return policy_; }
    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override { ++starts_; }
private:
    SlotPolicy policy_;
    std::atomic<int>& starts_;
    DeviceType device_;
};
class OccupancyWorker : public RetirementWorker {
public:
    OccupancyWorker(const std::string& name, SlotPolicy policy) : RetirementWorker(name), policy_(policy) {}
    SlotPolicy effective_slot_policy(const RecipeOptions&) const override { return policy_; }
private:
    SlotPolicy policy_;
};
}

namespace lemon {
class DetachedRetirementWorker : public RetirementWorker {
public:
    explicit DetachedRetirementWorker(const std::string& name) : RetirementWorker(name) {}
    bool is_backend_alive() const override { return visible && RetirementWorker::is_backend_alive(); }
    void unload() override { visible = false; RetirementWorker::unload(); }
    std::atomic<bool> visible{true};
};
}

namespace lemon {
class ProviderRetirementWorker : public RetirementWorker {
public:
    explicit ProviderRetirementWorker(const std::string& name) : RetirementWorker(name) {}
    std::string cloud_provider() const override { return "fixture"; }
};
}

namespace {
size_t controlled_cases = 0;
void report(const char* group, const lemon::json& rows) {
    controlled_cases += rows.size();
    std::printf("Router retirement %s: %s\n", group, rows.dump().c_str());
}
int service_retirement() {
    using namespace lemon;
    using namespace std::chrono_literals;
    json results = json::array();
    for (const bool automatic : {false, true}) {
        auto settings = ConfigFile::base_defaults();
        settings["offline"] = true;
        settings["disable_model_filtering"] = true;
        settings["auto_evict"] = false;
        settings["auto_check_model_updates"] = false;
        RuntimeConfig config(settings);
        RuntimeConfig::set_global(&config);
        {
            ModelManager models;
            BackendManager backends;
            Router router(&config, &models, &backends);
            RoutingHelperTestHook::stop(router);
            auto retiring = std::make_shared<RetirementWorker>("Retiring-Fixture");
            auto other = std::make_shared<RetirementWorker>("Unrelated-Fixture");
            other->release();
            RoutingHelperTestHook::add(router, retiring);
            RoutingHelperTestHook::add(router, other);
            retiring->fail = true;
            if (automatic) retiring->set_state(ModelState::EVICTING);
            auto retirement = std::async(std::launch::async, [&] {
                try {
                    if (automatic) RoutingHelperTestHook::evict(router, retiring);
                    else router.unload_model(retiring->get_model_name());
                } catch (const std::runtime_error&) { return false; }
                return true;
            });
            const bool entered = retiring->await();
            const auto started = std::chrono::steady_clock::now();
            auto query = std::async(std::launch::async, [&] { return router.is_model_loaded(other->get_model_name()); });
            auto inference = std::async(std::launch::async, [&] {
                return router.chat_completion({{"model", other->get_model_name()},
                    {"messages", json::array({{{"role", "user"}, {"content", "fixture"}}})}, {"max_tokens", 4}});
            });
            auto competing_stop = std::async(std::launch::async, [&] {
                try { router.unload_model(retiring->get_model_name()); }
                catch (const RouterResidencyConflictException&) { return false; }
                catch (const std::runtime_error&) { return false; }
                return true;
            });
            const bool responsive = query.wait_for(250ms) == std::future_status::ready;
            const bool serving = inference.wait_for(250ms) == std::future_status::ready;
            const bool stop_excluded = competing_stop.wait_for(250ms) == std::future_status::ready;
            const auto concurrent_stops = retiring->stops.load();
            const auto concurrent_requests = other->requests.load();
            const auto held_residents = router.admission_snapshot()["resident"];
            retiring->release();
            const bool stop_reported_success = retirement.get();
            const bool unrelated_present = query.get();
            const auto served = inference.get();
            const bool duplicate_stop_success = competing_stop.get();
            const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
                std::chrono::steady_clock::now() - started).count();
            const bool retained = retiring->cleanup_pending() && retiring->alive &&
                router.admission_snapshot()["resident"] == 2;
            retiring->fail = false;
            RoutingHelperTestHook::evict(router, retiring);
            const bool retried = !retiring->alive &&
                router.admission_snapshot()["resident"] == 1 && router.is_model_loaded(other->get_model_name());
            results.push_back({{"path", automatic ? "automatic-eviction" : "native-unload"},
                {"entered_stop", entered}, {"unrelated_query_responsive_during_stop", responsive},
                {"unrelated_inference_completed_during_stop", serving},
                {"unrelated_provider_calls_during_stop", concurrent_requests},
                {"unrelated_response_success", !served.contains("error")},
                {"competing_stop_rejected_during_stop", stop_excluded && !duplicate_stop_success},
                {"simultaneous_stop_calls", concurrent_stops},
                {"stop_calls_after_confirmed_retry", retiring->stops.load()},
                {"query_elapsed_ms", elapsed}, {"resident_reservations_during_stop", held_residents},
                {"unrelated_worker_preserved", unrelated_present}, {"false_stop_success", stop_reported_success},
                {"failed_stop_retained_exact_generation", retained}, {"confirmed_retry_preserved_other_generation", retried}});
        }
        RuntimeConfig::set_global(nullptr);
    }
    report("service", results);
    return std::all_of(results.begin(), results.end(), [](const auto& item) {
        return item["unrelated_query_responsive_during_stop"] == true &&
            item["unrelated_inference_completed_during_stop"] == true &&
            item["unrelated_provider_calls_during_stop"] == 1 &&
            item["unrelated_response_success"] == true &&
            item["competing_stop_rejected_during_stop"] == true &&
            item["simultaneous_stop_calls"] == 1 && item["stop_calls_after_confirmed_retry"] == 2 &&
            item["failed_stop_retained_exact_generation"] == true && item["false_stop_success"] == false &&
            item["confirmed_retry_preserved_other_generation"] == true && item["resident_reservations_during_stop"] == 2;
    }) ? 0 : 1;
}

int helper_retirement() {
    using namespace lemon;
    using namespace std::chrono_literals;
    json results = json::array();
    for (const std::string change : {"hot", "needed", "standard", "replacement"}) {
        auto settings = ConfigFile::base_defaults();
        settings["offline"] = true;
        settings["disable_model_filtering"] = true;
        settings["auto_evict"] = false;
        settings["auto_check_model_updates"] = false;
        RuntimeConfig config(settings);
        RuntimeConfig::set_global(&config);
        {
            ModelManager models;
            BackendManager backends;
            Router router(&config, &models, &backends);
            RoutingHelperTestHook::stop(router);
            auto first = std::make_shared<DetachedRetirementWorker>("First-Helper");
            auto second = std::make_shared<RetirementWorker>("Second-Helper");
            first->set_residency_class(ResidencyClass::RoutingHelper);
            second->set_residency_class(ResidencyClass::RoutingHelper);
            second->release();
            RoutingHelperTestHook::add(router, first);
            RoutingHelperTestHook::add(router, second);
            std::shared_ptr<RetirementWorker> replacement;
            auto pruning = std::async(std::launch::async, [&] {
                try { router.reconcile_routing_helpers({}, 1); }
                catch (const std::exception&) { return false; }
                return true;
            });
            const bool entered = first->await();
            auto publication = std::async(std::launch::async, [&] {
                if (change == "hot") {
                    router.publish_residency_policy(second->get_model_name(), 2, true, second->get_instance_id());
                } else if (change == "needed") {
                    router.reconcile_routing_helpers({second->get_model_name()}, 2);
                } else if (change == "standard") {
                    if (!router.ensure_loaded_model_residency(second->get_model_name(), LoadPurpose::UserInference)) return false;
                } else {
                    router.unload_model(second->get_model_name());
                    replacement = std::make_shared<RetirementWorker>(second->get_model_name());
                    replacement->release();
                    RoutingHelperTestHook::add(router, replacement);
                }
                return true;
            });
            const bool completed_while_held = publication.wait_for(500ms) == std::future_status::ready;
            first->release();
            const bool published = publication.get();
            const bool pruned = pruning.get();
            const bool resident = router.is_model_loaded(second->get_model_name());
            const auto current = router.get_model_generation(second->get_model_name());
            const auto expected = replacement ? replacement->get_instance_id() : second->get_instance_id();
            results.push_back({{"change", change}, {"first_stop_entered", entered},
                {"publication_completed_during_stop", completed_while_held && published},
                {"old_pruning_completed", pruned}, {"current_worker_resident", resident},
                {"current_generation_preserved", current == expected},
                {"second_generation_stop_calls", second->stops.load()},
                {"replacement_stop_calls", replacement ? replacement->stops.load() : 0}});
        }
        RuntimeConfig::set_global(nullptr);
    }
    {
        auto settings = ConfigFile::base_defaults();
        settings["offline"] = true;
        settings["disable_model_filtering"] = true;
        settings["auto_evict"] = false;
        settings["auto_check_model_updates"] = false;
        RuntimeConfig config(settings);
        RuntimeConfig::set_global(&config);
        {
            ModelManager models;
            BackendManager backends;
            Router router(&config, &models, &backends);
            RoutingHelperTestHook::stop(router);
            auto victim = std::make_shared<DetachedRetirementWorker>("Pool-Victim");
            auto first = std::make_shared<RetirementWorker>("First-Promotion");
            auto second = std::make_shared<RetirementWorker>("Second-Promotion");
            first->set_residency_class(ResidencyClass::RoutingHelper);
            second->set_residency_class(ResidencyClass::RoutingHelper);
            first->release(); second->release();
            RoutingHelperTestHook::add(router, victim);
            RoutingHelperTestHook::add(router, first);
            RoutingHelperTestHook::add(router, second);
            auto promotion = std::async(std::launch::async, [&] {
                return router.ensure_loaded_model_residency(first->get_model_name(), LoadPurpose::UserInference);
            });
            const bool entered = victim->await();
            std::promise<void> attempting;
            auto competing = std::async(std::launch::async, [&] {
                attempting.set_value();
                return router.ensure_loaded_model_residency(second->get_model_name(), LoadPurpose::UserInference);
            });
            attempting.get_future().wait();
            const bool waited = competing.wait_for(250ms) != std::future_status::ready;
            victim->release();
            const bool first_promoted = promotion.get();
            const bool second_promoted = competing.get();
            const int standard = (first->alive && first->get_residency_class() == ResidencyClass::Standard ? 1 : 0) +
                                 (second->alive && second->get_residency_class() == ResidencyClass::Standard ? 1 : 0);
            results.push_back({{"change", "pool"}, {"first_stop_entered", entered},
                {"first_promotion_completed", first_promoted}, {"second_promotion_completed", second_promoted},
                {"second_promotion_waited_for_pool_transaction", waited}, {"final_standard_count", standard},
                {"second_generation_resident", router.is_model_loaded(second->get_model_name())},
                {"resident_reservations", router.admission_snapshot()["resident"]}});
        }
        RuntimeConfig::set_global(nullptr);
    }
    report("helpers", results);
    return std::all_of(results.begin(), results.end(), [](const auto& item) {
        if (item["change"] == "pool")
            return item["first_stop_entered"] == true && item["first_promotion_completed"] == true &&
                item["second_promotion_completed"] == true && item["second_promotion_waited_for_pool_transaction"] == true &&
                item["final_standard_count"] == 1 && item["second_generation_resident"] == true && item["resident_reservations"] == 1;
        return item["first_stop_entered"] == true && item["publication_completed_during_stop"] == true &&
            item["old_pruning_completed"] == true && item["current_worker_resident"] == true &&
            item["current_generation_preserved"] == true && item["replacement_stop_calls"] == 0 &&
            item["second_generation_stop_calls"] == (item["change"] == "replacement" ? 1 : 0);
    }) ? 0 : 1;
}


int bulk_retirement() {
    using namespace lemon;
    using namespace std::chrono_literals;
    json results = json::array();
    struct Case { std::string name; std::string path; bool pinned; std::string change; };
    const std::vector<Case> cases = {
        {"native-new-hot", "native", false, "hot"},
        {"native-unchanged-hot", "native", true, "none"},
        {"native-new-hot-revision", "native", true, "hot"},
        {"native-relaxed-policy", "native", true, "on-demand"},
        {"npu-new-hot", "npu", false, "hot"},
        {"exclusive-new-hot", "exclusive", false, "hot"},
        {"coexist-new-hot", "coexist", false, "hot"},
        {"exclusive-superseded-intent", "exclusive", false, "cancel"},
        {"coexist-superseded-intent", "coexist", false, "cancel"},
        {"exclusive-initial-hot", "exclusive", true, "preflight"},
        {"coexist-initial-hot", "coexist", true, "preflight"},
        {"explicit-on-demand", "explicit", false, "none"},
    };
    for (const auto& trial : cases) {
        auto settings = ConfigFile::base_defaults();
        settings["offline"] = true;
        settings["disable_model_filtering"] = true;
        settings["auto_evict"] = false;
        settings["auto_check_model_updates"] = false;
        settings["max_loaded_models"] = 10;
        RuntimeConfig config(settings); RuntimeConfig::set_global(&config);
        {
            ModelManager models; BackendManager backends;
            std::atomic<int> starts{0};
            Router router(&config, &models, &backends);
            RoutingHelperTestHook::stop(router);
            auto first = std::make_shared<RetirementWorker>("First-Victim");
            auto second = std::make_shared<RetirementWorker>("Second-Victim");
            if (trial.path != "native") {
                for (const auto& worker : {first, second})
                    worker->set_model_metadata(worker->get_model_name(), "fixture", ModelType::LLM, DEVICE_NPU,
                                               RecipeOptions("llamacpp", {{"ctx_size", 2048}}));
            }
            second->release();
            RoutingHelperTestHook::add(router, first);
            RoutingHelperTestHook::add(router, second);
            router.publish_residency_policy(second->get_model_name(), 1, trial.pinned, second->get_instance_id());
            const std::string target = "Replacement-Fixture";
            router.publish_residency_policy(target, 1, trial.path != "explicit");
            RoutingHelperTestHook::factory(router, [&](const ModelInfo& info) {
                return std::make_unique<RetirementLoadWorker>(info.model_name,
                    trial.path == "coexist" ? SlotPolicy::CoexistByType : SlotPolicy::ExclusiveNpu, starts);
            });
            auto operation = std::async(std::launch::async, [&] {
                try {
                    if (trial.path == "native") router.unload_model("");
                    else if (trial.path == "npu") RoutingHelperTestHook::npu(router);
                    else {
                        ModelInfo info; info.model_name = target; info.recipe = "llamacpp";
                        info.checkpoints["main"] = "fixture"; info.recipe_options = RecipeOptions("llamacpp", {{"ctx_size", 2048}});
                        router.load_model(target, info, RecipeOptions("llamacpp", {{"ctx_size", 2048}}), false, false,
                                          std::nullopt, LoadPurpose::UserInference, nullptr,
                                          trial.path == "explicit" ? std::nullopt : std::optional<uint64_t>(1));
                    }
                } catch (const AdmissionError& error) { return error.code; }
                catch (const RouterResidencyConflictException&) { return std::string("residency_conflict"); }
                catch (const std::exception& error) { return std::string("unexpected: ") + error.what(); }
                return std::string("success");
            });
            const bool entered = first->await();
            if (trial.change == "cancel") router.publish_residency_policy(target, 2, false);
            else if (trial.change == "hot" || trial.change == "on-demand")
                router.publish_residency_policy(second->get_model_name(), 2, trial.change == "hot", second->get_instance_id());
            first->release();
            const auto outcome = operation.get();
            const bool preserve = trial.change == "hot" || trial.change == "cancel" || trial.change == "preflight";
            results.push_back({{"case", trial.name}, {"first_stop_entered", entered}, {"outcome", outcome},
                {"expect_first_stop", trial.change != "preflight"}, {"first_stop_calls", first->stops.load()},
                {"second_stop_calls", second->stops.load()}, {"replacement_load_calls", starts.load()},
                {"expect_replacement_start", trial.path == "explicit"},
                {"second_generation_retained", router.get_model_generation(second->get_model_name()) == second->get_instance_id()},
                {"second_generation_alive", second->alive.load()}, {"expect_preservation", preserve},
                {"resident_reservations", router.admission_snapshot()["resident"]}});
        }
        RuntimeConfig::set_global(nullptr);
    }
    report("bulk", results);
    return std::all_of(results.begin(), results.end(), [](const auto& row) {
        if (row["first_stop_entered"] != row["expect_first_stop"]) return false;
        if (row["expect_preservation"] == true)
            return row["second_stop_calls"] == 0 && row["replacement_load_calls"] == 0 &&
                row["second_generation_retained"] == true && row["second_generation_alive"] == true &&
                (row["outcome"] == "policy_intent_changed" || row["outcome"] == "residency_conflict") &&
                row["resident_reservations"] == (row["expect_first_stop"] == true ? 1 : 2);
        return row["outcome"] == "success" && row["second_stop_calls"] == 1 && row["second_generation_retained"] == false &&
            row["replacement_load_calls"] == (row["expect_replacement_start"] == true ? 1 : 0);
    }) ? 0 : 1;
}


int detached_occupancy() {
    using namespace lemon;
    json rows = json::array();
    for (const std::string mode : {"standard", "exclusive", "exclusive-peer", "coexisting"}) {
        auto settings = ConfigFile::base_defaults(); settings["offline"] = true;
        settings["disable_model_filtering"] = true; settings["auto_evict"] = false;
        settings["auto_check_model_updates"] = false;
        settings["max_loaded_models"] = mode == "standard" ? 1 : 10;
        RuntimeConfig config(settings); RuntimeConfig::set_global(&config);
        {
            ModelManager models; BackendManager backends;
            std::atomic<int> starts{0};
            Router router(&config, &models, &backends);
            RoutingHelperTestHook::stop(router);
            const auto device = mode == "standard" ? DEVICE_CPU : DEVICE_NPU;
            auto old = std::make_shared<OccupancyWorker>("Detached-Occupancy",
                mode == "coexisting" ? SlotPolicy::CoexistByType : SlotPolicy::ExclusiveNpu);
            old->set_model_metadata(old->get_model_name(), "fixture", ModelType::LLM, device,
                                    RecipeOptions("llamacpp", {{"ctx_size", 2048}}));
            old->fail = true;
            RoutingHelperTestHook::add(router, old);
            const auto policy = mode == "standard" ? SlotPolicy::Standard :
                mode == "exclusive" ? SlotPolicy::ExclusiveNpu : SlotPolicy::CoexistByType;
            RoutingHelperTestHook::factory(router, [&](const ModelInfo& info) {
                return std::make_unique<RetirementLoadWorker>(info.model_name, policy, starts, device);
            });
            ModelInfo info; info.model_name = "After-Detached"; info.recipe = "llamacpp";
            info.checkpoints["main"] = "fixture"; info.recipe_options = RecipeOptions("llamacpp", {{"ctx_size", 2048}});
            const auto load = [&] {
                try { router.load_model(info.model_name, info, info.recipe_options, false, false); }
                catch (const AdmissionError& error) { return error.code; }
                catch (const std::exception&) { return std::string("unexpected_failure"); }
                return std::string("success");
            };
            auto stopping = std::async(std::launch::async, [&] {
                try { router.unload_model(old->get_model_name()); }
                catch (const std::runtime_error&) { return true; }
                return false;
            });
            const bool entered = old->await();
            const bool detached = !old->is_backend_alive() && old->alive;
            const auto during = load();
            old->release();
            const bool failed = stopping.get();
            const auto after_failure = load();
            const bool charge_held = router.admission_snapshot()["resident"] == 1;
            const auto starts_before_cleanup = starts.load();
            const bool still_detached = !old->is_backend_alive() && old->alive && old->cleanup_pending();
            old->fail = false;
            RoutingHelperTestHook::evict(router, old);
            const auto after_cleanup = load();
            rows.push_back({{"mode", mode}, {"stop_entered", entered}, {"detached_while_owned", detached},
                {"during_stop_result", during}, {"failed_stop", failed}, {"after_failed_stop_result", after_failure},
                {"failed_stop_still_detached", still_detached}, {"resident_charge_retained", charge_held},
                {"starts_before_cleanup", starts_before_cleanup}, {"after_confirmed_cleanup_result", after_cleanup},
                {"final_load_starts", starts.load()}, {"old_stop_calls", old->stops.load()},
                {"replacement_usable", router.is_model_loaded(info.model_name)}});
        }
        RuntimeConfig::set_global(nullptr);
    }
    report("detached", rows);
    return std::all_of(rows.begin(), rows.end(), [](const auto& row) {
        return row["stop_entered"] == true && row["detached_while_owned"] == true && row["failed_stop"] == true &&
            row["during_stop_result"] == "worker_cleanup_pending" && row["after_failed_stop_result"] == "worker_cleanup_pending" &&
            row["failed_stop_still_detached"] == true && row["resident_charge_retained"] == true && row["starts_before_cleanup"] == 0 &&
            row["after_confirmed_cleanup_result"] == "success" && row["final_load_starts"] == 1 && row["old_stop_calls"] == 2 &&
            row["replacement_usable"] == true;
    }) ? 0 : 1;
}

int provider_retirement() {
    using namespace lemon;
    using namespace std::chrono_literals;
    json rows = json::array();
    for (bool fail_publication : {false, true}) {
        auto settings = ConfigFile::base_defaults(); settings["offline"] = true;
        settings["disable_model_filtering"] = true; settings["auto_evict"] = false;
        settings["auto_check_model_updates"] = false;
        RuntimeConfig config(settings); RuntimeConfig::set_global(&config);
        {
            ModelManager models; BackendManager backends;
            Router router(&config, &models, &backends);
            RoutingHelperTestHook::stop(router);
            auto first = std::make_shared<ProviderRetirementWorker>("First-Cloud");
            auto second = std::make_shared<ProviderRetirementWorker>("Second-Cloud");
            second->release();
            RoutingHelperTestHook::add(router, first); RoutingHelperTestHook::add(router, second);
            if (fail_publication) {
                bool failed = false;
                try { router.uninstall_cloud_provider("fixture", []() -> bool { throw std::runtime_error("controlled publication failure"); }); }
                catch (const std::runtime_error&) { failed = true; }
                auto lease = second->try_acquire_ready_inference();
                rows.push_back({{"case", "failed-publication"}, {"publication_failed", failed},
                    {"first_cleanup_pending", first->cleanup_pending()}, {"second_cleanup_pending", second->cleanup_pending()},
                    {"second_still_usable", bool(lease)}, {"resident_reservations", router.admission_snapshot()["resident"]}});
                first->release();
            } else {
                auto removal = std::async(std::launch::async, [&] { return router.uninstall_cloud_provider("fixture", [] { return true; }); });
                const bool entered = first->await();
                auto lease = second->try_acquire_ready_inference();
                std::string reuse = "success";
                try {
                    if (!router.ensure_loaded_model_residency(second->get_model_name(), LoadPurpose::UserInference)) reuse = "absent";
                } catch (const AdmissionError& error) { reuse = error.code; }
                std::string load = "success";
                try {
                    ModelInfo info; info.model_name = second->get_model_name(); info.recipe = "llamacpp";
                    info.checkpoints["main"] = second->get_checkpoint(); info.recipe_options = second->get_recipe_options();
                    router.load_model(info.model_name, info, info.recipe_options, false, false);
                } catch (const AdmissionError& error) { load = error.code; }
                catch (const std::exception&) { load = "unexpected_failure"; }
                const bool pending = second->cleanup_pending();
                first->release();
                auto result = removal.get();
                rows.push_back({{"case", "committed-publication"}, {"first_stop_entered", entered},
                    {"second_inference_rejected_while_retiring", !bool(lease)}, {"second_cleanup_pending_during_stop", pending},
                    {"reuse_result", reuse}, {"native_load_result", load}, {"removed_workers", result["workers_removed"]},
                    {"resident_reservations_after_cleanup", router.admission_snapshot()["resident"]}});
            }

        }
        RuntimeConfig::set_global(nullptr);
    }
    report("provider", rows);
    return std::all_of(rows.begin(), rows.end(), [](const auto& row) {
        if (row["case"] == "failed-publication")
            return row["publication_failed"] == true && row["first_cleanup_pending"] == false &&
                row["second_cleanup_pending"] == false && row["second_still_usable"] == true && row["resident_reservations"] == 2;
        return row["first_stop_entered"] == true && row["second_inference_rejected_while_retiring"] == true &&
            row["second_cleanup_pending_during_stop"] == true && row["reuse_result"] == "worker_cleanup_pending" &&
            row["native_load_result"] == "worker_cleanup_pending" && row["removed_workers"] == 2 &&
            row["resident_reservations_after_cleanup"] == 0;
    }) ? 0 : 1;
}

int http_readiness() {
    using namespace lemon;
    using namespace std::chrono_literals;
    json rows = json::array();
    utils::unset_environment_variable_utf8("LEMONADE_API_KEY");
    utils::unset_environment_variable_utf8("LEMONADE_ADMIN_API_KEY");
    for (bool same_target : {false, true}) {
        const auto directory = test::make_scratch_dir(same_target ? "same-target-http" : "unrelated-http");
        utils::set_environment_variable_utf8("HF_HOME", utils::path_to_utf8(directory / "hf"));
        utils::set_environment_variable_utf8("JUICED_SNAPSHOT_DIR", utils::path_to_utf8(directory / "snapshots"));
        auto settings = ConfigFile::base_defaults(); settings["offline"] = true; settings["host"] = "127.0.0.1";
        settings["broadcast"] = false; settings["auto_evict"] = false; settings["auto_check_model_updates"] = false;
        settings["disable_model_filtering"] = true; settings["max_loaded_models"] = 2;
        settings["websocket_port"] = utils::ProcessManager::find_free_port(18000);
        auto config = std::make_shared<RuntimeConfig>(settings); RuntimeConfig::set_global(config.get());
        {
            std::atomic<int> starts{0};
            Server server(config, directory.u8string(), directory.u8string());
            StatePublicationTestHook::stop_background(server);
            auto& router = StatePublicationTestHook::router(server);
            RoutingHelperTestHook::stop(router);
            StatePublicationTestHook::register_model(server, "user.HttpReady");
            StatePublicationTestHook::register_model(server, "user.HttpCold");
            const auto ready_info = StatePublicationTestHook::models(server).get_model_info("user.HttpReady");
            const auto ready_name = StatePublicationTestHook::models(server).resolve_model_name("user.HttpReady");
            const auto target = same_target ? "user.HttpReady" : "user.HttpCold";
            const auto loading_info = StatePublicationTestHook::models(server).get_model_info(target);
            auto unavailable = std::make_shared<RetirementWorker>("Unavailable-Http");
            unavailable->detached = true;
            auto ready = std::make_shared<RetirementWorker>(ready_name);
            ready->set_model_metadata(ready_name, ready_info.checkpoint(), ModelType::LLM, DEVICE_CPU, ready_info.recipe_options);
            ready->release();
            RoutingHelperTestHook::add(router, unavailable); RoutingHelperTestHook::add(router, ready);
            RoutingHelperTestHook::factory(router, [&](const ModelInfo& info) {
                return std::make_unique<RetirementLoadWorker>(info.model_name, SlotPolicy::Standard, starts, DEVICE_CPU);
            });
            auto& front = StatePublicationTestHook::front(server);
            const int port = front.bind_to_any_port("127.0.0.1");
            if (port <= 0) throw std::runtime_error("Could not bind the HTTP readiness fixture");
            StatePublicationTestHook::serving(server);
            struct Listener {
                Server& server;
                std::thread thread;
                ~Listener() { server.stop(); if (thread.joinable()) thread.join(); }
            } listener{server, std::thread([&] { front.listen_after_bind(); })};
            front.wait_until_ready();
            std::future<std::string> loading;
            struct GateRelease {
                std::shared_ptr<RetirementWorker> worker;
                ~GateRelease() { worker->release(); }
            } release{unavailable};
            loading = std::async(std::launch::async, [&] {
                try { router.load_model(target, loading_info, loading_info.recipe_options, false, false); }
                catch (const std::exception& error) { return std::string(error.what()); }
                return std::string("success");
            });
            const bool entered = unavailable->await();
            auto request = std::async(std::launch::async, [&] {
                httplib::Client client("127.0.0.1", port); client.set_connection_timeout(2, 0); client.set_read_timeout(5, 0);
                const auto response = client.Post("/v1/chat/completions", json{{"model", "user.HttpReady"},
                    {"messages", json::array({{{"role", "user"}, {"content", "fixture"}}})}, {"max_tokens", 4}}.dump(), "application/json");
                return response ? response->status : 0;
            });
            const bool completed_while_held = request.wait_for(500ms) == std::future_status::ready;
            const auto dispatched_while_held = ready->requests.load();
            unavailable->release();
            const auto load_result = loading.get();
            const auto status = request.get();
            rows.push_back({{"same_target", same_target}, {"unrelated_stop_entered", entered},
                {"http_completed_during_stop", completed_while_held}, {"dispatches_during_stop", dispatched_while_held},
                {"http_status", status}, {"loading_result", load_result}, {"final_provider_calls", ready->requests.load()}});
        }
        RuntimeConfig::set_global(nullptr);
    }
    report("http", rows);
    return std::all_of(rows.begin(), rows.end(), [](const auto& row) {
        return row["unrelated_stop_entered"] == true && row["http_status"] == 200 && row["loading_result"] == "success" &&
            row["final_provider_calls"] == 1 && row["http_completed_during_stop"] == (row["same_target"] == false) &&
            row["dispatches_during_stop"] == (row["same_target"] == true ? 0 : 1);
    }) ? 0 : 1;
}

}
int main() {
    try {
        int failed_groups = service_retirement();
        failed_groups += helper_retirement();
        failed_groups += bulk_retirement();
        failed_groups += detached_occupancy();
        failed_groups += provider_retirement();
        failed_groups += http_readiness();
        std::printf("Router retirement: %zu controlled cases, %d failing groups\n", controlled_cases, failed_groups);
        return failed_groups ? 1 : 0;
    } catch (const std::exception& error) {
        std::fprintf(stderr, "Router retirement fixture failed: %s\n", error.what());
        return 1;
    }
}
