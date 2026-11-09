#include "lemon/backend_manager.h"
#include "lemon/backends/backend_ops.h"
#include "lemon/backends/llamacpp/llamacpp_server.h"
#include "lemon/config_file.h"
#include "lemon/juiced/service.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "lemon/utils/path_utils.h"
#include "support/test_state_isolation.h"

#include <algorithm>
#include <atomic>
#include <cstdlib>
#include <cstdio>
#include <fstream>
#include <future>
#include <system_error>
#include <thread>

using namespace lemon;
namespace fs = std::filesystem;
static int checks = 0, failures = 0;
static void check(bool ok, const char* label) {
    ++checks; std::fprintf(stderr, "%s: %s\n", ok ? "PASS" : "FAIL", label); if (!ok) ++failures;
}
template<class Action> static bool rejected(Action action, const char* code = "operation_unqualified") {
    try { action(); return false; }
    catch (const AdmissionError& error) { return error.status == 503 && error.code == code; }
}
class ManagementWorker : public WrappedServer {
public:
    ManagementWorker() : WrappedServer("fixture", "error") {
        set_model_metadata("Management-Fixture", "fixture", ModelType::LLM, DEVICE_CPU,
                           RecipeOptions("llamacpp", {{"llamacpp_backend", "cpu"}}));
        set_state(ModelState::READY);
    }
    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override {}
    void unload() override { set_state(ModelState::UNLOADED); }
    bool is_backend_alive() const override { return get_state() != ModelState::UNLOADED; }
    json chat_completion(const json&) override { return json::object(); }
};
struct PreparationProcessControl {
    std::atomic<bool> started{false}, running{false}, fail_stop{true};
    std::atomic<int> stops{0};
};
class ControlledPreparationProcess : public ServerProcess {
public:
    explicit ControlledPreparationProcess(std::shared_ptr<PreparationProcessControl> control)
        : ServerProcess(ProcessOutput{}), control_(std::move(control)) {}
    std::string start(const ServerCommand& command) override {
        json job; std::ifstream input(command.args.at(1)); input >> job;
        const auto staging = fs::path(job.at("cache").get<std::string>()) / (".partial-" + job.at("id").get<std::string>());
        fs::create_directories(staging); std::ofstream(staging / "owned") << "writer dependency";
        control_->running = true; control_->started = true;
        return "127.0.0.1";
    }
    void stop() override {
        if (!control_->running) return;
        ++control_->stops;
        if (control_->fail_stop) throw std::runtime_error("injected unconfirmed preparation stop");
        control_->running = false;
    }
    bool running() const override { return control_->running; }
    bool exited() const override { return !control_->running; }
private:
    std::shared_ptr<PreparationProcessControl> control_;
};
struct VoiceProcessControl {
    bool running = false, fail_stop = false, fail_start = false, overlapped = false;
    int starts = 0, stops = 0;
    ServerCommand command;
};
class ControlledVoiceProcess : public ServerProcess {
public:
    explicit ControlledVoiceProcess(std::shared_ptr<VoiceProcessControl> control)
        : ServerProcess(ProcessOutput{}), control_(std::move(control)) {}
    std::string start(const ServerCommand& command) override {
        control_->overlapped |= control_->running;
        control_->running = true; ++control_->starts; control_->command = command;
        if (control_->fail_start) throw std::runtime_error("injected post-dispatch voice start failure");
        return "127.0.0.1";
    }
    void stop() override {
        if (!control_->running) return;
        ++control_->stops;
        if (control_->fail_stop) throw std::runtime_error("injected unconfirmed voice stop");
        control_->running = false;
    }
    bool running() const override { return control_->running; }
    bool exited() const override { return !control_->running; }
private:
    std::shared_ptr<VoiceProcessControl> control_;
};
class VoiceEnvironment {
public:
    VoiceEnvironment(const char* key, const char* value) : key_(key) {
        if (const char* previous = std::getenv(key)) { previous_ = previous; present_ = true; }
        set(value);
    }
    ~VoiceEnvironment() { set(present_ ? previous_.c_str() : nullptr); }
    void set(const char* value) {
#ifdef _WIN32
        _putenv_s(key_.c_str(), value ? value : "");
#else
        if (value) setenv(key_.c_str(), value, 1); else unsetenv(key_.c_str());
#endif
    }
private:
    std::string key_, previous_; bool present_ = false;
};
namespace lemon {
struct ModelPublicationTestHook {
    static size_t history(ModelManager& models) {
        std::lock_guard<std::mutex> lock(models.sync_state_.mutex);
        return models.sync_state_.completed_generation_results.size();
    }
};
struct StatePublicationTestHook {
    static void voice_factory(Server& server, const std::shared_ptr<VoiceProcessControl>& control) {
        server.juiced_->voice_process_factory_ = [control] { return std::make_unique<ControlledVoiceProcess>(control); };
    }
    static void voice_models(Server& server, const std::string& model) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        server.juiced_->catalog_ = json::array({{{"id", model}, {"downloaded", true}, {"service", "Transcribe"}}});
        server.juiced_->state_["policies"][model] = "on-demand";
        server.juiced_->persist_locked();
    }
    static void voice(Server& server) { server.juiced_->voice_retry_after_ = 0; server.juiced_->reconcile_voice(); }
    static json voice_status(Server& server) { std::lock_guard<std::mutex> lock(server.juiced_->mutex_); return server.juiced_->voice_status_; }
    static json voice_intent(Server& server) { std::lock_guard<std::mutex> lock(server.juiced_->mutex_); return server.juiced_->state_.value("voice_writer", json::object()); }
    static void start_barrier(Server& server, std::function<void(const std::string&)> hook) {
        server.background_start_barrier_ = std::move(hook);
    }
    static void download(Server& server, std::function<void(DownloadProgressCallback)> operation) {
        auto admission = std::make_shared<ResourceAdmission::Lease>(server.router_->admit_management("start-fixture", "download-entry"));
        auto job = server.start_download_job("start-fixture", "model", "start-fixture",
            [admission, operation = std::move(operation)](auto callback) { operation(std::move(callback)); });
        server.join_download_job(job);
    }
    static json download_status(Server& server) {
        std::lock_guard<std::mutex> lock(server.downloads_mutex_);
        const auto job = server.download_jobs_.at("start-fixture");
        return {{"running", job->running}, {"status", job->status}, {"error", job->error}};
    }
    static httplib::Response invoke(Server& server, const std::string& path, const json& body) {
        httplib::Request request; request.method = "POST"; request.path = path; request.body = body.dump();
        httplib::Response response; response.status = 200;
        auto handler = server.contextual_handler([&](const auto& req, auto& res) {
            if (path.find("uninstall") != std::string::npos) server.handle_uninstall(req, res);
            else if (path.find("install") != std::string::npos) server.handle_install(req, res);
            else if (path.find("models/sync") != std::string::npos) server.handle_models_sync(req, res);
            else if (path.find("preparation") != std::string::npos) server.juiced_->preparation_api(req, res);
            else server.handle_pull(req, res);
        });
        handler(request, response); return response;
    }
    static void pause(Server& server, bool paused) { server.router_->pause_admission(paused); }
    static void configure(Server& server, const json& config) { server.router_->configure_admission(config); }
    static json admission(Server& server) { return server.router_->admission_snapshot(); }
    static ModelManager& models(Server& server) { return *server.model_manager_; }
    static void add(Server& server, const std::shared_ptr<WrappedServer>& worker) {
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_);
        server.router_->loaded_servers_.push_back(worker);
    }
    static void retain_writer(Server& server, const std::string& model, const fs::path& source,
                              const std::shared_ptr<PreparationProcessControl>& control) {
        auto accepted = server.router_->admit_management(model, "legacy-writer");
        auto operation = server.router_->admit_management(model, "legacy-cleanup");
        json job = {{"id", "writer-fixture"}, {"model", model}, {"name", "Prepared-Fixture"}, {"source", source.string()},
            {"pending", true}, {"phase", "cleanup-pending"}, {"writer_state", "dispatching"}};
        const auto dir = server.juiced_->directory_ / "preparations/writer-fixture";
        fs::create_directories(dir);
        auto command = job; command["cache"] = (server.juiced_->directory_ / "prepared").string();
        std::ofstream(dir / "job.json") << command;
        auto process = std::make_unique<ControlledPreparationProcess>(control);
        {
            std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
            server.juiced_->state_["preparations"]["writer-fixture"] = job; server.juiced_->persist_locked();
        }
        accepted.started(); operation.started();
        process->start({"controlled-old-writer", {"ignored", (dir / "job.json").string()}, {}, 0, ""});
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        auto task = std::make_unique<JuicedService::PreparationTask>();
        task->job = job; task->admission = std::move(accepted); task->operation = std::move(operation);
        task->process = std::move(process);
        task->outcome = {{"pending", false}, {"phase", "interrupted"}, {"error", "Model conversion is retired"}};
        server.juiced_->preparation_active_ = std::move(task); server.juiced_->wake_.notify_all();
    }
    static json writer(Server& server) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        return server.juiced_->state_["preparations"].at("writer-fixture");
    }
    static void stop_preparation(Server& server) { server.juiced_->stop(); }
    static json removal(Server& server, const std::string& name) { return server.juiced_->removal_plan(name); }
};
}

static void native_owners() {
    const auto root = test::make_scratch_dir("management-native");
    const auto engine = root / "installed-engine"; std::ofstream(engine) << "fixture, never executed";
    fs::permissions(engine, fs::perms::owner_exec | fs::perms::owner_read | fs::perms::owner_write);
    const auto runtime_dir = fs::path(backends::BackendUtils::get_install_directory("llamacpp", "cpu"));
    check(!fs::exists(fs::path(utils::get_cache_dir()) / "bin"),
          "runtime path lookup does not create its binary cache before admission");
    fs::create_directories(runtime_dir);
    const auto retained_runtime = runtime_dir / "retained-component"; std::ofstream(retained_runtime) << "preserve";
    auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false; defaults["auto_check_model_updates"] = false;
    defaults["llamacpp"]["cpu_bin"] = engine.string(); defaults["admission"] = {{"mode", "legacy"}, {"active_limit", 1}};
    RuntimeConfig config(defaults); RuntimeConfig::set_global(&config);
    ModelManager models; BackendManager runtime_manager;
    {
        Router router(&config, &models, &runtime_manager);
        std::promise<void> entered, release; auto released = release.get_future().share();
        std::atomic<int> dispatches{0}; std::atomic<bool> nested{false};
        models.set_download_model_override_for_test([&](const auto&, const auto&, bool, auto) {
            ++dispatches;
            runtime_manager.install_backend("llamacpp", "cpu", false, [&](const auto&) { nested = router.admission_snapshot()["active"] == 2; return true; });
            entered.set_value(); released.wait();
        });
        auto task = std::async(std::launch::async, [&] { models.download_model("asset", json::object(), true); });
        const bool active = entered.get_future().wait_for(std::chrono::seconds(5)) == std::future_status::ready;
        check(active && nested && router.admission_snapshot()["active"] == 1,
              "nested synchronous acquisition/install lifetimes do not deadlock a one-slot legacy policy");
        bool conflict = false;
        try { router.configure_admission({{"mode", "protected"}}); }
        catch (const AdmissionError& error) { conflict = error.status == 409 && error.code == "admission_configuration_busy"; }
        check(conflict, "protected mode cannot publish while native acquisition remains active");
        release.set_value(); task.get();
        models.set_download_model_override_for_test([&](const auto&, const auto&, bool, auto) { ++dispatches; });
        router.pause_admission(true);
        check(rejected([&] { models.download_model("asset", json::object(), true); }, "admission_paused") && dispatches == 1,
              "paused native acquisition rejects before the download owner executes");
        router.pause_admission(false); router.configure_admission({{"mode", "protected"}});
        ModelInfo info; info.model_name = "asset"; info.recipe = "llamacpp"; info.checkpoints["main"] = "fixture/repo:Q4_K_M";
        check(rejected([&] { models.download_model("asset", json::object(), true); }) && dispatches == 1,
              "protected download rejects before any overridden/native download execution");
        check(rejected([&] { models.download_registered_model(info, true); }), "automatic registered-weight acquisition is contained");
        check(rejected([&] { models.register_model("user.Management-New", {{"recipe", "llamacpp"}, {"checkpoint", "fixture/repo:Q4_K_M"}}); }),
              "remote registration rejects before metadata lookup or registry publication");
        check(rejected([&] { models.enqueue_sync(); }), "protected sync rejects before publishing a running job");
        check(rejected([&] { models.check_for_model_updates(); }), "background update checks reject before remote metadata acquisition");
        router.configure_admission({{"mode", "legacy"}});
        const auto queued_sync = models.enqueue_sync({"asset"});
        router.configure_admission({{"mode", "protected"}});
        const auto sync_result = models.execute_sync();
        check(sync_result.value("status", "") == "failed" && models.get_sync_status(queued_sync.sync_id).value("status", "") == "failed",
              "unadmitted queued sync reaches a terminal failure after an admission policy change");
        bool ready = false;
        runtime_manager.install_backend("llamacpp", "cpu", false, [&](const auto&) { ready = true; return true; });
        check(ready && fs::is_regular_file(engine), "protected loading reuses its installed runtime without an update");
        check(rejected([&] { runtime_manager.install_backend("llamacpp", "cpu", true); }) && fs::is_regular_file(engine),
              "protected forced installation preserves the existing runtime");
        const auto missing = fs::path(backends::BackendUtils::get_install_directory("llamacpp", "vulkan"));
        check(!fs::exists(missing) && rejected([&] { runtime_manager.install_backend("llamacpp", "vulkan"); }) && !fs::exists(missing),
              "an unavailable protected runtime rejects before package directory creation or download");
        check(rejected([&] { runtime_manager.uninstall_backend("llamacpp", "cpu"); }) && fs::is_regular_file(retained_runtime),
              "protected runtime removal rejects before filesystem deletion");
        info.extras["hf_load"] = true;
        backends::LlamaCppServer llama("error", &models, &runtime_manager);
        check(rejected([&] { llama.load("asset", info, RecipeOptions("llamacpp", {{"llamacpp_backend", "cpu"}, {"ctx_size", 128}}), true); }),
              "llama.cpp native HF loading cannot acquire weights behind protected admission");
        info.extras["hf_load"] = false;
        for (const std::string controls : {"--hf-repo fixture/repo", "--model-url https://127.0.0.1:1/model.gguf", "--models-dir fixture", "--parallel 4"}) {
            check(rejected([&] { llama.load("asset", info, RecipeOptions("llamacpp", {{"llamacpp_backend", "cpu"}, {"ctx_size", 128}, {"llamacpp_args", controls}}), true); }),
                  "opaque provider load controls cannot hide unqualified acquisition or resource changes");
        }
        backends::BackendOpsContext context; context.model_manager = &models;
        check(rejected([&] { backends::ops_for("llamacpp")->download_model(info, true, {}, context); }),
              "the shared registry engine rejects direct native provider acquisition");
        check(rejected([&] { backends::ops_for("flm")->discover_models(context); }),
              "protected native FLM discovery rejects before hardware/CLI probing");
        info.recipe = "flm";
        const auto denied = router.admission_snapshot().at("rejected_total").get<uint64_t>();
        check(!backends::ops_for("flm")->is_downloaded(info, context) &&
              router.admission_snapshot().at("rejected_total").get<uint64_t>() == denied + 1,
              "protected installed-model lookup conservatively reports unavailable after admission rejection");
        check(rejected([&] { backends::ops_for("flm")->download_model(info, true, {}, context); }),
              "self-managed FLM acquisition rejects before starting its CLI");
    }
    check(rejected([&] { models.download_model("asset", json::object(), true); }, "admission_paused"),
          "native owner views retain Router shutdown after the Router lifetime ends");
    RuntimeConfig::set_global(nullptr);
}

static void entry_boundaries() {
    const auto root = test::make_scratch_dir("management-entry");
    auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false; defaults["auto_check_model_updates"] = false;
    defaults["disable_model_filtering"] = true; defaults["admission"] = {{"mode", "protected"}};
    auto config = std::make_shared<RuntimeConfig>(defaults); RuntimeConfig::set_global(config.get());
    {
        Server server(config, root.string(), root.string());
        auto worker = std::make_shared<ManagementWorker>(); StatePublicationTestHook::add(server, worker);
        for (bool paused : {false, true}) {
            StatePublicationTestHook::pause(server, paused);
            const auto code = paused ? "admission_paused" : "operation_unqualified";
            for (const std::string prefix : {"/v1/", "/v0/", "/api/v1/", "/api/v0/"}) {
                for (const std::string operation : {"pull", "install", "uninstall", "juiced/preparation"}) {
                    for (int mode = 0; mode < (operation == "pull" || operation == "install" ? 3 : 1); ++mode) {
                        json body = {{"model", "Management-Fixture"}, {"recipe", "llamacpp"}, {"backend", "cpu"},
                                     {"stream", mode != 0}, {"subscribe", mode != 2}, {"quant", "Q4_K_M"}, {"acknowledge", true}};
                        const auto response = StatePublicationTestHook::invoke(server, prefix + operation, body);
                        const auto error = json::parse(response.body).at("error");
                        check((operation == "juiced/preparation" ? response.status == 410 && error.at("code") == "feature_retired" :
                              response.status == 503 && error.at("code") == code && response.get_header_value("Retry-After") == "1") && !response.content_provider_,
                              "native JSON/SSE/async entry rejects with typed metadata before dispatch");
                    }
                }
            }
            check(worker->get_state() == ModelState::READY && !fs::exists(root / "juiced/preparations"),
                  "rejected removal/preparation leaves its worker and scratch storage untouched");
        }
        config->set({{"offline", false}});
        for (bool paused : {false, true}) {
            StatePublicationTestHook::pause(server, paused);
            for (const std::string prefix : {"/v1/", "/v0/", "/api/v1/", "/api/v0/"}) {
                for (int mode = 0; mode < 3; ++mode) {
                    const auto response = StatePublicationTestHook::invoke(server, prefix + "models/sync", {{"dry_run", mode == 0}, {"async", mode == 2}});
                    check(response.status == 503 && json::parse(response.body).at("error").at("code") == (paused ? "admission_paused" : "operation_unqualified"),
                          "model sync/update preview rejects before staging JSON or async work across every prefix");
                }
            }
        }
        StatePublicationTestHook::pause(server, false);
        StatePublicationTestHook::configure(server, {{"mode", "legacy"}});
        config->set({{"offline", false}, {"no_fetch_executables", true}});
        const auto file = root / "fixture.gguf";
        { char header[24] = {'G', 'G', 'U', 'F', 3}; std::ofstream out(file, std::ios::binary); out.write(header, sizeof(header)); }
        auto& models = StatePublicationTestHook::models(server);
        models.register_user_model("Management-Fixture", {{"recipe", "llamacpp"}, {"source", "local_path"},
            {"checkpoints", {{"main", file.string()}}}, {"labels", json::array({"chat"})}}, "local_path");
        std::atomic<int> dispatched{0};
        models.set_download_model_override_for_test([&](const auto&, const auto&, bool, auto) { ++dispatched; });
        {
            auto response = StatePublicationTestHook::invoke(server, "/v1/pull", {{"model", "Management-Fixture"}, {"stream", true}, {"do_not_upgrade", true}});
            bool conflict = false;
            try { StatePublicationTestHook::configure(server, {{"mode", "protected"}}); }
            catch (const AdmissionError& error) { conflict = error.code == "admission_configuration_busy"; }
            check(response.status == 200 && response.content_provider_ && dispatched == 0 && conflict,
                  "an accepted deferred stream retains admission before its download provider starts");
        }
        const auto idle_until = std::chrono::steady_clock::now() + std::chrono::seconds(5);
        while (StatePublicationTestHook::admission(server)["active"] != 0 && std::chrono::steady_clock::now() < idle_until)
            std::this_thread::sleep_for(std::chrono::milliseconds(10));
        check(StatePublicationTestHook::admission(server)["active"] == 0,
              "discarding a proven undispatched stream releases its transition ticket after other native work drains");
        config->set({{"offline", true}});
        StatePublicationTestHook::configure(server, {{"mode", "legacy"}});
        for (const std::string action : {"", "start", "plan", "resume", "convert", "quantize"}) {
            for (const std::string prefix : {"/v1/", "/v0/", "/api/v1/", "/api/v0/"}) {
                const auto response = StatePublicationTestHook::invoke(server, prefix + "juiced/preparation",
                    {{"model", "Management-Fixture"}, {"action", action}, {"quant", "Q4_K_M"}, {"acknowledge", true}});
                check(response.status == 410 && json::parse(response.body)["error"]["code"] == "feature_retired" &&
                      dispatched == 0 && !fs::exists(root / "juiced/preparations"),
                      "retired producer actions never dispatch or create scratch, including legacy mode");
            }
        }

    }
    RuntimeConfig::set_global(nullptr);
}

static void worker_start_boundaries() {
    const auto root = test::make_scratch_dir("worker-start");
    auto defaults = ConfigFile::base_defaults();
    defaults["offline"] = true; defaults["broadcast"] = false; defaults["auto_check_model_updates"] = false;
    defaults["disable_model_filtering"] = true;
    auto config = std::make_shared<RuntimeConfig>(defaults); RuntimeConfig::set_global(config.get());
    {
        Server server(config, root.string(), root.string());
        auto& models = StatePublicationTestHook::models(server);
        std::atomic<int> downloads{0}, syncs{0}, attempted_starts{0};
        models.set_update_check_override_for_test([&](const auto&) { ++syncs; return ModelManager::UpdateCheckResult{}; });
        config->set({{"offline", false}});
        StatePublicationTestHook::start_barrier(server, [&](const std::string&) {
            ++attempted_starts;
            throw std::system_error(std::make_error_code(std::errc::resource_unavailable_try_again), "injected worker start failure");
        });
        const auto response = StatePublicationTestHook::invoke(server, "/v1/models/sync", {{"async", true}});
        check(response.status == 500 && json::parse(response.body)["error"]["code"] == "worker_start_failed",
              "failed sync dispatch returns a machine-readable start failure");
        const auto failed = models.get_sync_status();
        check(failed["status"] == "failed" && !failed["already_in_progress"].get<bool>() &&
              failed["completed_sync_id"] == failed["sync_id"] && failed["pending_targets"].empty() && syncs == 0,
              "failed worker construction finalizes its sync generation without executing it");
        check(StatePublicationTestHook::admission(server)["active"] == 0,
              "failed sync start releases its accepted management ownership");
        bool rejected_start = false;
        try { StatePublicationTestHook::download(server, [&](auto) { ++downloads; }); }
        catch (const std::system_error&) { rejected_start = true; }
        const auto failed_download = StatePublicationTestHook::download_status(server);
        check(rejected_start && failed_download["running"] == false && failed_download["status"] == "error" && downloads == 0,
              "failed download construction leaves a terminal retryable row without execution");
        check(StatePublicationTestHook::admission(server)["active"] == 0 && attempted_starts == 2,
              "failed download start releases admission and preserves the live manager");
        StatePublicationTestHook::start_barrier(server, {});
        StatePublicationTestHook::download(server, [&](auto) { ++downloads; });
        check(StatePublicationTestHook::download_status(server)["status"] == "completed" && downloads == 1,
              "the same download identifier can retry through a real owned worker");
        const auto retried = StatePublicationTestHook::invoke(server, "/v1/models/sync", {{"async", true}});
        check(retried.status == 202, "sync accepts a fresh generation after failed construction");
        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
        while ((models.get_sync_status()["already_in_progress"].get<bool>() || StatePublicationTestHook::admission(server)["active"] != 0) &&
               std::chrono::steady_clock::now() < deadline) std::this_thread::sleep_for(std::chrono::milliseconds(10));
        check(!models.get_sync_status()["already_in_progress"].get<bool>() && syncs > 0 && StatePublicationTestHook::admission(server)["active"] == 0,
              "successful sync retry completes and releases its accepted ownership");
        check(models.get_sync_status(failed["sync_id"].get<uint64_t>())["status"] == "failed",
              "a later successful generation retains the original failed outcome");
        const auto queued = models.enqueue_sync();
        models.fail_sync_start(failed["sync_id"].get<uint64_t>(), "obsolete failure");
        check(models.get_sync_status()["already_in_progress"] == true && models.get_sync_status()["sync_id"] == queued.sync_id,
              "an obsolete start failure cannot terminate a newer queued generation");
        models.fail_sync_start(queued.sync_id, "controlled undispatched cleanup");
        check(models.get_sync_status(queued.sync_id)["status"] == "failed",
              "undispatched queue cleanup retains a terminal generation outcome");
        StatePublicationTestHook::start_barrier(server, [](const std::string&) {
            throw std::system_error(std::make_error_code(std::errc::resource_unavailable_try_again), "bounded failed dispatch");
        });
        std::vector<uint64_t> generations;
        bool failed_starts = true;
        for (int i = 0; i < 40; ++i) {
            const auto response = StatePublicationTestHook::invoke(server, "/v1/models/sync", {{"async", true}});
            failed_starts = failed_starts && response.status == 500 &&
                json::parse(response.body)["error"]["code"] == "worker_start_failed";
            generations.push_back(models.get_sync_status()["sync_id"].get<uint64_t>());
        }
        check(failed_starts && ModelPublicationTestHook::history(models) == 32,
              "forty actual failed dispatches retain at most32 completed generation results");
        bool exact_results = true;
        for (size_t i = 0; i < generations.size(); ++i) {
            const auto outcome = models.get_sync_status(generations[i]);
            exact_results = exact_results && outcome["sync_id"] == generations[i] &&
                outcome["status"] == (i < 8 ? "not_found" : "failed");
        }
        check(exact_results, "bounded history evicts oldest outcomes while retaining generation-specific failures");
        StatePublicationTestHook::start_barrier(server, {});
        const auto successful = StatePublicationTestHook::invoke(server, "/v1/models/sync", {{"async", true}});
        const auto retry_deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
        while ((models.get_sync_status()["already_in_progress"].get<bool>() || StatePublicationTestHook::admission(server)["active"] != 0) &&
               std::chrono::steady_clock::now() < retry_deadline) std::this_thread::sleep_for(std::chrono::milliseconds(10));
        check(successful.status == 202 && !models.get_sync_status()["already_in_progress"].get<bool>() &&
              models.get_sync_status()["terminal_error"] == "" && ModelPublicationTestHook::history(models) == 32,
              "a real successful retry uses the same bounded terminal publication");
        models.set_update_check_override_for_test([](const auto&) -> ModelManager::UpdateCheckResult { throw std::runtime_error("controlled sync failure"); });
        const auto terminal = models.enqueue_sync();
        const auto terminal_result = models.execute_sync();
        check(terminal_result["status"] == "failed" && terminal_result["sync_id"] == terminal.sync_id &&
              terminal_result["terminal_error"] == "controlled sync failure" && ModelPublicationTestHook::history(models) == 32,
              "ordinary terminal exceptions publish through the bounded result owner");
        models.set_update_check_override_for_test([](const auto&) -> ModelManager::UpdateCheckResult { throw 7; });
        const auto unwinding = models.enqueue_sync();
        bool escaped = false;
        try { models.execute_sync(); } catch (int) { escaped = true; }
        const auto interrupted = models.get_sync_status(unwinding.sync_id);
        check(escaped && interrupted["status"] == "failed" &&
              interrupted["terminal_error"] == "Sync worker terminated unexpectedly" && ModelPublicationTestHook::history(models) == 32,
              "exception unwinding retains the correct bounded generation outcome");
    }
    RuntimeConfig::set_global(nullptr);
}

static void preparation_cleanup() {
    using Hook = StatePublicationTestHook;
    for (int boundary : {0, 1, 2}) {
        const auto root = test::make_scratch_dir("preparation-cleanup");
        auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        defaults["auto_evict"] = false; defaults["auto_check_model_updates"] = false;
        auto config = std::make_shared<RuntimeConfig>(defaults); RuntimeConfig::set_global(config.get());
        const auto source = root / "source.gguf";
        { char header[24] = {'G', 'G', 'U', 'F', 3}; std::ofstream file(source, std::ios::binary); file.write(header, sizeof(header)); }
        const auto partial = root / "juiced/prepared/.partial-writer-fixture/owned";
        auto control = std::make_shared<PreparationProcessControl>();
        const auto wait = [](auto ready) {
            const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(10);
            while (!ready() && std::chrono::steady_clock::now() < deadline) std::this_thread::sleep_for(std::chrono::milliseconds(10));
            return ready();
        };
        {
            Server server(config, root.string(), root.string());
            Hook::models(server).register_user_model("Preparation-Source", {{"recipe", "llamacpp"}, {"source", "local_path"},
                {"checkpoints", {{"main", source.string()}}}});
            Hook::retain_writer(server, "Preparation-Source", source, control);
            check(wait([&] { return control->started.load(); }), "a controlled retained legacy writer has an owned process");
            check(Hook::writer(server)["writer_state"] == "dispatching" && fs::exists(partial),
                  "legacy cleanup retains its recorded writer and staging ownership");
            if (boundary == 0) {
                const auto cancel = Hook::invoke(server, "/v1/juiced/preparation", {{"action", "cancel"}, {"id", "writer-fixture"}});
                check(cancel.status == 200, "native preparation cancellation acknowledges the request");
            } else Hook::stop_preparation(server);
            check(wait([&] { return Hook::writer(server).value("cleanup_pending", false); }),
                  "unconfirmed cancellation or shutdown retains a pending cleanup outcome");
            check(control->running && control->stops > 0 && fs::exists(partial) &&
                  Hook::writer(server)["pending"] == true && Hook::admission(server)["active"].get<int>() >= 2,
                  "failed stop retains the process, staging and accepted operation claims");
            bool guarded = false;
            try { Hook::configure(server, {{"mode", "protected"}}); }
            catch (const AdmissionError& error) { guarded = error.code == "admission_configuration_busy"; }
            check(guarded, "configuration cannot discard an unresolved preparation writer");
            bool unregister_guarded = false, delete_guarded = false;
            try { Hook::models(server).unregister_user_model("Preparation-Source"); }
            catch (const AdmissionError& error) { unregister_guarded = error.status == 409 && error.code == "preparation_cleanup_pending"; }
            try { Hook::models(server).delete_model("Preparation-Source"); }
            catch (const AdmissionError& error) { delete_guarded = error.status == 409 && error.code == "preparation_cleanup_pending"; }
            check(unregister_guarded && delete_guarded && fs::exists(source) && Hook::models(server).model_exists("Preparation-Source"),
                  "native unregister and delete retain an unresolved preparation's source and registration");
            if (boundary != 2) {
                control->fail_stop = false;
                if (boundary == 1) Hook::stop_preparation(server);
                check(wait([&] { return !Hook::writer(server).value("pending", true) && Hook::admission(server)["active"] == 0; }),
                      "confirmed cleanup retry commits terminal status before releasing operation ownership");
                check(!fs::exists(partial) && !control->running && Hook::writer(server)["writer_state"] == "stopped" &&
                      Hook::writer(server)["phase"] == (boundary == 0 ? "cancelled" : "interrupted"),
                      "only confirmed cleanup removes staging and publishes the requested terminal outcome");
            }
        }
        if (boundary == 2) {
            Server restarted(config, root.string(), root.string());
            const auto recovered = Hook::writer(restarted);
            check(recovered["pending"] == true && recovered["cleanup_pending"] == true && fs::exists(partial),
                  "restart retains unknown prior-manager staging ownership without certifying termination");
            check(!Hook::removal(restarted, "Preparation-Source")["allowed"].get<bool>(),
                  "unknown prior preparation continues to protect its source from ordinary removal");
            bool native_guarded = false;
            try { Hook::models(restarted).delete_model("Preparation-Source"); }
            catch (const AdmissionError& error) { native_guarded = error.status == 409 && error.code == "preparation_cleanup_pending"; }
            check(native_guarded && fs::exists(source), "restart preserves the source guard through native deletion");
        }
        RuntimeConfig::set_global(nullptr);
    }
}

static void seed_preparation_snapshot(const fs::path& root, const fs::path& source, const json& record) {
    ModelManager previous;
    previous.register_user_model("Preparation-Source", {{"recipe", "llamacpp"}, {"source", "local_path"},
        {"checkpoints", {{"main", source.string()}}}});
    fs::create_directories(root / "juiced");
    const json state = {{"version", 1}, {"revision", 0}, {"policies", json::object()}, {"imports", json::object()},
        {"cache", {{"mode", "memory"}, {"retention_days", 7}, {"disk_budget_gib", 8}}},
        {"preparations", {{"writer-fixture", record}}}};
    std::ofstream(root / "juiced/state.json") << state;
}

static void preparation_retirement() {
    using Hook = StatePublicationTestHook;
    const json base = {{"model", "Preparation-Source"}, {"phase", "dispatching"}, {"writer_state", "dispatching"}};
    auto missing = base;
    auto invalid_pending = base; invalid_pending["pending"] = "true";
    auto inconsistent = base; inconsistent["pending"] = false;
    auto unsupported = base; unsupported["pending"] = true; unsupported["writer_state"] = "future-writer-state";
    auto wrong_writer_type = base; wrong_writer_type["pending"] = true; wrong_writer_type["writer_state"] = 7;
    auto missing_source = base; missing_source["pending"] = true; missing_source.erase("model");
    for (const auto& original : {missing, invalid_pending, inconsistent, unsupported, wrong_writer_type, missing_source}) {
        const auto root = test::make_scratch_dir("preparation-quarantine");
        auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto config = std::make_shared<RuntimeConfig>(defaults); RuntimeConfig::set_global(config.get());
        const auto source = root / "source.gguf";
        { char header[24] = {'G', 'G', 'U', 'F', 3}; std::ofstream file(source, std::ios::binary); file.write(header, sizeof(header)); }
        const auto partial = root / "juiced/prepared/.partial-writer-fixture/owned";
        fs::create_directories(partial.parent_path()); std::ofstream(partial) << "unconfirmed writer dependency";
        seed_preparation_snapshot(root, source, original);
        for (int restart = 0; restart < 2; ++restart) {
            Server recovered(config, root.string(), root.string());
            const auto record = Hook::writer(recovered);
            check(record["pending"] == true && record["cleanup_pending"] == true && record["recovery_record"] == original,
                  "ambiguous retained writer records remain quarantined with their original record across restart");
            const auto cancel = Hook::invoke(recovered, "/v1/juiced/preparation", {{"action", "cancel"}, {"id", "writer-fixture"}});
            check(cancel.status == 200 && json::parse(cancel.body)["recovery_required"] == true &&
                  Hook::writer(recovered)["pending"] == true && fs::exists(partial),
                  "cancelling an unknown prior writer never certifies cleanup or removes staging");
            bool guarded = false;
            try { Hook::models(recovered).delete_model("Preparation-Source"); }
            catch (const AdmissionError& error) { guarded = error.code == "preparation_cleanup_pending"; }
            check(guarded && fs::exists(source), "quarantined writer intent continues to protect source registration and weights");
        }
        RuntimeConfig::set_global(nullptr);
    }

}

static void preparation_legacy_state() {
    using Hook = StatePublicationTestHook;
    for (bool queued : {false, true}) {
        const auto root = test::make_scratch_dir("preparation-retained");
        auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto config = std::make_shared<RuntimeConfig>(defaults); RuntimeConfig::set_global(config.get());
        const auto source = root / "source.gguf";
        { char header[24] = {'G', 'G', 'U', 'F', 3}; std::ofstream file(source, std::ios::binary); file.write(header, sizeof(header)); }
        const json record = {{"model", "Preparation-Source"}, {"pending", queued},
            {"phase", queued ? "queued" : "complete"}, {"writer_state", queued ? "queued" : "stopped"}};
        seed_preparation_snapshot(root, source, record);
        {
            Server recovered(config, root.string(), root.string());
            const auto current = Hook::writer(recovered);
            check(current["pending"] == false && current["phase"] == (queued ? "interrupted" : "complete") &&
                  !current.contains("recovery_record") && !fs::exists(root / "juiced/preparations"),
                  "a proven queued job is interrupted without dispatch while valid completed history remains intact");
            check(Hook::models(recovered).model_exists("Preparation-Source") && fs::exists(source) &&
                  Hook::removal(recovered, "Preparation-Source")["allowed"] == true,
                  "valid retained records preserve weights and normal reference-safe removal");
        }
        RuntimeConfig::set_global(nullptr);
    }

}

static void voice_intent_recovery() {
    using Hook = StatePublicationTestHook;
    VoiceEnvironment enabled("JUICED_WYOMING_ENABLED", "false");
    VoiceEnvironment executable("JUICED_VOICE_PYTHON", "controlled-voice-python");
    VoiceEnvironment script("JUICED_VOICE_SCRIPT", "controlled-voice-script");
    auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto config = std::make_shared<RuntimeConfig>(defaults); RuntimeConfig::set_global(config.get());
    const json pending = {{"owner", "previous-manager"}, {"pending", true}, {"state", "dispatching"}};
    auto missing = pending; missing.erase("pending");
    auto inconsistent = pending; inconsistent["pending"] = false;
    auto owner_type = pending; owner_type["owner"] = 3;
    auto empty_owner = pending; empty_owner["owner"] = "";
    auto pending_type = pending; pending_type["pending"] = "true";
    auto unsupported = pending; unsupported["state"] = "unknown";
    auto extra = pending; extra["version"] = 2;
    const std::vector<json> ambiguous = {missing, inconsistent, owner_type, empty_owner, pending_type, unsupported, extra, json(nullptr)};
    fs::path restart_root;
    for (const auto& marker : ambiguous) {
        const auto root = test::make_scratch_dir("voice-intent-recovery");
        fs::create_directories(root / "juiced");
        json initial = {{"version", 1}, {"revision", 0}, {"policies", json::object()},
            {"cache", {{"mode", "memory"}}}, {"imports", json::object()}, {"voice_writer", marker}};
        { std::ofstream out(root / "juiced/state.json"); out << initial; }
        enabled.set("false");
        auto control = std::make_shared<VoiceProcessControl>();
        {
            Server server(config, root.string(), root.string()); Hook::stop_preparation(server);
            check(Hook::voice_status(server).value("recovery_required", false) && Hook::voice_intent(server) == marker,
                  "constructor quarantines a present ambiguous voice intent without changing it");
            Hook::voice_factory(server, control); Hook::voice_models(server, "Voice-Recovery"); enabled.set("true");
            Hook::voice(server); Hook::voice(server); Hook::stop_preparation(server);
            check(control->starts == 0 && Hook::voice_status(server).value("recovery_required", false) &&
                  Hook::voice_intent(server) == marker,
                  "reconciliation and shutdown preserve ambiguous voice ownership without dispatch");
        }
        json persisted; { std::ifstream in(root / "juiced/state.json"); in >> persisted; }
        check(persisted.at("voice_writer") == marker, "durable voice quarantine preserves the original record");
        restart_root = root;
    }
    {
        enabled.set("false"); Server restarted(config, restart_root.string(), restart_root.string()); Hook::stop_preparation(restarted);
        auto control = std::make_shared<VoiceProcessControl>(); Hook::voice_factory(restarted, control);
        Hook::voice_models(restarted, "Voice-Recovery"); enabled.set("true"); Hook::voice(restarted);
        check(control->starts == 0 && Hook::voice_status(restarted).value("recovery_required", false) &&
              Hook::voice_intent(restarted).is_null(), "manager restart cannot replace quarantined ambiguous voice intent");
    }
    for (bool stopped : {false, true}) {
        enabled.set("false"); const auto root = test::make_scratch_dir("voice-valid-intent");
        if (stopped) {
            fs::create_directories(root / "juiced");
            json initial = {{"version", 1}, {"revision", 0}, {"policies", json::object()},
                {"cache", {{"mode", "memory"}}}, {"imports", json::object()},
                {"voice_writer", {{"owner", "previous-manager"}, {"pending", false}, {"state", "stopped"}}}};
            std::ofstream(root / "juiced/state.json") << initial;
        }
        Server server(config, root.string(), root.string()); Hook::stop_preparation(server);
        auto control = std::make_shared<VoiceProcessControl>(); Hook::voice_factory(server, control);
        Hook::voice_models(server, "Voice-Valid"); enabled.set("true"); Hook::voice(server);
        check(control->starts == 1 && control->running && !Hook::voice_status(server).value("recovery_required", false),
              "absent legacy and valid stopped intents permit one owned voice dispatch");
        enabled.set("false"); Hook::voice(server);
    }
    RuntimeConfig::set_global(nullptr);
}

static void voice_helper_ownership() {
    using Hook = StatePublicationTestHook;
    VoiceEnvironment enabled("JUICED_WYOMING_ENABLED", "false");
    VoiceEnvironment executable("JUICED_VOICE_PYTHON", "controlled-voice-python");
    VoiceEnvironment script("JUICED_VOICE_SCRIPT", "controlled-voice-script");
    VoiceEnvironment uri("JUICED_WYOMING_URI", "tcp://127.0.0.1:10300");
    auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    const auto root = test::make_scratch_dir("voice-ownership");
    auto config = std::make_shared<RuntimeConfig>(defaults); RuntimeConfig::set_global(config.get());
    auto control = std::make_shared<VoiceProcessControl>();
    {
        Server server(config, root.string(), root.string()); Hook::stop_preparation(server);
        Hook::voice_factory(server, control); Hook::voice_models(server, "Voice-One"); enabled.set("true");
        Hook::pause(server, true); Hook::voice(server);
        check(control->starts == 0 && Hook::admission(server)["active"] == 0,
              "paused admission rejects voice before intent or process dispatch");
        Hook::pause(server, false); Hook::configure(server, {{"mode", "protected"}}); Hook::voice(server);
        check(control->starts == 0 && Hook::admission(server)["active"] == 0,
              "protected admission contains the unqualified voice helper before dispatch");
        Hook::configure(server, {{"mode", "legacy"}}); Hook::voice(server);
        check(control->starts == 1 && control->running && Hook::voice_status(server)["process_running"] == true &&
              Hook::voice_intent(server)["pending"] == true && Hook::admission(server)["active"] == 1,
              "voice dispatch persists intent and retains its accepted native helper ticket");
        bool blocked = false;
        try { Hook::configure(server, {{"mode", "protected"}}); }
        catch (const AdmissionError& error) { blocked = error.code == "admission_configuration_busy"; }
        check(blocked, "an accepted voice helper blocks protected activation until cleanup is confirmed");
        Hook::voice(server);
        check(control->starts == 1, "unchanged voice configuration keeps its existing process owner");
        control->fail_stop = true; Hook::voice_models(server, "Voice-Two"); Hook::voice(server);
        check(control->starts == 1 && control->running && Hook::voice_status(server)["cleanup_pending"] == true && Hook::admission(server)["active"] == 1,
              "failed replacement stop keeps the old process and ticket without launching another helper");
        Hook::stop_preparation(server);
        check(control->starts == 1 && control->running && Hook::voice_intent(server)["pending"] == true && Hook::admission(server)["active"] == 1,
              "shutdown acknowledges no cleanup success while voice termination remains unconfirmed");
        control->fail_stop = false; Hook::voice(server);
        check(control->starts == 2 && control->running && !control->overlapped && Hook::admission(server)["active"] == 1,
              "confirmed cleanup permits one replacement without overlapping helper ownership");
        uri.set("tcp://127.0.0.1:10301"); Hook::voice(server);
        check(control->starts == 3 && std::find(control->command.args.begin(), control->command.args.end(), "tcp://127.0.0.1:10301") != control->command.args.end(),
              "voice endpoint changes retire and replace the command snapshot");
        enabled.set("false"); control->fail_stop = true; Hook::voice(server);
        check(Hook::voice_status(server)["cleanup_pending"] == true && Hook::admission(server)["active"] == 1,
              "disabling voice cannot discard unconfirmed process ownership");
        control->fail_stop = false; Hook::voice(server);
        check(!control->running && Hook::admission(server)["active"] == 0 && Hook::voice_intent(server)["pending"] == false,
              "confirmed disabled-helper cleanup commits stopped intent before releasing the ticket");
        enabled.set("true"); control->fail_start = true; control->fail_stop = true; Hook::voice(server);
        check(control->starts == 4 && control->running && Hook::voice_status(server)["cleanup_pending"] == true && Hook::admission(server)["active"] == 1,
              "post-dispatch startup failure retains the process when its cleanup also fails");
        control->fail_start = false; control->fail_stop = false; Hook::voice(server);
        check(control->starts == 5 && !control->overlapped && Hook::admission(server)["active"] == 1,
              "startup cleanup retry reconciles its owner before allowing another dispatch");
        enabled.set("false"); fs::create_directory(root / "juiced/state.json.tmp"); Hook::voice(server);
        check(!control->running && Hook::admission(server)["active"] == 1 && Hook::voice_intent(server)["pending"] == true &&
              Hook::voice_status(server)["cleanup_pending"] == true,
              "failed cleanup publication retains the stopped process owner and ticket for retry");
        fs::remove(root / "juiced/state.json.tmp"); Hook::voice(server);
        check(Hook::admission(server)["active"] == 0 && Hook::voice_intent(server)["pending"] == false,
              "cleanup publication retry commits the terminal result before releasing ownership");
        fs::create_directory(root / "juiced/state.json.tmp"); enabled.set("true"); Hook::voice(server);
        check(control->starts == 5 && !control->running && Hook::admission(server)["active"] == 0 && Hook::voice_intent(server)["pending"] == false,
              "failed durable voice intent prevents dispatch and releases the unstarted ticket");
        fs::remove(root / "juiced/state.json.tmp"); Hook::voice(server);
        check(control->starts == 6 && control->running, "voice dispatch retries after metadata persistence is repaired");
        control->fail_stop = true; Hook::stop_preparation(server);
    }
    {
        enabled.set("false"); Server restarted(config, root.string(), root.string()); Hook::stop_preparation(restarted);
        Hook::voice_factory(restarted, control); enabled.set("true"); Hook::voice(restarted);
        check(control->starts == 6 && Hook::voice_status(restarted)["cleanup_pending"] == true && Hook::voice_intent(restarted)["pending"] == true,
              "manager restart retains prior-helper uncertainty and refuses duplicate dispatch");
    }
    RuntimeConfig::set_global(nullptr);
}

int main(int argc, char** argv) {
    try {
        if (argc > 1 && std::string(argv[1]) == "--voice-cleanup") voice_helper_ownership();
        else if (argc > 1 && std::string(argv[1]) == "--voice-intent") voice_intent_recovery();
        else if (argc > 1 && std::string(argv[1]) == "--preparation-cleanup") preparation_cleanup();
        else if (argc > 1 && std::string(argv[1]) == "--preparation-retirement") preparation_retirement();
        else if (argc > 1 && std::string(argv[1]) == "--preparation-legacy-state") preparation_legacy_state();
        else { native_owners(); entry_boundaries(); worker_start_boundaries(); }
    }
    catch (const std::exception& error) { ++failures; std::fprintf(stderr, "Unexpected fixture failure: %s\n", error.what()); }
    std::fprintf(stderr, "Management admission: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
