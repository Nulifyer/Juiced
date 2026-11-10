#include "lemon/alias_manager.h"
#include "lemon/config_file.h"
#include "lemon/model_manager.h"
#include "lemon/resource_admission.h"
#include "lemon/router.h"
#include "lemon/runtime_config.h"
#include "lemon/server.h"
#include "support/test_state_isolation.h"

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <functional>
#include <future>
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
    static ResourceAdmission admission(Server& server) { return server.router_->admission_view(); }
    static void effects(Server& server, std::function<void()> callback) { server.config_effect_barrier_ = std::move(callback); }
    static json publish(Server& server, const json& changes) { return server.publish_config([&] { return changes; }); }
    static bool effects_complete(Server& server) { return server.pending_config_effects_.empty(); }
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

int main() {
    configuration_aliases(); model_publication(); server_effect_lifetime();
    std::fprintf(stderr, "Writer operations: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
