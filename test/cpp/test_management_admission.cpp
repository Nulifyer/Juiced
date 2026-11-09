#include "lemon/backend_manager.h"
#include "lemon/backends/backend_ops.h"
#include "lemon/backends/llamacpp/llamacpp_server.h"
#include "lemon/config_file.h"
#include "lemon/juiced/service.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "lemon/utils/path_utils.h"
#include "support/test_state_isolation.h"

#include <atomic>
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
namespace lemon {
struct ModelPublicationTestHook {
    static size_t history(ModelManager& models) {
        std::lock_guard<std::mutex> lock(models.sync_state_.mutex);
        return models.sync_state_.completed_generation_results.size();
    }
};
struct StatePublicationTestHook {
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
    static void queue_preparation(Server& server, std::function<void()> barrier, bool accepted) {
        ResourceAdmission::Lease admission;
        if (accepted) admission = server.router_->admit_management("queued-fixture", "preparation-entry");
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        server.juiced_->preparation_admission_barrier_ = std::move(barrier);
        json job = {{"id", "queued-fixture"}, {"model", "Missing-Preparation-Fixture"}, {"pending", true}, {"phase", "queued"}};
        server.juiced_->state_["preparations"]["queued-fixture"] = job;
        server.juiced_->preparation_pending_.push_back({job, std::move(admission)}); server.juiced_->wake_.notify_all();
    }
    static json preparation(Server& server) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        return server.juiced_->state_["preparations"].value("queued-fixture", json::object());
    }
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
                        check(response.status == 503 && error.at("code") == code && response.get_header_value("Retry-After") == "1" && !response.content_provider_,
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
        for (bool accepted : {true, false}) {
            StatePublicationTestHook::configure(server, {{"mode", "legacy"}});
            std::promise<void> entered, release; auto released = release.get_future().share();
            StatePublicationTestHook::queue_preparation(server, [&] { entered.set_value(); released.wait(); }, accepted);
            const bool queued = entered.get_future().wait_for(std::chrono::seconds(5)) == std::future_status::ready;
            bool conflict = false;
            try { StatePublicationTestHook::configure(server, {{"mode", "protected"}}); }
            catch (const AdmissionError& error) { conflict = error.code == "admission_configuration_busy"; }
            release.set_value();
            const auto until = std::chrono::steady_clock::now() + std::chrono::seconds(5);
            json job;
            do {
                job = StatePublicationTestHook::preparation(server);
                if (!job.value("pending", true) && StatePublicationTestHook::admission(server)["active"] == 0) break;
                std::this_thread::sleep_for(std::chrono::milliseconds(10));
            } while (std::chrono::steady_clock::now() < until);
            check(queued && conflict == accepted && job.value("phase", "") == "failed" &&
                  !fs::exists(root / "juiced/preparations/queued-fixture") && StatePublicationTestHook::admission(server)["active"] == 0,
                  accepted ? "accepted queued preparation retains admission through native dispatch and terminal status" :
                             "unadmitted queued preparation rechecks committed policy before filesystem/process dispatch");
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

int main() {
    try { native_owners(); entry_boundaries(); worker_start_boundaries(); }
    catch (const std::exception& error) { ++failures; std::fprintf(stderr, "Unexpected fixture failure: %s\n", error.what()); }
    std::fprintf(stderr, "Management admission: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
