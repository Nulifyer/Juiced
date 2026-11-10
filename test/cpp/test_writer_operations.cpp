#include "lemon/alias_manager.h"
#include "lemon/config_file.h"
#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/resource_admission.h"
#include "lemon/router.h"
#include "lemon/runtime_config.h"
#include "lemon/server.h"
#include "support/test_state_isolation.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <functional>
#include <future>
#include <sstream>
#include <stdexcept>

using namespace lemon;
using namespace std::chrono_literals;
namespace fs = std::filesystem;
static int checks = 0, failures = 0;
static void check(bool ok, const char* label) {
    ++checks;
    std::fprintf(stderr, "%s: %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) ++failures;
}
template<class Action> static bool closed(Action action) {
    try { action(); return false; }
    catch (const AdmissionError& error) { return error.status == 503 && error.code == "maintenance_active"; }
}
static json read(const fs::path& file) { std::ifstream input(file); json value; input >> value; return value; }
namespace lemon {
struct AliasPublicationTestHook {
    static void fail(AliasManager& aliases, bool fail) {
        aliases.persist_override_ = fail ? [](const auto&, const auto&) { throw std::runtime_error("controlled alias persistence failure"); }
                                        : std::function<void(const std::string&, const std::unordered_map<std::string, std::string>&)>{};
    }
};
struct ModelPublicationTestHook {
    static fs::path options_path(ModelManager& models) { return models.get_recipe_options_file(); }
    static void before_options(ModelManager& models, std::function<void()> callback) { models.options_persistence_barrier_ = std::move(callback); }
};
struct StatePublicationTestHook {
    static ModelManager& models(Server& server) { return *server.model_manager_; }
    static ResourceAdmission admission(Server& server) { return server.router_->admission_view(); }
    static void effects(Server& server, std::function<void()> callback) { server.config_effect_barrier_ = std::move(callback); }
    static json publish(Server& server, const json& changes) { return server.publish_config([&] { return changes; }); }
    static bool effects_complete(Server& server) { return server.pending_config_effects_.empty(); }
    static void finish_background(Server& server) {
        server.juiced_->stop();
        if (server.model_cache_warmup_thread_.joinable()) server.model_cache_warmup_thread_.join();
    }
    static void stream(Server& server, httplib::Response& response, std::function<void(DownloadProgressCallback)> action) {
        server.stream_download_operation(response, std::move(action));
    }
    static void start_barrier(Server& server, std::function<void(const std::string&)> callback) { server.background_start_barrier_ = std::move(callback); }
    static std::string download(Server& server, std::function<void(DownloadProgressCallback)> action) {
        auto job = server.start_download_job("writer-job", "model", "writer-job", std::move(action));
        server.join_download_job(job);
        return job->status;
    }
    static void enqueue(Server& server, const json& command) { server.juiced_->enqueue(command); }
    static bool execute_pending(Server& server) { return server.juiced_->execute_pending(); }
    static json operations(Server& server) { std::lock_guard lock(server.juiced_->mutex_); return server.juiced_->operations_; }
    static json state(Server& server) { std::lock_guard lock(server.juiced_->mutex_); return server.juiced_->state_; }
    static void import_barrier(Server& server, std::function<void(const std::string&)> callback) { server.juiced_->import_publication_barrier_ = std::move(callback); }
    static httplib::Response import(Server& server, const std::string& action, const json& body, const std::string& bytes = "") {
        httplib::Request request; request.method = "POST"; request.body = body.dump();
        if (action == "chunk") {
            request.params.emplace("id", body.at("id").get<std::string>()); request.params.emplace("offset", "0"); request.body = bytes;
        }
        httplib::Response response; server.juiced_->import_file(request, response, action); return response;
    }
    static httplib::Response api(Server& server, const std::string& action, const json& body, const std::string& method = "POST") {
        httplib::Request request; request.method = method; request.body = body.dump();
        httplib::Response response; server.juiced_->handle(request, response, action); return response;
    }
};
}

static void configuration_aliases() {
    const auto root = test::make_scratch_dir("writer-config-alias");
    AliasManager aliases(root.string());
    RuntimeConfig config(ConfigFile::base_defaults());
    ResourceAdmission admission;
    config.set_resource_admission(admission);
    aliases.set_resource_admission(admission);
    std::promise<void> committed, release;
    const auto released = release.get_future().share();
    auto update = std::async(std::launch::async, [&] {
        return config.set_persisted({{"ctx_size", 3072}}, [&](const json& changes) {
            ConfigFile::save(root.string(), changes);
        }, [&](const json&) {
            committed.set_value(); released.wait();
            std::string error;
            if (!aliases.set_alias("continued-alias", "target", error)) throw std::runtime_error(error);
        });
    });
    const bool reached = committed.get_future().wait_for(5s) == std::future_status::ready;
    admission.maintenance(true);
    check(reached && config.ctx_size() == 3072 && read(root / "config.json")["ctx_size"] == 3072,
          "configuration commits before its controlled effect callback");
    check(!admission.wait_for_writes(5ms) && admission.snapshot()["writes"]["active_operations"] == 1,
          "configuration writer ownership spans persisted state through effects");
    check(closed([&] { config.set({{"ctx_size", 4096}}); }) && config.ctx_size() == 3072,
          "maintenance rejects a fresh configuration writer without changing live settings");
    std::string error;
    check(closed([&] { aliases.set_alias("fresh-alias", "target", error); }) && !aliases.has_alias("fresh-alias"),
          "maintenance rejects a fresh alias writer before publication");
    release.set_value(); update.get();
    check(aliases.resolve_alias("continued-alias") == "target" && admission.wait_for_writes(50ms),
          "an accepted configuration operation finishes nested alias publication during drain");
    {
        std::ofstream file(root / "aliases.json"); file << "not valid JSON";
    }
    check(closed([&] { aliases.load(); }) && fs::is_regular_file(root / "aliases.json") && !fs::exists(root / "aliases.json.corrupted"),
          "maintenance closes alias reload before corruption quarantine can mutate files");
    const auto other = test::make_scratch_dir("writer-alias-other");
    { std::ofstream file(other / "aliases.json"); file << "not valid JSON"; }
    check(closed([&] { aliases.set_cache_dir(other.string()); }) && aliases.get_cache_dir() == root.string() &&
          !fs::exists(other / "aliases.json.corrupted"), "alias directory changes cannot bypass writer admission");
    admission.maintenance(false);
    aliases.load();
    check(fs::is_regular_file(root / "aliases.json.corrupted") && aliases.get_all_aliases().empty(),
          "explicit maintenance resume restores normal alias recovery");
    aliases.set_alias("original", "target", error);
    const auto before = read(root / "aliases.json");
    AliasPublicationTestHook::fail(aliases, true);
    bool failed = false;
    try { aliases.set_alias("failed", "other-target", error); } catch (const std::exception&) { failed = true; }
    check(failed && read(root / "aliases.json") == before && !aliases.has_alias("failed") && admission.wait_for_writes(50ms),
          "failed synchronous alias publication preserves state and releases writer ownership");
    AliasPublicationTestHook::fail(aliases, false);
}

static void model_publication() {
    ResourceAdmission admission;
    ModelManager models;
    models.set_resource_admission(admission);
    models.register_user_model("user.Writer-Maintenance", {{"recipe", "llamacpp"}, {"checkpoint", "fixture/repo:Q4_K_M"}});
    std::promise<void> staged, release;
    const auto released = release.get_future().share();
    ModelPublicationTestHook::before_options(models, [&] { staged.set_value(); released.wait(); });
    auto update = std::async(std::launch::async, [&] { models.update_saved_model_options("user.Writer-Maintenance", {{"ctx_size", 512}}); });
    const bool reached = staged.get_future().wait_for(5s) == std::future_status::ready;
    admission.maintenance(true);
    check(reached && !admission.wait_for_writes(5ms), "model option ownership spans its persistence and cache publication");
    check(closed([&] { models.select_execution_profile("user.Writer-Maintenance", {{"id", "registered"}}); }),
          "new profile selection rejects before waiting on an accepted options transaction");
    check(closed([&] { models.register_user_model("user.Writer-Rejected", {{"recipe", "llamacpp"}}); }),
          "new registration rejects before taking native model mutation ownership");
    release.set_value(); update.get();
    ModelPublicationTestHook::before_options(models, {});
    check(admission.wait_for_writes(50ms) && models.get_saved_model_options("user.Writer-Maintenance")["ctx_size"] == 512 &&
          models.get_model_info("user.Writer-Maintenance").recipe_options.get_option("ctx_size") == 512,
          "an accepted model write finishes coherently and releases its operation");
    admission.maintenance(false);
    const auto target = ModelPublicationTestHook::options_path(models);
    const auto before = read(target);
    const auto held = target.string() + ".held";
    fs::rename(target, held);
    fs::create_directory(target);
    { std::ofstream keep(target / "keep"); keep << "blocked replacement"; }
    bool failed = false;
    try { models.select_execution_profile("user.Writer-Maintenance", {{"id", "alternate"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 1024}}}}); }
    catch (const std::exception&) { failed = true; }
    fs::remove_all(target);
    fs::rename(held, target);
    check(failed && admission.wait_for_writes(50ms) && read(target) == before &&
          models.get_model_info("user.Writer-Maintenance").recipe == "llamacpp",
          "controlled profile failure completes synchronous rollback before writer release");
    admission.maintenance(true);
    check(closed([&] { models.unregister_user_model("user.Writer-Maintenance"); }) &&
          models.get_model_info("user.Writer-Maintenance").recipe == "llamacpp", "maintenance blocks unregistration without dropping the registration");
    admission.maintenance(false);
    models.unregister_user_model("user.Writer-Maintenance");
}

static void server_effect_lifetime() {
    const auto root = test::make_scratch_dir("writer-server-effect");
    auto settings = ConfigFile::base_defaults();
    settings["broadcast"] = false; settings["auto_check_model_updates"] = false;
    settings["offline"] = true; settings["websocket_port"] = utils::ProcessManager::find_free_port(18081);
    auto config = std::make_shared<RuntimeConfig>(settings);
    RuntimeConfig::set_global(config.get());
    {
        Server server(config, root.string(), root.string());
        auto admission = StatePublicationTestHook::admission(server);
        std::promise<void> committed, release;
        const auto released = release.get_future().share();
        StatePublicationTestHook::effects(server, [&] { committed.set_value(); released.wait(); });
        auto update = std::async(std::launch::async, [&] { return StatePublicationTestHook::publish(server, {{"global_timeout", 310}}); });
        const bool reached = committed.get_future().wait_for(5s) == std::future_status::ready;
        admission.maintenance(true);
        check(reached && config->global_timeout() == 310 && !admission.wait_for_writes(5ms),
              "the Server operation spans post-RuntimeConfig listener and binary effects");
        check(closed([&] { StatePublicationTestHook::publish(server, {{"global_timeout", 320}}); }),
              "fresh Server configuration publication rejects during maintenance");
        release.set_value(); update.get();
        check(admission.wait_for_writes(100ms) && StatePublicationTestHook::effects_complete(server),
              "Server effect completion precedes writer release");
        StatePublicationTestHook::effects(server, {});
        admission.maintenance(false);
    }
    RuntimeConfig::set_global(nullptr);
}


static void deferred_publication() {
    using Hook = StatePublicationTestHook;
    const auto root = test::make_scratch_dir("writer-deferred");
    auto settings = ConfigFile::base_defaults();
    settings["broadcast"] = false; settings["auto_check_model_updates"] = false; settings["offline"] = true;
    auto config = std::make_shared<RuntimeConfig>(settings); RuntimeConfig::set_global(config.get());
    {
        Server server(config, root.string(), root.string()); Hook::finish_background(server);
        auto admission = Hook::admission(server);
        std::atomic<int> calls{0};
        httplib::Response stream;
        Hook::stream(server, stream, [&](auto) { ++calls; Hook::publish(server, {{"global_timeout", 311}}); });
        admission.maintenance(true);
        check(calls == 0 && !admission.wait_for_writes(1ms), "an undispatched native stream retains its accepted writer");
        httplib::Response rejected;
        check(closed([&] { Hook::stream(server, rejected, [&](auto) { ++calls; }); }) && !rejected.content_provider_,
              "maintenance rejects a fresh stream before registering a content provider");
        auto exchange = std::async(std::launch::async, [&] {
            httplib::DataSink sink;
            sink.write = [](const char*, size_t) { return true; }; sink.is_writable = [] { return true; }; sink.done = [] {};
            stream.content_provider_(0, 0, sink);
        });
        exchange.get(); stream.content_provider_ = nullptr;
        check(calls == 1 && read(root / "config.json")["global_timeout"] == 311 && admission.wait_for_writes(100ms),
              "accepted SSE work enters its continuation on another thread and publishes through maintenance");
        admission.maintenance(false);
        Hook::stream(server, stream, [&](auto) { ++calls; });
        admission.maintenance(true); stream.content_provider_ = nullptr;
        check(calls == 1 && admission.wait_for_writes(100ms), "discarding a proven undispatched stream releases only its writer");
        admission.maintenance(false);

        std::promise<void> accepted, release;
        const auto released = release.get_future().share();
        Hook::start_barrier(server, [&](const auto&) { accepted.set_value(); released.wait(); });
        auto download = std::async(std::launch::async, [&] {
            return Hook::download(server, [&](auto) { ++calls; Hook::publish(server, {{"global_timeout", 312}}); });
        });
        const bool reached = accepted.get_future().wait_for(5s) == std::future_status::ready;
        admission.maintenance(true);
        check(reached && !admission.wait_for_writes(1ms) && closed([&] { Hook::download(server, [](auto) {}); }),
              "a queued download owns publication before thread dispatch and rejects a fresh job during drain");
        release.set_value(); const auto status = download.get(); Hook::start_barrier(server, {});
        check(status == "completed" && calls == 2 && read(root / "config.json")["global_timeout"] == 312 && admission.wait_for_writes(100ms),
              "accepted server-owned download completes publication after its initiating thread enters maintenance");
        admission.maintenance(false);
        Hook::start_barrier(server, [](const auto&) { throw std::runtime_error("controlled deferred dispatch failure"); });
        bool failed = false;
        try { Hook::download(server, [&](auto) { ++calls; }); } catch (const std::runtime_error&) { failed = true; }
        check(failed && calls == 2 && admission.wait_for_writes(100ms), "failed worker construction releases an undispatched continuation without invoking work");
        Hook::start_barrier(server, {});
        check(Hook::download(server, [&](auto) { ++calls; }) == "completed" && calls == 3 && admission.wait_for_writes(100ms),
              "the same native download identifier retries after dispatch failure");

        auto& models = Hook::models(server);
        const std::string model = "user.Writer-Queued";
        models.register_user_model(model, {{"recipe", "llamacpp"}, {"checkpoint", "fixture/repo:Q4_K_M"}});
        const json command = {{"model", model}, {"action", "select"},
            {"profile", {{"id", "alternate"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 640}}}}}};
        Hook::enqueue(server, command);
        admission.maintenance(true);
        check(!admission.wait_for_writes(1ms) && closed([&] { Hook::enqueue(server, command); }),
              "a queued model command retains one accepted writer and rejects a new command during maintenance");
        check(Hook::execute_pending(server) && models.get_model_info(model).recipe == "vllm" &&
              Hook::state(server)["execution_trials"].empty() && Hook::operations(server).empty() && admission.wait_for_writes(100ms),
              "queued profile selection resumes accepted ownership through journal, options and terminal publication");
        const auto before = Hook::state(server);
        const auto denied = Hook::api(server, "policy", {{"model", model}, {"policy", "hot"}});
        const auto inspected = Hook::api(server, "imports", json::object(), "GET");
        check(denied.status == 503 && denied.get_header_value("Retry-After") == "1" &&
              json::parse(denied.body)["error"]["code"] == "maintenance_active" &&
              Hook::state(server) == before && inspected.status == 200,
              "maintenance rejects state mutation before publication and preserves read-only inspection");
        admission.maintenance(false);
        Hook::enqueue(server, command); Hook::finish_background(server);
        check(admission.wait_for_writes(100ms) && Hook::operations(server)[model]["state"] == "cancelled",
              "shutdown cancels proven undispatched commands and releases their queued writer ownership");

        std::ostringstream payload;
        const auto number = [&](auto value) { payload.write(reinterpret_cast<const char*>(&value), sizeof(value)); };
        const auto string = [&](const std::string& value) { number(uint64_t(value.size())); payload << value; };
        payload << "GGUF"; number(uint32_t(3)); number(uint64_t(0)); number(uint64_t(1));
        string("general.architecture"); number(uint32_t(8)); string("llama");
        const auto started = Hook::import(server, "start", {{"filename", "writer.gguf"}, {"size", payload.str().size()}});
        if (started.status != 201) throw std::runtime_error(started.body);
        const auto id = json::parse(started.body).at("id").get<std::string>();
        const auto chunked = Hook::import(server, "chunk", {{"id", id}}, payload.str());
        if (chunked.status != 200) throw std::runtime_error(chunked.body);
        std::promise<void> registered, complete;
        const auto completed = complete.get_future().share();
        Hook::import_barrier(server, [&](const auto& phase) { if (phase == "registered") { registered.set_value(); completed.wait(); } });
        auto importing = std::async(std::launch::async, [&] { return Hook::import(server, "finish", {{"id", id}, {"name", "Writer-Import"}}); });
        const bool published = registered.get_future().wait_for(5s) == std::future_status::ready;
        admission.maintenance(true);
        const auto journal = Hook::state(server)["imports"][id];
        const auto path = journal.at("publication").at("destination").get<std::string>();
        check(published && journal["status"] == "publishing" && fs::is_regular_file(path) && !admission.wait_for_writes(1ms),
              "import ownership spans renamed file and registration through its pending final status");
        const auto cancelled = Hook::import(server, "cancel", {{"id", id}});
        check(cancelled.status == 503 && cancelled.get_header_value("Retry-After") == "1" && fs::is_regular_file(path),
              "a fresh cancellation cannot mutate an import after maintenance closes writers");
        complete.set_value(); const auto imported = importing.get(); Hook::import_barrier(server, {});
        check(imported.status == 201 && Hook::state(server)["imports"][id]["status"] == "complete" &&
              models.model_exists("Writer-Import") && admission.wait_for_writes(100ms),
              "accepted import finishes coherent registration and journal publication during maintenance");
        admission.maintenance(false);
    }
    RuntimeConfig::set_global(nullptr);
}

int main() {
    configuration_aliases(); model_publication(); server_effect_lifetime(); deferred_publication();
    std::fprintf(stderr, "Writer operations: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
