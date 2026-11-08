#include "lemon/cloud_provider_registry.h"
#include "lemon/config_file.h"
#include "lemon/jobs/job_manager.h"
#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/runtime_config.h"
#include "lemon/server.h"
#include "lemon/utils/json_utils.h"
#include "lemon/utils/path_utils.h"
#include "support/test_state_isolation.h"

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <functional>
#include <future>
#include <thread>

#ifdef _WIN32
#include <windows.h>
#endif

namespace lemon {
struct ModelPublicationTestHook {
    static void barrier(ModelManager& models, std::function<void()> callback) {
        models.profile_persistence_barrier_ = std::move(callback);
    }
};
class PolicyWorker : public WrappedServer {
public:
    explicit PolicyWorker(const std::string& name) : WrappedServer("policy-fixture", "error") {
        set_model_metadata(name, "fixture", ModelType::LLM, DEVICE_CPU, RecipeOptions("llamacpp", {{"ctx_size", 2048}}));
        set_state(ModelState::READY);
    }
    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override {}
    void unload() override { set_state(ModelState::UNLOADED); }
    bool is_backend_alive() const override { return get_state() != ModelState::UNLOADED; }
    json chat_completion(const json&) override { return json::object(); }
};
struct StatePublicationTestHook {
    static void add_worker(Server& server, const std::string& name) {
        auto worker = std::make_shared<PolicyWorker>(name);
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_);
        server.router_->loaded_servers_.push_back(worker);
    }
    static bool pinned(Server& server, const std::string& name) {
        const auto public_name = server.model_manager_->get_public_model_name(name);
        for (const auto& row : server.router_->get_all_loaded_models())
            if (row.at("model_name") == public_name) return row.value("pinned", false);
        return false;
    }
    static json options(Server& server, const std::string& name) { return server.model_manager_->get_saved_model_options(name); }

    static void register_model(Server& server, const std::string& name) {
        server.model_manager_->register_user_model(name, {{"recipe", "llamacpp"}, {"checkpoint", "fixture/repo:Q4_K_M"}});
    }
    static json state(Server& server) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        return server.juiced_->state_;
    }
    static bool allows(Server& server, const std::string& name) { return server.juiced_->allows(name); }
    static httplib::Response cache(Server& server, const json& body) {
        httplib::Request req; req.method = "POST"; req.path = "/v1/juiced/cache"; req.body = body.dump();
        httplib::Response response; server.juiced_->handle(req, response, "cache"); return response;
    }
    static httplib::Response mutate(Server& server, const json& body, bool pin = false) {
        httplib::Request req; req.method = "POST"; req.path = pin ? "/internal/pin" : "/v1/juiced/policy"; req.body = body.dump();
        httplib::Response response;
        if (pin) server.handle_pin(req, response);
        else server.juiced_->handle(req, response, "policy");
        return response;
    }
};
}

using json = nlohmann::json;
namespace fs = std::filesystem;
static int checks = 0, failures = 0;
static void check(bool ok, const char* message) {
    ++checks;
    if (!ok) ++failures;
    std::fprintf(stderr, "%s: %s\n", ok ? "PASS" : "FAIL", message);
}
static json read(const fs::path& path) { std::ifstream f(path); return json::parse(f); }
static void write(const fs::path& path, const json& body) { std::ofstream f(path); f << body.dump(); }

class BlockReplacement {
public:
    explicit BlockReplacement(const fs::path& target) : parent_(target.parent_path()) {
#ifdef _WIN32
        handle_ = CreateFileW(target.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE,
            nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (handle_ == INVALID_HANDLE_VALUE) throw std::runtime_error("Could not hold replacement fixture");
#else
        permissions_ = fs::status(parent_).permissions();
        fs::permissions(parent_, fs::perms::owner_read | fs::perms::owner_exec);
#endif
    }
    ~BlockReplacement() {
#ifdef _WIN32
        CloseHandle(handle_);
#else
        fs::permissions(parent_, permissions_);
#endif
    }
private:
    fs::path parent_;
#ifdef _WIN32
    HANDLE handle_;
#else
    fs::perms permissions_;
#endif
};

static void cloud_failures() {
    for (const auto& failure : {"staging", "replacement"}) for (int operation = 0; operation < 4; ++operation) {
        const auto root = lemon::test::make_scratch_dir("cloud");
        lemon::CloudProviderRegistry registry;
        registry.install("fixture", "http://localhost.invalid/v1");
        registry.set_runtime_key("fixture", "process-only-fixture");
        lemon::RuntimeConfig runtime(lemon::ConfigFile::base_defaults());
        auto persist = [&](const json& metadata) {
            runtime.set_persisted({{"cloud_providers", metadata}}, [&](const json& change) {
                auto disk = lemon::ConfigFile::load_raw(root.string());
                lemon::ConfigFile::save(root.string(), lemon::utils::JsonUtils::merge(disk, change));
            });
        };
        persist(registry.to_config_array());
        const auto before = registry.to_config_array(), disk = read(root / "config.json"), config = runtime.snapshot();
        auto mutate = [&] {
            if (operation == 0) registry.install("new", "https://localhost.invalid/v2", {}, persist);
            if (operation == 1) registry.install("fixture", "https://localhost.invalid/v2", {}, persist);
            if (operation == 2) registry.set_allow_insecure_http("fixture", true, persist);
            if (operation == 3) registry.uninstall("fixture", persist);
        };
        bool rejected = false;
        if (std::string(failure) == "staging") {
            fs::create_directory(root / "config.json.tmp");
            write(root / "config.json.tmp" / "block", json::object());
            try { mutate(); } catch (const std::exception&) { rejected = true; }
            fs::remove_all(root / "config.json.tmp");
        } else {
            write(root / "config.json.tmp", json::object());
            BlockReplacement block(root / "config.json");
            try { mutate(); } catch (const std::exception&) { rejected = true; }
        }
        check(rejected && registry.to_config_array() == before && runtime.snapshot() == config && read(root / "config.json") == disk,
              "failed cloud publication preserves registry, runtime and disk for every mutation");
        check(registry.resolve_key("fixture") == "process-only-fixture", "failed cloud mutation preserves process credentials");
        mutate();
        const auto durable = read(root / "config.json").at("cloud_providers");
        lemon::CloudProviderRegistry restarted; restarted.load_from_config(durable);
        check(registry.to_config_array() == durable && runtime.snapshot().at("cloud_providers") == durable && restarted.to_config_array() == durable,
              "cloud retry and restart agree on complete metadata");
        check(read(root / "config.json").dump().find("process-only-fixture") == std::string::npos &&
              (operation != 3 || registry.resolve_key("fixture").empty()), "credentials never persist and uninstall clears them after commit");
    }
}

static void retained_jobs() {
    for (const auto& status : {"completed", "interrupted"}) for (bool staging : {false, true}) {
        auto root = lemon::test::make_scratch_dir("jobs");
        auto target = root / "jobs.json";
        write(target, {{"version", 1}, {"jobs", json::array({{{"id", "retained-1"}, {"status", status}}})}});
        {
            lemon::jobs::JobManager jobs(root.string(), root.string(), {});
            auto before = jobs.get("retained-1"); const auto disk = read(target);
            bool active = false, rejected = false;
            if (staging) {
                fs::create_directory(root / "jobs.json.tmp");
                try { jobs.remove("retained-1", active); } catch (const lemon::jobs::JobError& e) { rejected = e.status == 500; }
                fs::remove(root / "jobs.json.tmp");
            } else {
                write(root / "jobs.json.tmp", json::object());
                BlockReplacement block(target);
                try { jobs.remove("retained-1", active); } catch (const lemon::jobs::JobError& e) { rejected = e.status == 500; }
            }
            check(rejected && jobs.get("retained-1") == before && read(target) == disk,
                  "failed retained deletion or tombstone preserves memory and committed record");
            check(jobs.remove("retained-1", active), "failed retained deletion remains retryable");
        }
        lemon::jobs::JobManager restarted(root.string(), root.string(), {});
        check(!restarted.get("retained-1"), "durable deletion does not reappear after restart");
    }
}

static void model_files() {
    auto defaults = lemon::ConfigFile::base_defaults();
    defaults["offline"] = true; defaults["auto_check_model_updates"] = false;
    lemon::RuntimeConfig runtime(defaults); lemon::RuntimeConfig::set_global(&runtime);
    {
        lemon::ModelManager models;
        const json definition = {{"checkpoint", "fixture/repo:Q4_K_M"}, {"recipe", "llamacpp"}};
        models.register_user_model("user.Persistence", definition);
        auto info = models.get_model_info("user.Persistence");
        models.save_model_options(info);
        models.select_execution_profile("user.Persistence", {{"id", "registered"}});
        const auto root = lemon::utils::get_config_dir();
        for (const auto& name : {"user_models.json", "recipe_options.json", "execution_profiles.json"}) {
            auto target = fs::path(root) / name;
            const auto before = read(target);
            const auto before_options = models.get_saved_model_options("user.Persistence");
            bool rejected = false;
            {
                BlockReplacement block(target);
                try {
                    if (std::string(name) == "user_models.json") models.register_user_model("user.Persistence2", definition);
                    if (std::string(name) == "recipe_options.json") {
                        auto changed = info;
                        changed.recipe_options = lemon::RecipeOptions("llamacpp", {{"ctx_size", 4096}});
                        models.save_model_options(changed);
                    }
                    if (std::string(name) == "execution_profiles.json") models.select_execution_profile("user.Persistence", {{"id", "registered"}});
                } catch (const std::exception&) { rejected = true; }
            }
            check(rejected && read(target) == before, "model registration/options/profile write failure preserves complete committed file");
            check(models.get_saved_model_options("user.Persistence") == before_options, "failed model-file publication preserves visible saved options");
        }
        lemon::ModelManager restarted;
        check(restarted.model_exists("user.Persistence") && !restarted.model_exists("user.Persistence2"), "model registration restart preserves only committed registrations");
        check(restarted.get_saved_model_options("user.Persistence") == models.get_saved_model_options("user.Persistence") &&
              restarted.get_execution_profiles("user.Persistence") == models.get_execution_profiles("user.Persistence"), "saved options and profiles retain restart readback");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void profile_concurrency() {
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true;
    lemon::RuntimeConfig runtime(defaults); lemon::RuntimeConfig::set_global(&runtime);
    {
        lemon::ModelManager models;
        const auto before = models.get_execution_profiles("user.Persistence");
        const auto previous = models.get_model_info("user.Persistence");
        std::promise<void> entered, release;
        auto released = release.get_future();
        lemon::ModelPublicationTestHook::barrier(models, [&] {
            entered.set_value(); released.wait(); throw std::runtime_error("controlled persistence failure");
        });
        auto selection = std::async(std::launch::async, [&] {
            try { models.select_execution_profile("user.Persistence", {{"id", "changed"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 4096}}}}); }
            catch (const std::runtime_error&) { return true; }
            return false;
        });
        entered.get_future().wait();
        check(models.get_execution_profiles("user.Persistence") == before && models.get_model_info("user.Persistence").recipe == previous.recipe,
              "blocked changed profile stays private while concurrent readers retain the committed recipe");
        auto options = std::async(std::launch::async, [&] { return models.update_saved_model_options("user.Persistence", {{"ctx_size", 8192}}); });
        release.set_value();
        check(selection.get(), "controlled profile publication failure returns an error");
        options.get();
        lemon::ModelPublicationTestHook::barrier(models, {});
        lemon::ModelManager restarted;
        check(models.get_execution_profiles("user.Persistence") == before && models.get_saved_model_options("user.Persistence").at("ctx_size") == 8192 &&
              restarted.get_saved_model_options("user.Persistence") == models.get_saved_model_options("user.Persistence"),
              "failed profile selection cannot overwrite a concurrent successful option update or restart state");
        models.select_execution_profile("user.Persistence", {{"id", "changed"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 4096}}}});
        check(models.get_model_info("user.Persistence").recipe == "vllm" && models.get_model_info("user.Persistence").recipe_options.get_option("ctx_size") == 4096,
              "successful changed profile publishes its recipe with matching options");
        const auto config_root = fs::path(lemon::utils::get_config_dir());
        const auto options_path = config_root / "recipe_options.json";
        const auto profiles_path = config_root / "execution_profiles.json";
        const auto committed_profiles = read(profiles_path);
        fs::rename(options_path, config_root / "recipe_options.backup");
        fs::create_directory(options_path);
        bool second_failed = false;
        try { models.select_execution_profile("user.Persistence", {{"id", "registered"}}); }
        catch (const std::exception&) { second_failed = true; }
        fs::remove(options_path);
        fs::rename(config_root / "recipe_options.backup", options_path);
        check(second_failed && read(profiles_path) == committed_profiles && models.get_model_info("user.Persistence").recipe == "vllm" &&
              models.get_model_info("user.Persistence").recipe_options.get_option("ctx_size") == 4096,
              "second-file publication failure restores the profile file without exposing a mixed selection");
        lemon::ModelManager accepted;
        check(accepted.get_model_info("user.Persistence").recipe == "vllm" && accepted.get_model_info("user.Persistence").recipe_options.get_option("ctx_size") == 4096,
              "changed profile and matching options survive restart together");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void policy_failures() {
    for (bool pin : {false, true}) for (bool staging : {false, true}) {
        const auto root = lemon::test::make_scratch_dir("policy");
        const auto name = "user.Policy" + root.filename().string();
        auto defaults = lemon::ConfigFile::base_defaults();
        defaults["offline"] = true; defaults["broadcast"] = false; defaults["auto_check_model_updates"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        json committed;
        {
            lemon::Server server(runtime, root.string(), root.string());
            lemon::StatePublicationTestHook::register_model(server, name);
            auto initial = lemon::StatePublicationTestHook::mutate(server, {{"model", name}, {"policy", pin ? "on-demand" : "disabled"}});
            if (pin) lemon::StatePublicationTestHook::add_worker(server, name);
            const auto before_options = lemon::StatePublicationTestHook::options(server, name);
            check(initial.status == 200, "initial policy is committed through the native handler");
            const auto before = lemon::StatePublicationTestHook::state(server);
            auto mutate = [&] { return lemon::StatePublicationTestHook::mutate(server,
                pin ? json{{"model", name}, {"pinned", true}} : json{{"model", name}, {"policy", "hot"}}, pin); };
            httplib::Response rejected;
            const auto target = root / "juiced/state.json";
            if (staging) {
                fs::create_directory(root / "juiced/state.json.tmp");
                rejected = mutate();
                fs::remove(root / "juiced/state.json.tmp");
            } else {
                write(root / "juiced/state.json.tmp", json::object());
                BlockReplacement block(target);
                rejected = mutate();
            }
            check(rejected.status == 500 && lemon::StatePublicationTestHook::state(server) == before && read(target) == before,
                  "failed policy and pin persistence preserves policy, revision and committed bytes");
            check(lemon::StatePublicationTestHook::allows(server, name) == pin, "failed policy or pin update preserves inference eligibility");
            check(!lemon::StatePublicationTestHook::pinned(server, name) && lemon::StatePublicationTestHook::options(server, name) == before_options,
                  "failed policy or pin update preserves live pin effects and saved options");
            fs::remove(root / "juiced/state.json.tmp");
            const auto before_cache = lemon::StatePublicationTestHook::state(server);
            fs::create_directory(root / "juiced/state.json.tmp");
            const auto cache_failure = lemon::StatePublicationTestHook::cache(server, {{"retention_days", 14}, {"disk_budget_gib", 16}});
            fs::remove(root / "juiced/state.json.tmp");
            check(cache_failure.status == 500 && lemon::StatePublicationTestHook::state(server) == before_cache,
                  "shared state-save failure restores rejected cache controls as well as policy");
            auto accepted = mutate();
            check(accepted.status == 200 && lemon::StatePublicationTestHook::allows(server, name), "rejected policy and pin changes remain retryable");
            check(!pin || lemon::StatePublicationTestHook::pinned(server, name), "successful pin publication updates the existing worker");
            committed = lemon::StatePublicationTestHook::state(server);
        }
        {
            lemon::Server restarted(runtime, root.string(), root.string());
            check(lemon::StatePublicationTestHook::state(restarted) == committed && lemon::StatePublicationTestHook::allows(restarted, name),
                  "successful policy and pin publication retains revision and eligibility after restart");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
}

int main() {
    cloud_failures(); retained_jobs(); model_files(); profile_concurrency(); policy_failures();
    std::fprintf(stderr, "Persistence boundaries: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
