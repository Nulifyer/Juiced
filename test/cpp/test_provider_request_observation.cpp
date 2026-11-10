#include "lemon/backends/asr4all/asr4all_server.h"
#include "lemon/backends/whispercpp/whispercpp_server.h"
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

using namespace std::chrono_literals;
namespace lemon {
class SpeechProtocolFixture {
public:
    SpeechProtocolFixture() {
        http.Post("/inference", [&](const httplib::Request& request, httplib::Response& response) {
            ++calls;
            active = true;
            while (hold) std::this_thread::sleep_for(2ms);
            if (reject) {
                response.status = 400;
                response.set_content("{\"error\":\"fixture rejection\"}", "application/json");
                active = false;
                return;
            }
            auto format = request.form.has_field("response_format") ? request.form.get_field("response_format") : "json";
            if (format == "text") response.set_content("fixture speech", "text/plain");
            else response.set_content("{\"text\":\"fixture speech\"}", "application/json");
            active = false;
        });
        port = http.bind_to_any_port("127.0.0.1");
        if (port < 0) throw std::runtime_error("Fixture bind failed");
        listener = std::thread([&] { http.listen_after_bind(); });
        for (int i = 0; i < 500 && !http.is_running(); ++i) std::this_thread::sleep_for(1ms);
        if (!http.is_running()) throw std::runtime_error("Fixture listener failed");
    }
    ~SpeechProtocolFixture() { stop(); }
    void stop() {
        hold = false;
        http.stop();
        if (listener.joinable()) listener.join();
    }
    int port;
    std::atomic<int> calls{0};
    std::atomic<bool> hold{false}, active{false}, reject{false};
private:
    httplib::Server http;
    std::thread listener;
};
template<class Backend> class ObservedSpeechWorker : public Backend {
public:
    ObservedSpeechWorker(ModelManager& models, SpeechProtocolFixture& engine, const std::string& recipe)
        : Backend("error", &models, nullptr), engine_(engine) {
        this->port_ = engine.port;
        this->set_model_metadata("speech-fixture", "fixture-only", ModelType::TRANSCRIPTION, DEVICE_CPU,
                                 RecipeOptions(recipe, json::object()));
        this->set_state(ModelState::READY);
    }
    bool is_backend_alive() const override { return this->get_state() != ModelState::UNLOADED; }
    void unload() override { engine_.stop(); this->set_state(ModelState::UNLOADED); }
private:
    SpeechProtocolFixture& engine_;
};
struct RoutingHelperTestHook {
    static void add(Router& router, const std::shared_ptr<WrappedServer>& worker) {
        std::lock_guard<std::mutex> lock(router.load_mutex_);
        router.loaded_servers_.push_back(worker);
    }
};
}

int main() {
    using namespace lemon;
    int checks = 0, failures = 0;
    auto check = [&](bool ok, const char* label) {
        ++checks;
        if (!ok) { ++failures; std::fprintf(stderr, "FAIL: %s\n", label); }
    };
    auto until = [](auto predicate) {
        for (int i = 0; i < 1000; ++i) { if (predicate()) return true; std::this_thread::sleep_for(2ms); }
        return false;
    };
    auto root = std::filesystem::temp_directory_path() /
        ("juiced-provider-observation-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    std::filesystem::create_directories(root);
    utils::set_cache_dir(utils::path_to_utf8(root)); utils::set_config_dir(utils::path_to_utf8(root));
    utils::set_models_dir(utils::path_to_utf8(root / "hf"));
    {
        auto defaults = ConfigFile::base_defaults();
        defaults["auto_evict"] = false; defaults["disable_model_filtering"] = true; defaults["auto_check_model_updates"] = false;
        for (const auto& recipe : {"asr4all", "whispercpp"}) {
            RuntimeConfig config(defaults); RuntimeConfig::set_global(&config); ModelManager models;
            SpeechProtocolFixture engine;
            std::shared_ptr<WrappedServer> worker;
            if (std::string(recipe) == "asr4all")
                worker = std::make_shared<ObservedSpeechWorker<backends::Asr4allServer>>(models, engine, recipe);
            else worker = std::make_shared<ObservedSpeechWorker<backends::WhisperServer>>(models, engine, recipe);
            auto profile = json{{"checkpoint", worker->get_checkpoint()}, {"options", worker->get_recipe_options().to_json()},
                {"source", "mock speech transport, no model or accelerator"}, {"concurrency", 1}, {"completion_ms", 10000},
                {"load_bytes", {{"host", 1024}}}, {"resident_bytes", {{"host", 512}}}, {"request_bytes", {{"host", 256}}}};
            auto admission = json{{"mode", "legacy"}, {"interactive_wait_ms", 100},
                {"pools", {{"host", {{"capacity_bytes", 1048576}, {"source", "single physical pool"}}}}},
                {"models", {{worker->get_model_name(), profile}}}};
            config.set({{"admission", admission}});
            Router router(&config, &models, nullptr, [](const std::string& pool) -> std::optional<uint64_t> {
            return pool == "host" ? std::optional<uint64_t>(16 * 1024 * 1024) : std::nullopt;
        }); RoutingHelperTestHook::add(router, worker);
            json request = {{"model", worker->get_model_name()}, {"file_data", "neutral fixture bytes"}};
            auto response = router.audio_transcriptions(request);
            check(response.value("text", "") == "fixture speech" && router.admission_snapshot()["active"] == 0,
                  "JSON speech response reports actual completed transport");
            request["response_format"] = "text";
            response = router.audio_transcriptions(request);
            check(response.value("text", "") == "fixture speech" && router.admission_snapshot()["uncertain"] == 0,
                  "text speech response releases its reservation");
            engine.reject = true;
            response = router.audio_transcriptions(request);
            check(response.contains("error") && router.admission_snapshot()["uncertain"] == 0,
                  "complete provider error response settles its managed exchange");
            engine.reject = false;
            const int dispatched = engine.calls;
            auto invalid = request; invalid["file_data"] = "";
            try { router.audio_transcriptions(invalid); } catch (const std::exception&) {}
            check(engine.calls == dispatched && router.admission_snapshot()["active"] == 0,
                  "local speech validation cannot quarantine work that never started");
            response = router.chat_completion({{"model", worker->get_model_name()}, {"messages", json::array()}});
            check(response.contains("error") && engine.calls == dispatched && router.admission_snapshot()["active"] == 0,
                  "wrong service capability rejects without consuming the worker forever");
            engine.hold = true;
            std::atomic<bool> disconnected{false};
            auto pending = std::async(std::launch::async, [&] {
                NativeRequestContext context; context.identity.principal = "fixture";
                context.cancelled = [&] { return disconnected.load(); };
                RequestContextScope scope(context);
                try { return router.audio_transcriptions(request).contains("error"); }
                catch (const std::exception&) { return true; }
            });
            check(until([&] { return engine.active.load(); }), "actual speech HTTP request reaches the upstream handler");
            disconnected = true;
            const bool prompt_abort = pending.wait_for(5s) == std::future_status::ready;
            if (!prompt_abort) engine.hold = false;
            const bool cancelled = pending.get();
            check(prompt_abort && cancelled, "speech cancellation aborts the upstream transfer promptly");
            check(engine.active && router.admission_snapshot()["uncertain"] == 1,
                  "cancelled HTTP transport does not pretend upstream computation stopped");
            response = router.audio_transcriptions(request);
            check(response.contains("error") && engine.calls == dispatched + 1,
                  "unsettled speech work keeps its generation's compute slot reserved");
            router.unload_model(worker->get_model_name());
            check(!engine.active && router.admission_snapshot()["uncertain"] == 0,
                  "confirmed provider shutdown reconciles the exact speech generation");
        }
    }
    RuntimeConfig::set_global(nullptr);
    std::filesystem::remove_all(root);
    std::printf("Speech protocol lifetime: %d checks, %d failures; mock transport only\n", checks, failures);
    return failures ? 1 : 0;
}
