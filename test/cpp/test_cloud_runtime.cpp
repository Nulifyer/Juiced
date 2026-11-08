#include "lemon/backends/cloud/cloud_server.h"
#include "lemon/config_file.h"
#include "lemon/error_types.h"
#include "lemon/router.h"
#include "support/test_state_isolation.h"

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

int main() {
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
