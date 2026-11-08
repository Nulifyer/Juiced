#include "lemon/backends/llamacpp/llamacpp_server.h"
#include "lemon/config_file.h"
#include "lemon/ollama_api.h"
#include "lemon/router.h"
#include "support/test_state_isolation.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <future>
#include <thread>

using namespace lemon;
namespace lemon {
class GatewayWorker : public backends::LlamaCppServer {
public:
    GatewayWorker(ModelManager& models, const std::string& name, ModelType type, int context)
        : LlamaCppServer("error", &models, nullptr) {
        set_model_metadata(name, "fixture/checkpoint", type, DEVICE_CPU, RecipeOptions("llamacpp", {{"ctx_size", context}}));
        set_state(ModelState::READY);
    }
    bool is_backend_alive() const override { return get_state() != ModelState::UNLOADED; }
    void unload() override { set_state(ModelState::UNLOADED); }
    json chat_completion(const json&) override { return run(); }
    json completion(const json&) override { return run(); }
    json embeddings(const json&) override { return run(); }
    json run() {
        note_request_started();
        if (hold) { entered.set_value(); released.get_future().wait(); }
        note_request_completed();
        return {{"choices", json::array({{{"message", {{"content", "fixture"}}}}})}};
    }
    std::promise<void> entered, released;
    std::atomic<bool> hold{false};
};
struct RoutingHelperTestHook {
    static void add(Router& router, const std::shared_ptr<WrappedServer>& worker) {
        std::lock_guard<std::mutex> lock(router.load_mutex_);
        router.loaded_servers_.push_back(worker);
    }
};
}

int main() {
    int checks = 0, failures = 0;
    auto check = [&](bool ok, const char* message) { ++checks; if (!ok) ++failures; std::fprintf(stderr, "%s: %s\n", ok ? "PASS" : "FAIL", message); };
    auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["auto_evict"] = false;
    RuntimeConfig config(defaults); RuntimeConfig::set_global(&config);
    {
        ModelManager models;
        models.register_user_model("user.GatewayChat", {{"recipe", "llamacpp"}, {"checkpoint", "fixture/checkpoint"}, {"labels", json::array({"chat"})}});
        models.register_user_model("user.GatewayEmbedding", {{"recipe", "llamacpp"}, {"checkpoint", "fixture/checkpoint"}, {"labels", json::array({"embeddings"})}});
        auto chat = std::make_shared<GatewayWorker>(models, "user.GatewayChat", ModelType::LLM, 2048);
        auto embedding = std::make_shared<GatewayWorker>(models, "user.GatewayEmbedding", ModelType::EMBEDDING, 8192);
        auto profile = [](const std::shared_ptr<GatewayWorker>& worker) {
            auto result = json{{"checkpoint", worker->get_checkpoint()}, {"options", worker->get_recipe_options().to_json()},
                {"source", "controlled native gateway envelopes"}, {"concurrency", 1}, {"completion_ms", 10000},
                {"load_bytes", {{"host", 256}}}, {"resident_bytes", {{"host", 128}}},
                {"request_limits", {{"input_bytes", 65536}, {"batch", 1}, {"output_tokens", 8}}}, {"request_bytes", {{"host", 128}}}};
            if (worker->get_model_type() != ModelType::LLM) result["request_limits"].erase("output_tokens");
            return result;
        };
        config.set({{"admission", {{"mode", "protected"}, {"interactive_wait_ms", 100},
            {"pools", {{"host", {{"capacity_bytes", 1048576}, {"source", "synthetic known pool"}}}}},
            {"models", {{"GatewayChat", profile(chat)}, {"GatewayEmbedding", profile(embedding)}}}}}});
        Router router(&config, &models, nullptr, [](const std::string&) { return std::optional<uint64_t>(1048576); });
        RoutingHelperTestHook::add(router, chat); RoutingHelperTestHook::add(router, embedding);
        httplib::Server http;
        auto api = std::make_shared<OllamaApi>(&router, &models, OllamaApi::RequestGuard{}, [](const std::string&, const json&) {});
        api->register_routes(http);
        const int port = http.bind_to_any_port("127.0.0.1");
        if (port <= 0) return 1;
        std::thread serving([&] { http.listen_after_bind(); }); http.wait_until_ready();
        httplib::Client client("127.0.0.1", port); client.set_read_timeout(15);
        const json messages = json::array({{{"role", "user"}, {"content", "fixture"}}});
        auto call = [&](const std::string& path, const json& body, int status, const std::string& code, bool retry) {
            const auto response = client.Post(path, body.dump(), "application/json");
            if (!response) { check(false, "gateway returns a bounded HTTP response"); return; }
            const auto result = json::parse(response->body);
            const auto received = path.rfind("/api/", 0) == 0 ? result.value("code", std::string{}) : result.at("error").value("code", std::string{});
            if (response->status != status || received != code) std::fprintf(stderr, "gateway %s received status %d code %s\n", path.c_str(), response->status, received.c_str());
            check(response->status == status && received == code, "loaded-worker native rejection preserves protocol HTTP status and machine-readable code");
            check(!retry || response->get_header_value("Retry-After") == "1", "loaded-worker overload retains applicable retry metadata");
        };
        const std::vector<std::pair<std::string, json>> cases = {
            {"/api/chat", {{"model", "GatewayChat"}, {"messages", messages}, {"stream", false}, {"options", {{"num_predict", 8}}}}},
            {"/api/generate", {{"model", "GatewayChat"}, {"prompt", "fixture"}, {"stream", false}, {"options", {{"num_predict", 8}}}}},
            {"/api/embed", {{"model", "GatewayEmbedding"}, {"input", "fixture"}}},
            {"/api/embeddings", {{"model", "GatewayEmbedding"}, {"prompt", "fixture"}}},
            {"/v1/messages", {{"model", "GatewayChat"}, {"messages", messages}, {"max_tokens", 8}, {"stream", false}}}
        };
        router.pause_admission(true);
        for (const auto& item : cases) call(item.first, item.second, 503, "admission_paused", true);
        router.pause_admission(false);
        for (const auto& item : cases) {
            auto body = item.second;
            if (item.first == "/api/chat" || item.first == "/api/generate") body["options"]["num_predict"] = 16;
            else if (item.first == "/v1/messages") body["max_tokens"] = 16;
            else body[item.first == "/api/embed" ? "input" : "prompt"] = std::string(65536, 'x');
            call(item.first, body, 413, "workload_limit_exceeded", false);
        }
        call("/v1/messages/count_tokens", {{"model", "GatewayChat"}, {"messages", messages}}, 503, "workload_unqualified", false);
        for (const bool use_embedding : {false, true}) {
            auto worker = use_embedding ? embedding : chat;
            worker->hold = true;
            auto held = std::async(std::launch::async, [&] {
                return use_embedding ? router.embeddings({{"model", "GatewayEmbedding"}, {"input", "fixture"}}) :
                    router.chat_completion({{"model", "GatewayChat"}, {"messages", messages}, {"max_tokens", 8}});
            });
            if (worker->entered.get_future().wait_for(std::chrono::seconds(10)) != std::future_status::ready) {
                worker->released.set_value();
                const auto rejected = held.get();
                std::fprintf(stderr, "Held fixture rejected: %s\n", rejected.dump().c_str());
                check(false, "held request reaches its controlled native provider");
                continue;
            }
            for (const auto& item : cases) {
                const bool targets_embedding = item.second.at("model") == "GatewayEmbedding";
                if (targets_embedding == use_embedding) call(item.first, item.second, 429, "queue_deadline_exceeded", true);
            }
            worker->released.set_value();
            check(!held.get().contains("error") && router.admission_snapshot()["active"] == 0,
                  "held generation completes and releases reservations after gateway queue rejection");
        }
        http.stop(); serving.join();
    }
    RuntimeConfig::set_global(nullptr);
    std::fprintf(stderr, "Gateway admission: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
