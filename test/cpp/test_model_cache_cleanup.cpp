#include "lemon/backends/hf_cache_util.h"
#include "lemon/config_file.h"
#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/router.h"
#include "lemon/server.h"
#include "lemon/utils/path_utils.h"
#include "support/test_state_isolation.h"

#include <atomic>
#include <cstdio>
#include <fstream>
#include <future>
#include <thread>

using namespace lemon;
using namespace std::chrono_literals;
namespace fs = std::filesystem;
static int checks = 0, failures = 0;
static void check(bool value, const char* label) {
    ++checks; std::fprintf(stderr, "%s: %s\n", value ? "PASS" : "FAIL", label);
    if (!value) ++failures;
}
template<class Action> static bool rejected(Action action, const char* code) {
    try { action(); return false; }
    catch (const AdmissionError& error) { return error.code == code; }
}
template<class Ready> static bool wait(Ready ready) {
    const auto deadline = std::chrono::steady_clock::now() + 5s;
    while (!ready() && std::chrono::steady_clock::now() < deadline) std::this_thread::sleep_for(5ms);
    return ready();
}
static void file(const fs::path& path, const std::string& text = "fixture") {
    fs::create_directories(path.parent_path()); std::ofstream out(path, std::ios::binary);
    char header[24] = {'G', 'G', 'U', 'F', 3}; out.write(header, sizeof(header)); out << text;
}
class TemplateEnvironment {
public:
    explicit TemplateEnvironment(std::string name = "LLAMA_ARG_CHAT_TEMPLATE_FILE")
        : name_(std::move(name)), previous_(utils::get_environment_variable_utf8(name_)) { utils::unset_environment_variable_utf8(name_); }
    ~TemplateEnvironment() {
        if (previous_.empty()) utils::unset_environment_variable_utf8(name_); else utils::set_environment_variable_utf8(name_, previous_);
    }
    static void set(const std::string& value) { utils::set_environment_variable_utf8("LLAMA_ARG_CHAT_TEMPLATE_FILE", value); }
private:
    std::string name_, previous_;
};
struct WorkerControl { int starts = 0; bool running = false, fail_stop = false; };
class CacheWorker : public WrappedServer {
public:
    explicit CacheWorker(std::shared_ptr<WorkerControl> control) : WrappedServer("cache-owner-fixture", "error"), control_(std::move(control)) {}
    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override { ++control_->starts; control_->running = true; set_state(ModelState::READY); }
    void unload() override {
        if (control_->fail_stop) throw std::runtime_error("controlled unconfirmed cache worker stop");
        control_->running = false; release_resource_residency(); set_state(ModelState::UNLOADED);
    }
    bool is_backend_alive() const override { return control_->running; }
    DeviceType effective_device(const RecipeOptions&) const override { return DEVICE_CPU; }
    json chat_completion(const json&) override { return json::object(); }
private:
    std::shared_ptr<WorkerControl> control_;
};
namespace lemon {
struct ModelPublicationTestHook {
    static void barrier(ModelManager& models, std::function<void(const std::string&)> hook) { models.removal_publication_barrier_ = std::move(hook); }
    static void invalidate(ModelManager& models) { models.invalidate_models_cache(); }
    static void hide(ModelManager& models, const std::string& name) {
        std::lock_guard lock(models.models_cache_mutex_); models.models_cache_.erase(name);
    }
    static void builtin(ModelManager& models, const std::string& name, const json& definition) {
        std::lock_guard lock(models.models_cache_mutex_); models.server_models_[name] = definition; models.invalidate_models_cache_locked();
    }
    static void erase_builtin(ModelManager& models, const std::string& name) {
        std::lock_guard lock(models.models_cache_mutex_); models.server_models_.erase(name); models.invalidate_models_cache_locked();
    }
};
struct RoutingHelperTestHook {
    static void factory(Router& router, const std::shared_ptr<WorkerControl>& control) {
        router.backend_factory_override_ = [control](const auto&) { return std::make_unique<CacheWorker>(control); };
    }
};
struct StatePublicationTestHook {
    static ModelManager& models(Server& server) { return *server.model_manager_; }
    static Router& router(Server& server) { return *server.router_; }
    static ResourceAdmission admission(Server& server) { return server.router_->admission_view(); }
    static void settle(Server& server) {
        server.juiced_->stop();
        if (server.model_cache_warmup_thread_.joinable()) server.model_cache_warmup_thread_.join();
    }
    static httplib::Response cleanup(Server& server, const json& body) {
        httplib::Request request; request.method = "POST"; request.body = body.dump();
        httplib::Response response; response.status = 200; server.handle_cleanup_cache(request, response); return response;
    }
    static httplib::Response configure(Server& server, const json& body) {
        httplib::Request request; request.method = "POST"; request.body = body.dump();
        httplib::Response response; response.status = 200; server.handle_config_set(request, response); return response;
    }
    static void retain_unknown_preparation(Server& server) {
        std::lock_guard lock(server.juiced_->mutex_);
        server.juiced_->state_["preparations"]["old-writer"] = {{"model", "A-Different-Model"}, {"pending", true},
            {"phase", "cleanup-pending"}, {"writer_state", "dispatching"}, {"cleanup_pending", true}};
        server.juiced_->persist_locked();
    }
    static std::string models_dir(Server& server) { return server.config_->models_dir(); }
    static json saved_config(Server& server) { return ConfigFile::load_raw(server.config_dir_); }
    static fs::path config_staging(Server& server) { return fs::path(server.config_dir_) / "config.json.tmp"; }
    static void effect_failure(Server& server, bool fail) {
        server.config_effect_barrier_ = fail ? [] { throw std::runtime_error("controlled storage effect failure"); } : std::function<void()>{};
    }
};
}

static void ledger_exclusion() {
    ResourceAdmission ledger;
    const auto load_work = [](uint64_t generation) {
        ResourceAdmission::Work work; work.model = "cache-model"; work.generation = generation;
        work.operation = ResourceAdmission::Operation::Load; work.model_cache_root = "cache-A"; return work;
    };
    {
        auto load = ledger.acquire(load_work(10)); load.started(); load.retain_residency();
        check(rejected([&] { ledger.acquire_management("cache", "model-cache-cleanup", "cache-A"); }, "model_cache_busy"),
              "resident loads retain cache ownership independently of the current registration");
        auto other = ledger.acquire_management("cache", "model-cache-cleanup", "cache-B");
        check(ledger.snapshot()["model_cache"]["cleanups"] == 1, "an unrelated cache root remains independent");
    }
    { auto load = ledger.acquire(load_work(11)); load.started(); }
    check(rejected([&] { ledger.acquire_management("cache", "model-cache-cleanup", "cache-A"); }, "model_cache_busy"),
          "released but unconfirmed load ownership blocks destructive cleanup");
    ledger.reconcile_stopped_generation(12);
    check(ledger.snapshot()["model_cache"]["uncertain"] == 1, "another generation cannot clear cache uncertainty");
    ledger.reconcile_stopped_generation(11);
    {
        auto cleanup = ledger.acquire_management("cache", "model-cache-cleanup", "cache-A");
        check(rejected([&] { ledger.acquire(load_work(13)); }, "model_cache_busy") &&
              rejected([&] { ledger.acquire_management("download", "model-acquisition", "cache-A"); }, "model_cache_busy"),
              "exclusive cleanup rejects new loads and metadata/download acquisition before dispatch");
        check(rejected([&] { ledger.write_scope("new-options-reference"); }, "model_cache_busy"),
              "exclusive cleanup closes new native metadata reference mutations");
    }
    {
        auto writer = ledger.write_scope("accepted-options-reference");
        auto incoming = std::async(std::launch::async, [&] {
            return rejected([&] { ledger.acquire_management("cache", "model-cache-cleanup", "cache-A"); }, "model_cache_busy");
        });
        check(incoming.get(),
              "cleanup cannot overtake an accepted metadata reference mutation");
    }
    {
        auto writer = ledger.write_scope("cache-root-change"); ledger.reserve_model_cache_change({"cache-A", "cache-B"});
        auto incoming = std::async(std::launch::async, [&] { return rejected([&] { ledger.acquire(load_work(15)); }, "model_cache_busy"); });
        check(incoming.get() && ledger.snapshot()["model_cache"]["root_changes"] == 1,
              "a bound root transition rejects fresh loads through its existing writer owner");
        auto continued = ledger.acquire_management("continued", "model-acquisition", "cache-B");
        check(ledger.snapshot()["model_cache"]["new_writes_closed"] == true, "the transition's own accepted effects retain their continuation");
    }
    {
        ResourceAdmission protected_metadata; protected_metadata.configure({{"mode", "protected"}});
        auto writer = protected_metadata.write_scope("metadata-only-root-change"); protected_metadata.reserve_model_cache_change({"cache-A", "cache-B"});
        check(rejected([&] { protected_metadata.acquire_management("cache", "model-cache-cleanup", "cache-A"); }, "operation_unqualified"),
              "logical root exclusion never waives protected acquisition envelopes");
    }
    ledger.configure({{"mode", "legacy"}, {"active_limit", 1}, {"interactive_wait_ms", 10000}});
    ResourceAdmission::Work busy; busy.model = "unrelated"; auto active = ledger.acquire(busy);
    auto queued_work = load_work(14); std::atomic<bool> cancelled{false}; queued_work.cancelled = [&] { return cancelled.load(); };
    auto queued = std::async(std::launch::async, [&] {
        try { auto work = ledger.acquire(queued_work); return false; } catch (const AdmissionError& error) { return error.code == "request_cancelled"; }
    });
    const bool pending = wait([&] { return ledger.snapshot()["pending"] == 1; });
    check(pending && rejected([&] { ledger.acquire_management("cache", "model-cache-cleanup", "cache-A"); }, "model_cache_busy"),
          "cleanup cannot overtake a queued cache load");
    cancelled = true; active = {}; check(queued.get(), "queued cancellation releases only its accepted ownership");
    auto cleanup = ledger.acquire_management("cache", "model-cache-cleanup", "cache-A");
    check(ledger.snapshot()["model_cache"]["references"] == 0, "confirmed collection and queue release reopen cleanup");
}

struct CacheFiles {
    fs::path directory, main, legacy, companion, backup, blob;
    explicit CacheFiles(ModelManager& models) {
        const auto root = fs::path(models.model_cache_root());
        const auto relative = root.lexically_relative(fs::weakly_canonical(test::state_dir()));
        if (relative.empty() || *relative.begin() == "..") throw std::runtime_error("Fixture model cache is not isolated");
        directory = root / backends::hf_cache::repo_id_to_cache_dir_name("fixture/cleanup-origin");
        main = directory / "snapshots" / "old" / "main.gguf"; legacy = directory / "snapshots" / "old" / "mmproj.gguf";
        companion = root / backends::hf_cache::repo_id_to_cache_dir_name("fixture/cleanup-companion") / "snapshots" / "current" / "mmproj.gguf";
        backup = directory / "snapshots" / "old" / "mmproj.gguf.backup"; blob = directory / "blobs" / "mmproj.gguf";
        file(main); file(legacy); file(companion); file(backup); file(blob);
        fs::create_directories(directory / "refs"); std::ofstream(directory / "refs" / "main") << "old\n";
        const auto companion_root = companion.parent_path().parent_path().parent_path();
        fs::create_directories(companion_root / "refs"); std::ofstream(companion_root / "refs" / "main") << "current\n";
        models.register_user_model("Cleanup-Origin", definition());
    }
    json local(const fs::path& path, bool plural = false) const {
        json result = {{"recipe", "llamacpp"}, {"source", "local_path"}};
        if (plural) result["checkpoints"] = {{"main", path.string()}}; else result["checkpoint"] = path.string();
        return result;
    }
    json definition() const {
        return {{"recipe", "llamacpp"}, {"checkpoints", {{"main", "fixture/cleanup-origin:main.gguf"}, {"mmproj", "fixture/cleanup-companion:mmproj.gguf"}}}};
    }
};

static void file_references(Server& server, const CacheFiles& paths) {
    using Hook = ModelPublicationTestHook; auto& models = StatePublicationTestHook::models(server);
    const auto preview = models.cleanup_orphaned_cache(true);
    check(preview["orphaned_files"].size() == 1 && preview["orphaned_files"][0]["path"] == paths.legacy.string() && fs::exists(paths.legacy),
          "preview identifies the exact obsolete layout entry without touching it");
    for (bool plural : {false, true}) {
        models.register_user_model("Cleanup-Shared", paths.local(paths.legacy, plural)); models.get_model_info("Cleanup-Shared");
        Hook::hide(models, "user.Cleanup-Shared");
        check(models.cleanup_orphaned_cache(true)["orphaned_files"].empty(), "singular/plural references survive exclusion from the filtered cache");
        models.unregister_user_model("Cleanup-Shared");
    }
    Hook::builtin(models, "Cleanup-Builtin", paths.local(paths.legacy)); models.get_model_info("Cleanup-Builtin"); Hook::hide(models, "Cleanup-Builtin");
    check(models.cleanup_orphaned_cache(true)["orphaned_files"].empty(), "hidden built-in definitions still protect their files");
    Hook::erase_builtin(models, "Cleanup-Builtin");
    models.register_user_model("Cleanup-Directory", paths.local(paths.directory));
    check(models.cleanup_orphaned_cache(true)["orphaned_files"].empty(), "directory checkpoints retain their complete contents");
    models.unregister_user_model("Cleanup-Directory");
    const auto linked = paths.directory / "shared-link.gguf";
    std::error_code link_error; fs::create_hard_link(paths.legacy, linked, link_error);
    if (!link_error) {
        models.register_user_model("Cleanup-Linked", paths.local(linked));
        check(models.cleanup_orphaned_cache(true)["orphaned_files"].empty(), "filesystem identity retains files referenced through another hard link");
        models.unregister_user_model("Cleanup-Linked"); fs::remove(linked);
    } else std::fprintf(stderr, "SKIP: hard links unavailable on this fixture filesystem\n");
    fs::remove(paths.companion); fs::remove(paths.blob); Hook::invalidate(models);
    check(models.get_model_info("Cleanup-Origin").resolved_path("mmproj") == paths.legacy.string() && models.cleanup_orphaned_cache(true)["orphaned_files"].empty(),
          "cleanup preserves the legacy companion when it remains the usable fallback");
    file(paths.companion); file(paths.blob); Hook::invalidate(models);
    check(fs::exists(paths.blob) && fs::exists(paths.backup) && models.cleanup_orphaned_cache(true)["orphaned_files"].size() == 1,
          "filename prefixes and blob directories never become cleanup candidates");
}

static void worker_ownership(Server& server, const CacheFiles& paths) {
    using Hook = StatePublicationTestHook; auto& models = Hook::models(server); auto& router = Hook::router(server);
    auto control = std::make_shared<WorkerControl>(); RoutingHelperTestHook::factory(router, control);
    models.register_user_model("Cleanup-Live", paths.local(paths.legacy));
    const auto info = models.get_model_info("Cleanup-Live");
    const RecipeOptions options("llamacpp", {{"ctx_size", 2048}, {"llamacpp_backend", "cpu"}});
    router.load_model("Cleanup-Live", info, options, true);
    const auto original_root = Hook::models_dir(server);
    const auto original_config = Hook::saved_config(server);
    const auto moved = Hook::configure(server, {{"models_dir", test::make_scratch_dir("live-root-change").string()}});
    check(moved.status == 503 && Hook::models_dir(server) == original_root && Hook::saved_config(server) == original_config,
          "model-cache root changes reject before persistence while an affected worker owns files");
    models.register_user_model("Cleanup-Live", paths.local(paths.main));
    const auto busy = Hook::cleanup(server, {{"dry_run", false}});
    check(control->starts == 1 && busy.status == 503 && json::parse(busy.body)["error"]["code"] == "model_cache_busy" && busy.get_header_value("Retry-After") == "1" && fs::exists(paths.legacy),
          "native load residency blocks cleanup after its registration changes and preserves retry metadata");
    control->fail_stop = true;
    bool failed = false;
    try { router.unload_model("Cleanup-Live"); } catch (const std::exception&) { failed = true; }
    check(failed && control->running,
          "failed native unload keeps its generation available for cleanup retry");
    check(Hook::cleanup(server, {{"dry_run", false}}).status == 503 && fs::exists(paths.legacy), "unconfirmed worker cleanup cannot release cache deletion authority");
    control->fail_stop = false; router.unload_model("Cleanup-Live");
    models.unregister_user_model("Cleanup-Live");
    check(Hook::admission(server).snapshot()["model_cache"]["references"] == 0, "confirmed native unload releases its cache reference");
}

static void option_references(Server& server, const CacheFiles& paths) {
    auto& models = StatePublicationTestHook::models(server);
    const auto argument = "--chat-template-file \"" + paths.legacy.generic_string() + "\"";
    models.update_saved_model_options("Cleanup-Origin", {{"llamacpp_args", argument}});
    check(models.cleanup_orphaned_cache(true)["orphaned_files"].empty(), "saved argument file inputs retain their cache files");
    models.update_saved_model_options("Cleanup-Origin", {{"llamacpp_args", nullptr}});
    models.select_execution_profile("Cleanup-Origin", {{"id", "saved-input"}, {"recipe", "llamacpp"}, {"options", {{"llamacpp_args", argument}}}});
    models.select_execution_profile("Cleanup-Origin", {{"id", "registered"}});
    check(models.cleanup_orphaned_cache(true)["orphaned_files"].empty(), "an unselected saved profile still retains its declared file inputs");
    models.select_execution_profile("Cleanup-Origin", {{"id", "saved-input"}, {"recipe", "llamacpp"}, {"options", {{"llamacpp_args", nullptr}}}});
    models.select_execution_profile("Cleanup-Origin", {{"id", "registered"}});
    for (const auto& arguments : {"--future-file-input opaque", "--lora relative.gguf"}) {
        models.update_saved_model_options("Cleanup-Origin", {{"llamacpp_args", arguments}});
        check(rejected([&] { models.cleanup_orphaned_cache(false); }, "artifact_reference_unknown") && fs::exists(paths.legacy),
              "opaque or relative argument dependencies cannot authorize destructive cleanup");
    }
    models.update_saved_model_options("Cleanup-Origin", {{"llamacpp_args", nullptr}});
    TemplateEnvironment::set(paths.legacy.string());
    check(models.cleanup_orphaned_cache(true)["orphaned_files"].empty(), "the declared template environment input retains its file");
    TemplateEnvironment::set("");
    auto* config = RuntimeConfig::global();
    const auto previous = config->backend_config("thenoise");
    for (const auto* key : {"lora_dir", "upscaler_dir"}) {
        config->set({{"thenoise", {{key, paths.directory.string()}}}});
        check(models.cleanup_orphaned_cache(true)["orphaned_files"].empty(), "declared provider directories retain their contents during cleanup");
        config->set({{"thenoise", {{key, previous.value(key, std::string())}}}});
    }
    {
        TemplateEnvironment runtime("LEMONADE_LLAMACPP_CPU_BIN");
        utils::set_environment_variable_utf8("LEMONADE_LLAMACPP_CPU_BIN", paths.legacy.string());
        check(models.cleanup_orphaned_cache(true)["orphaned_files"].empty(), "native runtime path overrides retain their declared files without installing anything");
    }
}

static void shard_references(Server& server, const CacheFiles& paths) {
    auto& models = StatePublicationTestHook::models(server);
    const auto first = paths.legacy.parent_path() / "shard-00001-of-00002.gguf";
    const auto second = paths.legacy.parent_path() / "shard-00002-of-00002.gguf";
    file(first); file(second); file(paths.companion.parent_path() / second.filename());
    auto definition = paths.definition(); definition["checkpoints"]["auxiliary"] = "fixture/cleanup-companion:" + second.filename().string();
    models.register_user_model("Cleanup-Origin", definition);
    models.register_user_model("Cleanup-Sharded", paths.local(first));
    const auto result = models.cleanup_orphaned_cache(true);
    check(result["orphaned_files"].size() == 1 && result["orphaned_files"][0]["path"] == paths.legacy.string() && fs::exists(second),
          "a registered first shard retains its companion shards from unrelated legacy cleanup");
    models.unregister_user_model("Cleanup-Sharded"); models.register_user_model("Cleanup-Origin", paths.definition());
}

static void root_transition(Server& server, const CacheFiles& paths) {
    using Hook = StatePublicationTestHook; auto& models = Hook::models(server);
    const auto previous = Hook::models_dir(server); const auto next = test::make_scratch_dir("other-model-cache");
    std::promise<void> entered, release; const auto released = release.get_future().share(); std::atomic<bool> first{true};
    ModelPublicationTestHook::barrier(models, [&](const auto& phase) {
        if (phase == "cache-cleanup-root" && first.exchange(false)) { entered.set_value(); released.wait(); }
    });
    auto cleanup = std::async(std::launch::async, [&] { return models.cleanup_orphaned_cache(false); });
    const bool reached = entered.get_future().wait_for(5s) == std::future_status::ready;
    const auto changed = Hook::configure(server, {{"models_dir", next.string()}});
    release.set_value(); const auto result = cleanup.get(); ModelPublicationTestHook::barrier(models, {});
    check(reached && changed.status == 503 && Hook::models_dir(server) == previous && result["orphaned_files"].size() == 1 && !fs::exists(paths.legacy),
          "an accepted cleanup prevents a competing root change before persistence");
    file(paths.legacy); ModelPublicationTestHook::invalidate(models);
    const auto before = Hook::saved_config(server);
    fs::create_directory(Hook::config_staging(server));
    check(Hook::configure(server, {{"models_dir", next.string()}}).status == 500 && Hook::saved_config(server) == before &&
          Hook::admission(server).snapshot()["model_cache"]["root_changes"] == 0,
          "failed root persistence preserves the committed configuration and releases logical ownership");
    fs::remove(Hook::config_staging(server)); Hook::effect_failure(server, true);
    const auto effect = Hook::configure(server, {{"models_dir", next.string()}});
    check(effect.status == 503 && json::parse(effect.body)["saved"] == true && json::parse(effect.body)["applied"] == false &&
          fs::path(models.model_cache_root()) != fs::weakly_canonical(next),
          "failed root application reports saved intent without inventing an applied storage transition");
    Hook::effect_failure(server, false);
    {
        auto reference = Hook::admission(server).acquire_management("retained-cache", "model-acquisition", models.model_cache_root());
        const auto saved = Hook::saved_config(server);
        check(Hook::configure(server, {{"broadcast", false}}).status == 503 && Hook::saved_config(server) == saved &&
              fs::path(models.model_cache_root()) != fs::weakly_canonical(next),
              "an unrelated configuration retry cannot apply a pending root change across a new storage reference");
    }
    check(Hook::configure(server, {{"models_dir", next.string()}}).status == 200 && models.cleanup_orphaned_cache(true)["orphaned_files"].empty() && fs::exists(paths.legacy),
          "a cold root transition preserves the previous root and publishes its new reference namespace");
    check(Hook::configure(server, {{"models_dir", previous}}).status == 200 && models.cleanup_orphaned_cache(true)["orphaned_files"].size() == 1,
          "returning to the previous root preserves its files and reference decision");
}

static void cleanup_overlap(Server& server, const CacheFiles& paths) {
    using Hook = StatePublicationTestHook; auto& models = Hook::models(server); auto admission = Hook::admission(server);
    std::promise<void> entered, release; const auto released = release.get_future().share();
    ModelPublicationTestHook::barrier(models, [&](const auto& phase) { if (phase == "cache-cleanup") { entered.set_value(); released.wait(); } });
    auto cleanup = std::async(std::launch::async, [&] { return models.cleanup_orphaned_cache(false); });
    const bool reached = entered.get_future().wait_for(5s) == std::future_status::ready;
    auto reader = std::async(std::launch::async, [&] { return models.get_model_info("Cleanup-Origin").checkpoint(); });
    const bool readable = reader.wait_for(1s) == std::future_status::ready;
    check(reached && readable, "the shared model cache remains readable while cleanup holds its file/reference decision");
    auto registration = std::async(std::launch::async, [&] {
        try { models.register_user_model("Cleanup-Late", paths.local(paths.legacy)); return false; }
        catch (const AdmissionError& error) { return error.code == "model_cache_busy"; }
    });
    const bool reference_rejected = registration.get();
    check(reached && reference_rejected && rejected([&] { models.update_saved_model_options("Cleanup-Origin", {{"ctx_size", 4096}}); }, "model_cache_busy"),
          "new registration and option references reject before publication while cleanup owns the decision");
    const auto configuration = Hook::configure(server, {{"ctx_size", 4096}});
    check(configuration.status == 503 && json::parse(configuration.body)["error"]["details"]["code"] == "model_cache_busy" && configuration.get_header_value("Retry-After") == "1",
          "configuration reference changes preserve typed retry metadata during cleanup");
    const auto info = models.get_model_info("Cleanup-Origin");
    check(rejected([&] { Hook::router(server).load_model("Cleanup-Origin", info, RecipeOptions("llamacpp", {{"ctx_size", 2048}, {"llamacpp_backend", "cpu"}}), true); }, "model_cache_busy"),
          "a native load cannot dispatch while its cache root is being cleaned");
    admission.maintenance(true);
    check(!admission.wait_for_writes(5ms), "maintenance waits through accepted cleanup and filesystem effects");
    release.set_value(); const auto result = cleanup.get(); reader.get();
    ModelPublicationTestHook::barrier(models, {});
    check(result["orphaned_files"].size() == 1 && result["bytes_are_estimate"] == true && !fs::exists(paths.legacy) && fs::exists(paths.companion),
          "cleanup removes only the unreferenced obsolete entry and reports apparent bytes");
    check(reference_rejected && !models.model_exists("Cleanup-Late") && admission.wait_for_writes(5s),
          "a reference cannot commit after deletion and accepted ownership drains during maintenance");
    check(Hook::cleanup(server, {{"dry_run", false}}).status == 503, "new native cleanup rejects during maintenance");
    admission.maintenance(false);
    bool missing_rejected = false;
    try { models.register_user_model("Cleanup-Late", paths.local(paths.legacy)); } catch (const ModelRegistrationConflict&) { missing_rejected = true; }
    check(missing_rejected && !models.model_exists("Cleanup-Late"), "a retried reference cannot commit a deleted managed-cache file");
}

static void failure_retry(Server& server, const CacheFiles& paths) {
    using Hook = StatePublicationTestHook; auto& models = Hook::models(server);
    file(paths.legacy); ModelPublicationTestHook::invalidate(models);
    ModelPublicationTestHook::barrier(models, [](const auto& phase) { if (phase == "cache-cleanup-remove") throw std::runtime_error("controlled unlink-boundary failure"); });
    const auto failure = Hook::cleanup(server, {{"dry_run", false}});
    check(failure.status == 500 && json::parse(failure.body)["error"]["code"] == "cache_cleanup_failed" && fs::exists(paths.legacy),
          "unlink-boundary failure preserves the file and returns a cleanup error rather than model-not-found");
    check(Hook::admission(server).snapshot()["model_cache"]["cleanups"] == 0,
          "synchronous cleanup failure releases its storage exclusion without claiming deletion");
    ModelPublicationTestHook::barrier(models, [&](const auto& phase) { if (phase == "cache-cleanup") file(paths.legacy, "changed after selection"); });
    check(rejected([&] { models.cleanup_orphaned_cache(false); }, "artifact_reference_changed") && fs::exists(paths.legacy),
          "changed file identity prevents an obsolete deletion decision");
    ModelPublicationTestHook::barrier(models, {});
    check(models.cleanup_orphaned_cache(false)["orphaned_files"].size() == 1 && !fs::exists(paths.legacy) && fs::exists(paths.backup) && fs::exists(paths.blob),
          "retry revalidates candidates and preserves unrelated files");
    check(Hook::cleanup(server, {{"dry_run", "invalid"}}).status == 400, "invalid cleanup input remains a client error");
}

int main() {
    try {
        ledger_exclusion();
        TemplateEnvironment environment;
        auto defaults = ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        defaults["auto_evict"] = false; defaults["auto_check_model_updates"] = false; defaults["disable_model_filtering"] = true;
        auto config = std::make_shared<RuntimeConfig>(defaults); RuntimeConfig::set_global(config.get());
        const auto root = test::make_scratch_dir("model-cache-cleanup");
        {
            Server server(config, root.string(), root.string()); StatePublicationTestHook::settle(server);
            CacheFiles paths(StatePublicationTestHook::models(server));
            file_references(server, paths); option_references(server, paths); shard_references(server, paths); root_transition(server, paths);
            worker_ownership(server, paths); cleanup_overlap(server, paths); failure_retry(server, paths);
            file(paths.legacy); ModelPublicationTestHook::invalidate(StatePublicationTestHook::models(server));
            StatePublicationTestHook::retain_unknown_preparation(server);
            const auto guarded = StatePublicationTestHook::cleanup(server, {{"dry_run", false}});
            check(guarded.status == 409 && json::parse(guarded.body)["error"]["code"] == "preparation_cleanup_pending" && fs::exists(paths.legacy),
                  "unresolved prior preparation protects cache-wide cleanup even after source registration changes");
        }
        {
            Server restarted(config, root.string(), root.string()); StatePublicationTestHook::settle(restarted);
            check(StatePublicationTestHook::models(restarted).cleanup_orphaned_cache(true)["orphaned_files"].size() == 1 && StatePublicationTestHook::models(restarted).is_model_downloaded("Cleanup-Origin") &&
                  StatePublicationTestHook::cleanup(restarted, {{"dry_run", false}}).status == 409,
                  "manager restart preserves registration, useful companions and prior-writer cleanup quarantine");
        }
        RuntimeConfig::set_global(nullptr);
    } catch (const std::exception& error) { ++failures; std::fprintf(stderr, "Unexpected fixture failure: %s\n", error.what()); }
    std::fprintf(stderr, "Model cache cleanup: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
