#include "lemon/backends/llamacpp/llamacpp_server.h"
#include "lemon/config_file.h"
#include "lemon/router.h"
#include "lemon/utils/path_utils.h"

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <mutex>
#include <thread>

namespace lemon {
class BoundsWorker : public backends::LlamaCppServer {
public:
    BoundsWorker(ModelManager& models, int port) : LlamaCppServer("error", &models, nullptr) {
        port_ = port;
        set_model_metadata("provider-bounds", "fixture", ModelType::LLM, DEVICE_CPU,
                           RecipeOptions("llamacpp", {{"ctx_size", 2048}}));
        context_observation_ = {{"slot_ids", json::array({0})}};
        set_state(ModelState::READY);
    }
    bool is_backend_alive() const override { return get_state() != ModelState::UNLOADED; }
    void unload() override { set_state(ModelState::UNLOADED); }
};
struct RoutingHelperTestHook {
    static void add(Router& router, const std::shared_ptr<BoundsWorker>& worker) {
        std::lock_guard<std::mutex> lock(router.load_mutex_);
        router.loaded_servers_.push_back(worker);
    }
};
}

int main() {
    using namespace lemon;
    int checks = 0, failures = 0;
    const auto check = [&](bool passed, const char* message) {
        ++checks;
        if (!passed) { ++failures; std::fprintf(stderr, "FAIL: %s\n", message); }
    };
    auto root = std::filesystem::temp_directory_path() /
        ("juiced-provider-bounds-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    std::filesystem::create_directories(root);
    utils::set_cache_dir(utils::path_to_utf8(root));
    utils::set_config_dir(utils::path_to_utf8(root));
    utils::set_models_dir(utils::path_to_utf8(root / "hf"));
    httplib::Server provider;
    std::mutex mutex;
    std::vector<json> received;
    for (const auto* endpoint : {"/v1/chat/completions", "/v1/completions", "/v1/responses"}) {
        provider.Post(endpoint, [&](const httplib::Request& request, httplib::Response& response) {
            const auto body = json::parse(request.body);
            { std::lock_guard<std::mutex> lock(mutex); received.push_back(body); }
            if (body.value("stream", false))
                response.set_content("data: {\"choices\":[{\"delta\":{\"content\":\"fixture\"}}]}\n\ndata: [DONE]\n\n", "text/event-stream");
            else response.set_content("{\"choices\":[{\"message\":{\"content\":\"fixture\"}}]}", "application/json");
        });
    }
    const int port = provider.bind_to_any_port("127.0.0.1");
    if (port <= 0) return 1;
    std::thread serving([&] { provider.listen_after_bind(); });
    provider.wait_until_ready();
    {
        auto defaults = ConfigFile::base_defaults();
        defaults["auto_evict"] = false;
        defaults["auto_check_model_updates"] = false;
        RuntimeConfig config(defaults); RuntimeConfig::set_global(&config);
        ModelManager models;
        auto worker = std::make_shared<BoundsWorker>(models, port);
        json profile = {{"checkpoint", worker->get_checkpoint()}, {"options", worker->get_recipe_options().to_json()},
            {"source", "controlled provider HTTP schema"}, {"concurrency", 1},
            {"load_bytes", {{"host", 512}}}, {"resident_bytes", {{"host", 256}}}, {"request_bytes", {{"host", 128}}},
            {"request_limits", {{"input_bytes", 65536}, {"batch", 2}, {"output_tokens", 8}}}};
        config.set({{"admission", {{"mode", "protected"},
            {"pools", {{"host", {{"capacity_bytes", 1048576}, {"source", "synthetic known pool"}}}}},
            {"models", {{worker->get_model_name(), profile}}}}}});
        Router router(&config, &models, nullptr, [](const std::string&) { return std::optional<uint64_t>(1048576); });
        RoutingHelperTestHook::add(router, worker);
        const auto count = [&] { std::lock_guard<std::mutex> lock(mutex); return received.size(); };
        for (const auto operation : {InferenceOperation::Chat, InferenceOperation::Completion, InferenceOperation::Responses}) {
            for (const bool stream : {false, true}) {
                json body = {{"model", worker->get_model_name()}, {"stream", stream}, {"n", 2}};
                if (operation == InferenceOperation::Chat) body["messages"] = json::array();
                else if (operation == InferenceOperation::Completion) body["prompt"] = "fixture";
                else body["input"] = "fixture";
                const auto run = [&](const json& request) {
                    if (stream) {
                        std::string output;
                        httplib::DataSink sink;
                        sink.write = [&](const char* data, size_t size) { output.append(data, size); return true; };
                        sink.is_writable = [] { return true; };
                        sink.done = [] {};
                        if (operation == InferenceOperation::Chat) router.chat_completion_stream(request.dump(), sink);
                        else if (operation == InferenceOperation::Completion) router.completion_stream(request.dump(), sink);
                        else router.responses_stream(request.dump(), sink);
                        return output.find("[DONE]") != std::string::npos;
                    }
                    auto result = operation == InferenceOperation::Chat ? router.chat_completion(request) :
                        operation == InferenceOperation::Completion ? router.completion(request) : router.responses(request);
                    return !result.contains("error");
                };
                const std::vector<std::string> aliases = operation == InferenceOperation::Responses ?
                    std::vector<std::string>{"max_output_tokens"} : std::vector<std::string>{"max_tokens", "max_completion_tokens", "n_predict"};
                for (const auto& alias : aliases) {
                    auto valid = body; valid[alias] = 8;
                    const auto before = count();
                    check(run(valid) && count() == before + 1, "qualified request reaches its actual provider exactly once");
                    std::lock_guard<std::mutex> lock(mutex);
                    if (received.size() != before + 1) continue;
                    const auto& actual = received.back();
                    const auto ceiling = operation == InferenceOperation::Responses ? "max_output_tokens" : "max_tokens";
                    check(actual.at(ceiling) == 8 && actual.at("n_cmpl") == 2 && actual.at("n_probs") == 0 &&
                          !actual.contains("n") && !actual.contains("n_predict") && !actual.contains("max_completion_tokens"),
                          "provider receives the same canonical token, batch and probability bounds that admission used");
                }
                body[operation == InferenceOperation::Responses ? "max_output_tokens" : "max_tokens"] = 8;
                for (const auto& overrides : std::vector<json>{
                        {{"extra_params", {{"n_predict", -1}}}}, {{"num_predict", 8}}, {{"n_probs", 1}},
                        {{"n_cmpl", 3}}, {{"best_of", 2}}, {{"n_predict", -1}}, {{"max_completion_tokens", 9}},
                        operation == InferenceOperation::Responses ? json{{"max_tokens", 8}} : json{{"max_output_tokens", 8}}}) {
                    auto invalid = body; invalid.update(overrides);
                    const auto before = count();
                    check(!run(invalid) && count() == before && router.admission_snapshot()["active"] == 0,
                          "unsupported aliases, nested overrides and oversized native controls reject before HTTP dispatch");
                }
            }
        }
    }
    RuntimeConfig::set_global(nullptr);
    provider.stop(); serving.join();
    std::filesystem::remove_all(root);
    std::printf("Provider request bounds: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
