#include "lemon/backends/cloud/cloud_server.h"
#include "lemon/config_file.h"
#include "lemon/error_types.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "support/test_state_isolation.h"
#include "lemon/utils/path_utils.h"
#include <fstream>

#include <atomic>
#include <cstdio>
#include <future>
#include <mutex>
#include <thread>

using namespace lemon;
static int checks = 0, failures = 0;
static void check(bool ok, const char* label) {
    ++checks; if (!ok) ++failures;
    std::fprintf(stderr, "%s: %s\n", ok ? "PASS" : "FAIL", label);
}

namespace lemon {
struct ModelPublicationTestHook {
    static bool notifications_stopped(ModelManager& models) {
        std::lock_guard<std::mutex> lock(models.models_changed_callback_mutex_);
        return models.models_changed_stopped_;
    }
    static bool watcher_stopped(ModelManager& models) {
        std::lock_guard<std::mutex> lock(models.directory_watcher_mutex_);
        return models.directory_watcher_stopped_ && !models.directory_watcher_;
    }
    static void notify(ModelManager& models) { models.notify_models_changed(); }
    static void options_barrier(ModelManager& models, std::function<void()> barrier) { models.options_persistence_barrier_ = std::move(barrier); }
    static bool writer_available(ModelManager& models) {
        std::unique_lock<std::mutex> lock(models.recipe_options_write_mutex_, std::try_to_lock);
        return lock.owns_lock();
    }
};
struct StatePublicationTestHook {
    static ModelManager& models(Server& server) { return *server.model_manager_; }
    static bool dependencies_alive(Server& server) {
        return server.cloud_registry_ && server.backend_manager_ && server.router_ &&
               server.router_->admission_snapshot().is_object();
    }
};
struct RoutingHelperTestHook {
    static void add(Router& router, const std::shared_ptr<WrappedServer>& worker) {
        std::lock_guard<std::mutex> lock(router.load_mutex_);
        router.loaded_servers_.push_back(worker);
    }
};
}

class Endpoint {
public:
    explicit Endpoint(const std::string& name) : name_(name) {
        http_.Get("/v1/models", [&](const httplib::Request& req, httplib::Response& res) {
            validate(req);
            if (block_discovery) { entered.set_value(); release.get_future().wait(); }
            res.set_content(json{{"data", json::array({{{"id", name_ + "-model"}}})}}.dump(), "application/json");
        });
        http_.Post("/v1/chat/completions", [&](const httplib::Request& req, httplib::Response& res) {
            validate(req); ++calls;
            res.set_content(R"({"choices":[{"message":{"role":"assistant","content":"fixture"}}]})", "application/json");
        });
        const auto port = http_.bind_to_any_port("127.0.0.1");
        if (port <= 0) throw std::runtime_error("Could not bind cloud fixture");
        base = "http://127.0.0.1:" + std::to_string(port) + "/v1";
        serving_ = std::thread([&] { http_.listen_after_bind(); }); http_.wait_until_ready();
    }
    ~Endpoint() { http_.stop(); serving_.join(); }
    void validate(const httplib::Request& req) {
        const auto expected_header = name_ == "a" ? "X-A-Key" : "Authorization";
        const auto expected_value = name_ == "a" ? "a-key" : "Bearer b-key";
        if (req.get_header_value(expected_header) != expected_value) ++mismatches;
    }
    CloudProviderRegistry::InstallOptions options() const {
        CloudProviderRegistry::InstallOptions options;
        options.allow_insecure_http = true;
        options.auth_header_name = name_ == "a" ? "X-A-Key" : "Authorization";
        options.auth_header_prefix = name_ == "a" ? "" : "Bearer ";
        options.wire_format = "openai";
        return options;
    }
    std::string key() const { return name_ + "-key"; }
    std::string base;
    std::atomic<int> calls{0}, mismatches{0};
    std::atomic<bool> block_discovery{false};
    std::promise<void> entered, release;
private:
    std::string name_;
    httplib::Server http_;
    std::thread serving_;
};

class Worker : public backends::CloudServer {
public:
    Worker(const std::string& name, const std::string& provider, ModelManager& models, CloudProviderRegistry& registry)
        : CloudServer(provider, "error", &models, nullptr, &registry) {
        auto options = RecipeOptions("cloud", json::object());
        set_model_metadata(name, "unit-model", ModelType::LLM, DEVICE_CPU, options);
        ModelInfo info; info.cloud_provider = provider; info.checkpoints["main"] = "unit-model";
        load(name, info, options); set_state(ModelState::READY);
    }
    void unload() override {
        if (fail_unload.exchange(false)) throw std::runtime_error("controlled cleanup failure");
        CloudServer::unload();
    }
    std::atomic<bool> fail_unload{false};
};

static void discovery_boundaries() {
    for (int kind = 0; kind < 3; ++kind) {
        Endpoint a("a"), b("b");
        auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["disable_model_filtering"] = true;
        RuntimeConfig config(defaults); RuntimeConfig::set_global(&config);
        {
            CloudProviderRegistry registry; ModelManager models; models.set_cloud_registry(&registry);
            const auto root = lemon::test::make_scratch_dir("discovery-locks");
            const auto name = "Discovery-" + root.filename().string();
            const auto added = "Concurrent-" + root.filename().string();
            const auto overlay = "user.Overlay-" + root.filename().string();
            const auto file = root / "metadata.gguf";
            { std::ofstream out(file, std::ios::binary); out << "GGUF"; const uint32_t version = 3; const uint64_t zero = 0; out.write(reinterpret_cast<const char*>(&version), 4); out.write(reinterpret_cast<const char*>(&zero), 8); out.write(reinterpret_cast<const char*>(&zero), 8); }
            const json definition = {{"recipe", "llamacpp"}, {"source", "local_path"}, {"checkpoints", {{"main", file.string()}}}, {"labels", json::array({"chat"})}};
            models.register_user_model(name, definition, "local_path");
            auto info = models.get_model_info(name);
            info.recipe_options = RecipeOptions("llamacpp", {{"ctx_size", 3072}});
            registry.install("fixture", a.base, a.options(), {}, a.key());
            config.set({{"offline", false}}); models.invalidate_models_cache();
            a.block_discovery = true;
            auto entered = a.entered.get_future();
            auto writer = std::async(std::launch::async, [&] {
                if (kind == 0) models.save_model_options(info);
                else if (kind == 1) models.update_saved_model_options(name, {{"ctx_size", 3072}}, "llamacpp");
                else models.select_execution_profile(name, {{"id", "fixture-profile"}, {"recipe", "llamacpp"}, {"options", {{"ctx_size", 3072}}}});
            });
            const bool requested = entered.wait_for(std::chrono::seconds(5)) == std::future_status::ready;
            const bool writer_free = ModelPublicationTestHook::writer_available(models);
            auto registration = std::async(std::launch::async, [&] { models.register_user_model(added, definition, "local_path"); });
            const bool registry_free = registration.wait_for(std::chrono::seconds(3)) == std::future_status::ready;
            const auto extra = root / "extra"; std::filesystem::create_directories(extra);
            auto directory = std::async(std::launch::async, [&] { models.set_extra_models_dir(extra.string()); });
            const bool directory_free = directory.wait_for(std::chrono::seconds(3)) == std::future_status::ready;
            registry.install("fixture", b.base, b.options(), {}, b.key()); models.invalidate_models_cache();
            a.block_discovery = false; a.release.set_value();
            directory.get();
            registration.get(); writer.get();
            check(requested && writer_free && registry_free && directory_free,
                  "all three native options/profile writers await provider discovery without holding shared cache or writer locks");
            check(models.model_exists(added) && models.model_exists("fixture.b-model") && !models.model_exists("fixture.a-model"),
                  "publication retries changed native revisions and discards delayed provider results while preserving concurrent registration");
            check(models.get_model_info(name).recipe_options.get_option("ctx_size") == 3072 && models.get_saved_model_options(name).at("ctx_size") == 3072,
                  "options/profile publication remains coherent after an overlapping cache rebuild");
            models.update_saved_model_options(overlay, {{"ctx_size", 512}});
            models.register_user_model(overlay, definition, "local_path");
            check(models.get_model_info(overlay).recipe_options.get_option("ctx_size") == 512,
                  "unknown-model import option overlays survive the committed writer snapshot boundary");
            check(a.mismatches == 0 && b.mismatches == 0,
                  "external discovery keeps destination and credentials paired through native revision retries");
        }
        RuntimeConfig::set_global(nullptr);
    }
}

static void mutation_publication_boundaries() {
    auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["disable_model_filtering"] = true;
    RuntimeConfig config(defaults); RuntimeConfig::set_global(&config);
    const auto root = lemon::test::make_scratch_dir("model-write-boundary");
    const auto name = "WriteBoundary-" + root.filename().string();
    const auto actor = "NotifyBoundary-" + root.filename().string();
    const auto file = root / "metadata.gguf";
    { std::ofstream out(file, std::ios::binary); out << "GGUF"; const uint32_t version = 3; const uint64_t zero = 0; out.write(reinterpret_cast<const char*>(&version), 4); out.write(reinterpret_cast<const char*>(&zero), 8); out.write(reinterpret_cast<const char*>(&zero), 8); }
    json definition = {{"recipe", "llamacpp"}, {"source", "local_path"}, {"checkpoints", {{"main", file.string()}}}, {"recipe_options", {{"ctx_size", 4096}}}, {"labels", json::array({"chat"})}};
    {
        ModelManager models;
        models.register_user_model(name, definition, "local_path");
        std::promise<void> entered, release; auto released = release.get_future().share();
        ModelPublicationTestHook::options_barrier(models, [&] { entered.set_value(); released.wait(); });
        auto writer = std::async(std::launch::async, [&] { models.update_saved_model_options(name, {{"llamacpp_backend", "cpu"}}, "llamacpp"); });
        const bool staged = entered.get_future().wait_for(std::chrono::seconds(5)) == std::future_status::ready;
        const bool mutation_free = std::async(std::launch::async, [&] { auto mutation = models.model_mutation(name, false); return mutation.owns_lock(); }).get();
        definition["recipe_options"]["ctx_size"] = 8192;
        auto registration = std::async(std::launch::async, [&] { models.register_user_model(name, definition, "local_path"); });
        const bool overtook = registration.wait_for(std::chrono::milliseconds(100)) == std::future_status::ready;
        const auto before = models.get_model_info(name).recipe_options.get_option("ctx_size");
        release.set_value(); writer.get(); registration.get(); ModelPublicationTestHook::options_barrier(models, {});
        check(staged && !mutation_free && !overtook && before == 4096,
              "registration cannot overtake an options transaction after its committed recipe/default snapshot");
        check(models.get_model_info(name).recipe_options.get_option("ctx_size") == 8192 && models.get_saved_model_options(name).at("llamacpp_backend") == "cpu",
              "successful registration and saved options publish one coherent final view without stale cached defaults");
        std::atomic<int> calls{0}; std::atomic<bool> released_before_effect{false};
        models.set_models_changed_callback([&](uint64_t) {
            ++calls;
            released_before_effect = std::async(std::launch::async, [&] { auto mutation = models.model_mutation(actor, false); return mutation.owns_lock(); }).get();
        });
        {
            auto outer = models.model_mutation(actor);
            models.register_user_model(actor, {{"recipe", COLLECTION_ROUTER_MODEL_RECIPE}, {"labels", json::array({"chat"})}});
            check(calls == 0, "nested model changes defer notifications until the outer publication claim ends");
        }
        check(calls == 1 && released_before_effect,
              "model-change callbacks run once after mutation ownership releases, allowing native reentry");
        models.set_models_changed_callback({}); models.unregister_user_model(actor);
    }
    {
        ModelManager models;
        check(models.get_model_info(name).recipe_options.get_option("ctx_size") == 8192 && models.get_saved_model_options(name).at("llamacpp_backend") == "cpu",
              "restart readback matches the final registered recipe/default and saved-option publication");
    }
    RuntimeConfig::set_global(nullptr);
}

static bool wait_until(const std::function<bool()>& predicate) {
    const auto end = std::chrono::steady_clock::now() + std::chrono::seconds(10);
    while (std::chrono::steady_clock::now() < end) {
        if (predicate()) return true;
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    return predicate();
}

static void notification_shutdown() {
    auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto config = std::make_shared<RuntimeConfig>(defaults); RuntimeConfig::set_global(config.get());
    const auto root = lemon::test::make_scratch_dir("notification-shutdown");
    const auto watched = root / "watched"; std::filesystem::create_directories(watched);
    // Exercise the actual native watcher while Server owns its dependencies.
    config->set({{"extra_models_dir", watched.string()}});
    for (bool destroy_server : {false, true}) {
        auto server = std::make_unique<Server>(config, root.string(), root.string());
        auto& models = StatePublicationTestHook::models(*server);
        std::promise<void> release; auto released = release.get_future().share();
        std::atomic<bool> entered{false}, dependencies_alive{false}; std::atomic<int> calls{0};
        Server* instance = server.get();
        models.set_models_changed_callback([&](uint64_t) {
            if (entered.exchange(true)) return;
            ++calls;
            released.wait();
            models.get_supported_models();
            dependencies_alive = StatePublicationTestHook::dependencies_alive(*instance);
        });
        // Watcher startup differs by OS. Wait for an observed callback rather
        // than assuming a scheduler delay establishes readiness.
        const auto end = std::chrono::steady_clock::now() + std::chrono::seconds(10);
        int sequence = 0;
        while (!entered && std::chrono::steady_clock::now() < end) {
            std::ofstream(watched / (std::to_string(destroy_server) + "-" + std::to_string(++sequence) + ".txt")) << "metadata";
            // macOS coalesces notifications until the tree has been quiet for
            // 200ms. Continuous 50ms writes would postpone that callback forever.
            const auto quiet = std::chrono::steady_clock::now() + std::chrono::milliseconds(500);
            while (!entered && std::chrono::steady_clock::now() < quiet)
                std::this_thread::sleep_for(std::chrono::milliseconds(10));
        }
        check(entered, "a real ModelManager directory notification reaches the controlled callback");
        if (!entered) { release.set_value(); continue; }
        auto shutdown = std::async(std::launch::async, [&] {
            if (destroy_server) server.reset(); else models.stop_model_notifications();
        });
        const bool closed = wait_until([&] { return ModelPublicationTestHook::notifications_stopped(models); });
        const bool pending = shutdown.wait_for(std::chrono::milliseconds(50)) != std::future_status::ready;
        auto read = std::async(std::launch::async, [&] { return models.get_supported_models().size(); });
        const bool readable = read.wait_for(std::chrono::seconds(3)) == std::future_status::ready;
        // All readers finish while the paused callback still owns its lifetime.
        read.get();
        std::future<void> directory;
        if (!destroy_server) directory = std::async(std::launch::async, [&] { models.set_extra_models_dir((root / "late").string()); });
        models.set_models_changed_callback([&](uint64_t) { ++calls; });
        ModelPublicationTestHook::notify(models);
        check(closed && pending && readable && calls == 1,
              "shutdown rejects new callbacks while draining without holding the model cache lock");
        release.set_value();
        if (directory.valid()) directory.get();
        shutdown.get();
        check(dependencies_alive && calls == 1,
              "a delayed watcher finishes with provider/backend/Router dependencies alive");
        if (destroy_server) check(BackendManager::global() == nullptr,
                                  "Server teardown clears the process backend accessor after its consumers stop");
        if (!destroy_server) {
            models.stop_model_notifications();
            models.set_extra_models_dir((root / "late-again").string());
            models.set_models_changed_callback([&](uint64_t) { ++calls; });
            ModelPublicationTestHook::notify(models);
            check(ModelPublicationTestHook::watcher_stopped(models) && calls == 1,
                  "terminal stop is idempotent and concurrent/later configuration cannot restart watcher or callbacks");
        }
    }

    const auto failed = root / "failed-construction";
    std::filesystem::create_directories(failed / "juiced/state.json.tmp");
    bool rejected = false;
    try { Server invalid(config, failed.string(), failed.string()); }
    catch (const std::exception&) { rejected = true; }
    check(rejected && BackendManager::global() == nullptr,
          "failed Server construction drains model activity and clears its backend accessor before returning failure");
    RuntimeConfig::set_global(nullptr);
}

int main(int argc, char** argv) {
    if (argc > 1 && std::string(argv[1]) == "--shutdown") {
        notification_shutdown();
        std::fprintf(stderr, "Model shutdown: %d checks, %d failures\n", checks, failures);
        return failures ? 1 : 0;
    }
    if (argc > 1 && std::string(argv[1]) == "--discovery") {
        discovery_boundaries(); mutation_publication_boundaries();
        std::fprintf(stderr, "Discovery boundaries: %d checks, %d failures\n", checks, failures);
        return failures ? 1 : 0;
    }
    Endpoint a("a"), b("b");
    auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["auto_evict"] = false;
    RuntimeConfig config(defaults); RuntimeConfig::set_global(&config);
    {
        ModelManager models; CloudProviderRegistry registry; models.set_cloud_registry(&registry);
        registry.install("fixture", a.base, a.options(), {}, a.key());
        auto worker = std::make_shared<Worker>("fixture-worker", "fixture", models, registry);
        const json request = {{"model", "fixture-worker"}, {"messages", json::array({{{"role", "user"}, {"content", "fixture"}}})}};
        std::atomic<int> errors{0};
        std::vector<std::thread> readers;
        for (int i = 0; i < 4; ++i) readers.emplace_back([&] {
            for (int j = 0; j < 25; ++j) if (worker->chat_completion(request).contains("error")) ++errors;
        });
        for (int i = 0; i < 100; ++i) {
            auto& endpoint = i % 2 ? a : b;
            registry.install("fixture", endpoint.base, endpoint.options(), {}, endpoint.key());
        }
        for (auto& reader : readers) reader.join();
        check(errors == 0 && a.mismatches == 0 && b.mismatches == 0 && a.calls + b.calls == 100,
              "concurrent forwarding uses a coherent endpoint, key and auth-header snapshot");
        auto changed = b.options(); changed.wire_format = "anthropic";
        registry.install("fixture", b.base, changed, {}, b.key());
        const auto calls = a.calls + b.calls;
        check(worker->chat_completion(request).contains("error") && a.calls + b.calls == calls,
              "wire-format updates reject an incompatible native path before dispatch");
        changed = a.options(); changed.allow_insecure_http = false;
        registry.install("fixture", a.base, changed, {}, a.key());
        check(worker->chat_completion(request).contains("error") && a.calls + b.calls == calls,
              "plaintext permission is evaluated from the same credential snapshot");
        registry.install("fixture", a.base, a.options(), {}, a.key());
        registry.clear_runtime_key("fixture");
        const auto missing_key = worker->chat_completion(request);
        check(ErrorResponse::http_status(missing_key) == 401 && ErrorResponse::code(missing_key) == "cloud_auth_required" && a.calls + b.calls == calls,
              "clearing a key prevents forwarding and preserves actionable authentication status");

        models.get_supported_models();
        registry.install("fixture", a.base, a.options(), {}, a.key());
        a.block_discovery = true;
        auto stale = std::async(std::launch::async, [&] { return models.refresh_cloud_models("fixture"); });
        a.entered.get_future().wait();
        registry.install("fixture", b.base, b.options(), {}, b.key());
        check(models.refresh_cloud_models("fixture") == 1, "replacement endpoint discovery publishes its current revision");
        a.release.set_value();
        check(stale.get() == 0 && models.model_exists("fixture.b-model") && !models.model_exists("fixture.a-model"),
              "delayed stale discovery cannot replace or erase current model metadata");
        check(a.mismatches == 0 && b.mismatches == 0, "discovery destination and credentials remain paired during replacement");

        registry.install("other", b.base, b.options(), {}, b.key());
        auto unrelated = std::make_shared<Worker>("other-worker", "other", models, registry);
        Router router(&config, &models, nullptr); router.set_cloud_registry(&registry);
        RoutingHelperTestHook::add(router, worker); RoutingHelperTestHook::add(router, unrelated);
        auto active = worker->try_acquire_ready_inference();
        bool published = false, busy = false;
        try { router.uninstall_cloud_provider("fixture", [&] { published = true; return registry.uninstall("fixture"); }); }
        catch (const RouterResidencyConflictException&) { busy = true; }
        check(busy && !published && registry.is_installed("fixture") && worker->is_backend_alive(),
              "busy provider uninstall rejects before persistence or worker effects");
        active.release();
        bool failed = false, acquisition_blocked = false;
        try {
            router.uninstall_cloud_provider("fixture", [&]() -> bool {
                acquisition_blocked = !worker->try_acquire_ready_inference();
                throw std::runtime_error("controlled publication failure");
            });
        } catch (const std::runtime_error&) { failed = true; }
        auto recovered = worker->try_acquire_ready_inference();
        check(failed && acquisition_blocked && bool(recovered) && registry.is_installed("fixture"),
              "publication failure cancels retirement and restores acquisition without stopping the generation");
        recovered.release();
        const auto inventory = router.get_all_loaded_models();
        check(inventory.size() == 2 && inventory[0].at("cloud_provider") == "fixture", "native worker inventory reports exact provider ownership");
        worker->fail_unload = true;
        auto pending = router.uninstall_cloud_provider("fixture", [&] { return registry.uninstall("fixture"); });
        check(pending["removed"] == true && pending["cleanup_pending"].size() == 1 && !worker->try_acquire_ready_inference() && unrelated->is_backend_alive(),
              "committed cleanup failure retains a retryable nonacquirable exact generation and preserves other providers");
        auto completed = router.uninstall_cloud_provider("fixture", [&] { return registry.uninstall("fixture"); });
        check(completed["removed"] == false && completed["workers_removed"] == 1 && completed["cleanup_pending"].empty() &&
              router.get_all_loaded_models().size() == 1 && unrelated->is_backend_alive(),
              "cleanup retry succeeds after registry removal and retires only provider-owned generations");
    }
    RuntimeConfig::set_global(nullptr);
    std::fprintf(stderr, "Cloud runtime: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
