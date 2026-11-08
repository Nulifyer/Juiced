#include "lemon/config_file.h"
#include "lemon/request_identity.h"
#include "lemon/router.h"
#include "lemon/utils/path_utils.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <filesystem>
#include <future>
#include <thread>

namespace lemon {
class AdmissionWorker : public WrappedServer {
public:
    AdmissionWorker() : WrappedServer("admission-fixture", "error") {
        set_model_metadata("admission-model", "immutable-fixture", ModelType::LLM, DEVICE_CPU,
                           RecipeOptions("llamacpp", {{"ctx_size", 2048}}));
        set_state(ModelState::READY);
    }
    bool observes_inference_completion() const override { return observed; }
    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override {}
    void unload() override { set_state(ModelState::UNLOADED); }
    bool is_backend_alive() const override { return get_state() != ModelState::UNLOADED; }
    json chat_completion(const json&) override {
        note_request_started();
        ++calls;
        running = true;
        while (hold && !current_request_cancel_context().cancelled()) std::this_thread::sleep_for(std::chrono::milliseconds(2));
        running = false;
        if (current_request_cancel_context().cancelled()) return {{"error", {{"message", "fixture cancelled"}}}};
        note_request_completed();
        return {{"choices", json::array({{{"message", {{"role", "assistant"}, {"content", "fixture"}}}}})}};
    }
    bool observed = true;
    std::atomic<int> calls{0};
    std::atomic<bool> hold{false}, running{false};
};
struct RoutingHelperTestHook {
    static void add(Router& router, const std::shared_ptr<AdmissionWorker>& worker) {
        std::lock_guard<std::mutex> lock(router.load_mutex_);
        router.loaded_servers_.push_back(worker);
    }
};
}

int main() {
    using namespace lemon;
    using namespace std::chrono_literals;
    int failures = 0, checks = 0;
    auto check = [&](bool ok, const char* label) {
        ++checks;
        if (!ok) { ++failures; std::fprintf(stderr, "FAIL: %s\n", label); }
    };
    auto until = [&](auto condition) {
        for (int i = 0; i < 300; ++i) {
            if (condition()) return true;
            std::this_thread::sleep_for(2ms);
        }
        return false;
    };
    auto root = std::filesystem::temp_directory_path() /
        ("juiced-router-admission-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    std::filesystem::create_directories(root);
    utils::set_cache_dir(utils::path_to_utf8(root));
    utils::set_config_dir(utils::path_to_utf8(root));
    utils::set_models_dir(utils::path_to_utf8(root / "hf"));
    {
        auto defaults = ConfigFile::base_defaults();
        defaults["auto_evict"] = false; defaults["disable_model_filtering"] = true; defaults["auto_check_model_updates"] = false;
        RuntimeConfig config(defaults); RuntimeConfig::set_global(&config);
        ModelManager models;
        auto worker = std::make_shared<AdmissionWorker>();
        json profile = {{"checkpoint", worker->get_checkpoint()}, {"options", worker->get_recipe_options().to_json()},
            {"priority", "interactive"}, {"concurrency", 1}, {"completion_ms", 1000},
            {"load_bytes", {{"host", 1024}}}, {"resident_bytes", {{"host", 512}}},
            {"request_bytes", {{"host", 256}}}, {"state_bytes", {{"host", 64}}},
            {"source", "native synthetic fixture"}};
        json admission = {{"mode", "protected"}, {"interactive_wait_ms", 120},
            {"pools", {{"host", {{"capacity_bytes", 1048576}, {"source", "host physical memory"}}}}},
            {"models", {{worker->get_model_name(), profile}}}};
        config.set({{"admission", admission}});
        Router router(&config, &models, nullptr);
        RoutingHelperTestHook::add(router, worker);
        std::atomic<bool> allowed{true};
        router.set_model_access_policy([&](const std::string&) { return allowed.load(); });
        json request = {{"model", worker->get_model_name()}, {"messages", json::array()}, {"priority", "essential"}};
        auto result = router.chat_completion(request);
        check(!result.contains("error") && worker->calls == 1 && router.admission_snapshot()["active"] == 0,
              "native request reserves and releases its declared resources");
        worker->observed = false;
        result = router.chat_completion(request);
        check(result.contains("error") && result["error"]["details"]["code"] == "operation_unqualified" && worker->calls == 1,
              "unobserved provider cannot enter protected execution even with a resource profile");
        worker->observed = true;
        result = router.embeddings({{"model", worker->get_model_name()}, {"input", "fixture"}});
        check(result.contains("error") && router.admission_snapshot()["active"] == 0 && worker->calls == 1,
              "unsupported local capability cannot quarantine a managed generation");
        router.pause_admission(true);
        result = router.chat_completion(request);
        check(result.contains("error") && worker->calls == 1 && !worker->is_busy(),
              "maintenance admission rejects before provider execution");
        router.pause_admission(false);
        worker->hold = true;
        auto first = std::async(std::launch::async, [&] { return router.chat_completion(request); });
        check(until([&] { return worker->running.load(); }), "first native request reaches provider");
        auto queued = std::async(std::launch::async, [&] { return router.chat_completion(request); });
        check(until([&] { return router.admission_snapshot()["pending"] == 1; }), "second native request queues atomically");
        auto inspection = std::async(std::launch::async, [&] { return router.get_all_loaded_models(); });
        check(inspection.wait_for(100ms) == std::future_status::ready && inspection.get().size() == 1,
              "queue waiting does not hold the Router lifecycle mutex");
        result = queued.get();
        check(result.contains("error") && result["error"]["details"]["code"] == "queue_deadline_exceeded" && worker->calls == 2,
              "client-supplied priority cannot bypass the declared queue bound");
        worker->hold = false;
        check(!first.get().contains("error"), "first provider completes normally");
        check(router.admission_snapshot()["active"] == 0 && router.admission_snapshot()["pending"] == 0,
              "native terminal paths leave no resource claims");
        worker->hold = true;
        std::atomic<bool> disconnected{false};
        auto cancelled = std::async(std::launch::async, [&] {
            NativeRequestContext context;
            context.identity.principal = "fixture-client";
            context.cancelled = [&] { return disconnected.load(); };
            RequestContextScope scope(context);
            return router.chat_completion(request);
        });
        check(until([&] { return worker->running.load(); }), "cancellable native request starts");
        disconnected = true;
        check(cancelled.get().contains("error") && router.admission_snapshot()["uncertain"] == 1,
              "socket cancellation retains reservation until upstream reconciliation");
        const auto before = worker->calls.load();
        result = router.chat_completion(request);
        check(result.contains("error") && worker->calls == before,
              "unreconciled provider cannot reuse the supposedly freed slot");
        worker->hold = false;
        router.unload_model(worker->get_model_name());
        check(router.admission_snapshot()["uncertain"] == 0 && router.admission_snapshot()["active"] == 0,
              "confirmed unload reconciles the exact worker generation");
    }
    RuntimeConfig::set_global(nullptr);
    std::filesystem::remove_all(root);
    std::printf("Router admission: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
