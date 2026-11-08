#include "lemon/backends/backend_registry.h"
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
    static void retire(Router& router, const std::shared_ptr<AdmissionWorker>& worker) {
        std::lock_guard<std::mutex> lock(router.load_mutex_);
        router.evict_server(worker.get());
    }
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
    {
        ModelInfo embedding; embedding.type = ModelType::EMBEDDING;
        RecipeOptions options("llamacpp", {{"ctx_size", 1024}});
        backends::ops_for("llamacpp")->resolve_runtime_options(embedding, options);
        check(options.get_option("ctx_size") == 8192,
              "embedding context floor is resolved before reservation and stored as effective execution settings");
        RecipeOptions native("llamacpp", {{"ctx_size", 0}});
        backends::ops_for("llamacpp")->resolve_runtime_options(embedding, native);
        check(native.get_option("ctx_size") == 8192,
              "embedding model-native context resolves to the actual positive backend floor");
    }
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
            {"request_limits", {{"input_bytes", 65536}, {"batch", 1}, {"output_tokens", 128}}},
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
        json request = {{"model", worker->get_model_name()}, {"messages", json::array()}, {"max_tokens", 32}, {"priority", "essential"}};
        auto result = router.chat_completion(request);
        check(!result.contains("error") && worker->calls == 1 && router.admission_snapshot()["active"] == 0,
              "native request reserves and releases its declared resources");
        for (const auto& changes : std::vector<json>{
                {{"max_tokens", 129}}, {{"max_tokens", 0}}, {{"n", 2}},
                {{"messages", json::array({{{"role", "user"}, {"content", std::string(12000, 'x')}}})}},
                {{"messages", json::array({{{"role", "user"}, {"content", json::array({{{"type", "image_url"}, {"image_url", "fixture"}}})}}})}},
                {{"width", 4096}, {"image_data", "fixture"}}}) {
            auto invalid = request; invalid.update(changes);
            result = router.chat_completion(invalid);
            check(result.contains("error") && worker->calls == 1 && router.admission_snapshot()["active"] == 0,
                  "unbounded or oversized request is rejected before provider dispatch");
        }
        auto image_request = request;
        worker->set_model_metadata(worker->get_model_name(), worker->get_checkpoint(), ModelType::IMAGE, DEVICE_CPU,
                                   worker->get_recipe_options());
        result = router.chat_completion(image_request);
        check(result.contains("error") && worker->calls == 1,
              "image workloads cannot use an unqualified flat protected request estimate");
        worker->set_model_metadata(worker->get_model_name(), worker->get_checkpoint(), ModelType::LLM, DEVICE_CPU,
                                   worker->get_recipe_options());
        bool realtime_rejected = false;
        try { router.acquire_streaming_transcription(worker->get_model_name()); }
        catch (const AdmissionError& e) { realtime_rejected = e.code == "operation_unqualified"; }
        check(realtime_rejected && worker->calls == 1,
              "protected Realtime rejects before a nonstreaming provider can choose batch fallback");
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
    {
        auto defaults = ConfigFile::base_defaults();
        defaults["auto_evict"] = false; defaults["auto_check_model_updates"] = false;
        RuntimeConfig config(defaults); RuntimeConfig::set_global(&config);
        ModelManager models;
        Router router(&config, &models, nullptr);
        auto original = std::make_shared<AdmissionWorker>();
        RoutingHelperTestHook::add(router, original);
        std::weak_ptr<AdmissionWorker> retired = original;
        std::promise<void> waiting, resume;
        auto resumed = resume.get_future();
        auto replacement = std::async(std::launch::async, [&, selected = original] {
            waiting.set_value(); resumed.wait();
            try { RoutingHelperTestHook::retire(router, selected); return false; }
            catch (const std::exception&) { return true; }
        });
        waiting.get_future().wait();
        router.unload_model(original->get_model_name());
        original.reset();
        auto current = std::make_shared<AdmissionWorker>();
        RoutingHelperTestHook::add(router, current);
        check(!retired.expired(), "replacement retains the selected generation while admission waits");
        resume.set_value();
        check(replacement.get() && current->get_state() == ModelState::READY,
              "a stale replacement cannot retire a newly registered generation with the same model name");
        check(retired.expired(), "rejected replacement releases its retained generation");
        auto acquired = current->try_acquire_inference();
        bool rejected = false;
        try { RoutingHelperTestHook::retire(router, current); }
        catch (const std::exception&) { rejected = true; }
        check(rejected && current->get_state() == ModelState::IN_USE,
              "native retirement cannot interrupt an acquisition that won the race");
        acquired.release();
    }
    RuntimeConfig::set_global(nullptr);
    std::filesystem::remove_all(root);
    std::printf("Router admission: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
