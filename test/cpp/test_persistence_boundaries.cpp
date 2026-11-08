#include "lemon/cloud_provider_registry.h"
#include "lemon/config_file.h"
#include "lemon/jobs/job_manager.h"
#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/mcp_server.h"
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
    static void read_barrier(ModelManager& models, std::function<void()> callback) { models.cache_read_barrier_ = std::move(callback); }
    static void write_barrier(ModelManager& models, std::function<void()> callback) { models.options_write_barrier_ = std::move(callback); }
    static void downloaded(ModelManager& models, const std::string& name) {
        const auto key = models.resolve_model_name(name);
        models.get_model_info(name);
        std::lock_guard<std::mutex> lock(models.models_cache_mutex_);
        models.models_cache_.at(key).downloaded = true;
    }
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
    static void stop(Server& server) { server.juiced_->stop(); }
    static ModelManager& models(Server& server) { return *server.model_manager_; }
    static void effect_barrier(Server& server, std::function<void()> callback) { server.juiced_->policy_effect_barrier_ = std::move(callback); }
    static void reconcile_barrier(Server& server, std::function<void()> callback) { server.juiced_->reconciliation_barrier_ = std::move(callback); }
    static void refresh(Server& server) { server.juiced_->refresh(); }
    static void reconcile(Server& server) { server.juiced_->reconcile(); }
    static json take_command(Server& server) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        if (server.juiced_->pending_.empty()) return json::object();
        auto cmd = server.juiced_->pending_.front(); server.juiced_->pending_.pop_front(); return cmd;
    }
    static void execute(Server& server, const json& command) { server.juiced_->execute(command); }
    static uint64_t generation(Server& server, const std::string& name) { return server.router_->get_model_generation(name); }
    static void stale_pin(Server& server, const std::string& name, uint64_t revision, uint64_t generation) {
        std::lock_guard<std::mutex> effects(server.juiced_->policy_effect_mutex_);
        server.juiced_->apply_policy_pin_locked(name, revision, generation);
    }
    static void add_worker(Server& server, const std::string& name, bool replace = false) {
        auto worker = std::make_shared<PolicyWorker>(name);
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_);
        if (replace) {
            auto& workers = server.router_->loaded_servers_;
            workers.erase(std::remove_if(workers.begin(), workers.end(), [&](const auto& old) { return old->get_model_name() == name; }), workers.end());
        }
        server.router_->loaded_servers_.push_back(worker);
    }
    static httplib::Response list(Server& server, bool all) {
        httplib::Request request; request.method = "GET"; request.path = "/v1/models";
        if (all) request.params.emplace("show_all", "true");
        httplib::Response response; response.status = 200; server.handle_models(request, response); return response;
    }
    static json mcp(Server& server) {
        McpServer mcp(server.router_.get(), server.model_manager_.get(), [](const std::string&) {},
                      [&](const std::string& name) { return server.juiced_->allows(name); }, true);
        return json::parse(mcp.handle_request_body(json{{"jsonrpc", "2.0"}, {"id", 1}, {"method", "tools/call"},
            {"params", {{"name", "lemonade_list_models"}, {"arguments", {{"include_suggested", false}}}}}}.dump()));
    }
    static void seed_snapshots(Server& server) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        for (const auto& id : {"first", "second"}) {
            std::ofstream file(server.juiced_->snapshot_directory_ / (std::string(id) + ".bin")); file << "saved computation";
            server.juiced_->state_["snapshots"][id] = {{"session", "fixture-session"}, {"retained", true}, {"bytes", 17}, {"last_used", 4102444800LL}};
        }
        server.juiced_->persist_locked();
    }
    static void cleanup(Server& server) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_); server.juiced_->cleanup_snapshots_locked();
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
    static httplib::Response cache(Server& server, const json& body, const std::string& operation = "cache") {
        httplib::Request req; req.method = "POST"; req.path = "/v1/juiced/cache"; req.body = body.dump();
        httplib::Response response; server.juiced_->handle(req, response, operation); return response;
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

static void successful_profile_overlap() {
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true;
    lemon::RuntimeConfig runtime(defaults); lemon::RuntimeConfig::set_global(&runtime);
    {
        lemon::ModelManager models;
        models.register_user_model("user.Persistence", {{"recipe", "llamacpp"}, {"checkpoint", "fixture/repo:Q4_K_M"}});
        models.select_execution_profile("user.Persistence", {{"id", "registered"}});
        models.update_saved_model_options("user.Persistence", {{"ctx_size", 8192}, {"llamacpp_backend", "vulkan"}});
        for (bool list : {false, true}) {
            models.select_execution_profile("user.Persistence", {{"id", "changed"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 4096}}}});
            std::promise<void> entered, release; auto released = release.get_future();
            std::thread::id reader_id; std::atomic<bool> first{true};
            lemon::ModelPublicationTestHook::read_barrier(models, [&] {
                if (std::this_thread::get_id() == reader_id && first.exchange(false)) { entered.set_value(); released.wait(); }
            });
            auto reader = std::async(std::launch::async, [&] {
                reader_id = std::this_thread::get_id();
                return list ? models.get_supported_models().at(models.get_public_model_name("user.Persistence")) : models.get_model_info("user.Persistence");
            });
            entered.get_future().wait();
            models.select_execution_profile("user.Persistence", {{"id", "registered"}});
            release.set_value(); const auto observed = reader.get();
            lemon::ModelPublicationTestHook::read_barrier(models, {});
            check(observed.recipe == "llamacpp" && observed.recipe_options.get_option("ctx_size") == 8192 &&
                  observed.recipe_options.get_option("llamacpp_backend") == "vulkan",
                  "lookup and supported listing recheck successful profile commits across the cache/read gap");
        }
        models.select_execution_profile("user.Persistence", {{"id", "changed"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 4096}}}});
        auto stale = models.get_model_info("user.Persistence");
        std::promise<void> entered, release; auto released = release.get_future();
        lemon::ModelPublicationTestHook::write_barrier(models, [&] { entered.set_value(); released.wait(); });
        auto writer = std::async(std::launch::async, [&] { return models.update_saved_model_options("user.Persistence", {{"ctx_size", 6144}}); });
        entered.get_future().wait();
        models.select_execution_profile("user.Persistence", {{"id", "registered"}});
        const auto rebuilt = models.get_model_info("user.Persistence");
        release.set_value(); writer.get(); lemon::ModelPublicationTestHook::write_barrier(models, {});
        const auto committed = models.get_model_info("user.Persistence");
        check(rebuilt.recipe == "llamacpp" && committed.recipe == "llamacpp" && committed.recipe_options.get_option("ctx_size") == 6144 &&
              committed.recipe_options.get_option("llamacpp_backend") == "vulkan", "queued option writer interprets current committed recipe after successful selection and rebuild");
        bool rejected = false; try { models.save_model_options(stale); } catch (const std::invalid_argument&) { rejected = true; }
        check(rejected && models.get_model_info("user.Persistence").recipe_options.to_json() == committed.recipe_options.to_json(),
              "stale full-option publication rejects instead of overwriting another recipe");
        bool stale_validation = false;
        try { models.update_saved_model_options("user.Persistence", {{"ctx_size", 2048}}, "vllm"); }
        catch (const std::invalid_argument&) { stale_validation = true; }
        check(stale_validation && models.get_model_info("user.Persistence").recipe_options.to_json() == committed.recipe_options.to_json(),
              "options validated for another recipe reject before durable or cached publication");
        lemon::ModelManager restarted;
        check(restarted.get_model_info("user.Persistence").recipe_options.to_json() == committed.recipe_options.to_json() &&
              restarted.get_execution_profiles("user.Persistence") == models.get_execution_profiles("user.Persistence"),
              "successful profile/options overlap agrees after restart");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void profile_discovery() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("profile-discovery");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    for (bool restart : {false, true}) {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        const std::string name = "user.DiscoveryProfile";
        auto& models = Hook::models(server);
        if (!restart) {
            Hook::register_model(server, name);
            models.select_execution_profile(name, {{"id", "alternate"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 4096}}}});
            Hook::mutate(server, {{"model", name}, {"policy", "on-demand"}});
        }
        const auto public_name = models.get_public_model_name(name);
        lemon::ModelPublicationTestHook::downloaded(models, name);
        check(models.get_model_info(name).recipe == "vllm" && models.get_supported_models().at(public_name).recipe == "vllm" &&
              models.get_downloaded_models().at(public_name).recipe == "vllm", "direct, supported and downloaded projections agree on the selected recipe before/after restart");
        for (bool all : {false, true}) {
            const auto response = Hook::list(server, all); bool found = false;
            const auto listing = json::parse(response.body);
            for (const auto& row : listing.at("data")) if (row.at("id") == public_name) found = row.at("recipe") == "vllm";
            check(response.status == 200 && found, "ordinary and show_all model APIs retain selected recipe and proper public name");
        }
        const auto reply = Hook::mcp(server);
        const auto payload = json::parse(reply.at("result").at("content").at(1).at("text").get<std::string>());
        bool found = false;
        for (const auto& row : payload.at("available")) if (row.at("model_name") == public_name) found = row.at("recipe") == "vllm";
        check(found, "MCP downloaded discovery uses the same committed selected recipe");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void policy_ordering() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("policy-order");
    const std::string name = "user.PolicyOrdering";
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server); Hook::register_model(server, name); Hook::add_worker(server, name);
        const auto public_name = Hook::models(server).get_public_model_name(name);
        Hook::mutate(server, {{"model", name}, {"policy", "hot"}});
        std::promise<void> entered, release, newer_started; auto released = release.get_future();
        std::thread::id old_id;
        Hook::effect_barrier(server, [&] { if (std::this_thread::get_id() == old_id) { entered.set_value(); released.wait(); } });
        auto older = std::async(std::launch::async, [&] { old_id = std::this_thread::get_id(); return Hook::mutate(server, {{"model", name}, {"policy", "on-demand"}}); });
        entered.get_future().wait();
        const auto stale_revision = Hook::state(server).at("policy_revisions").at(public_name).get<uint64_t>();
        const auto old_generation = Hook::generation(server, name);
        auto newer = std::async(std::launch::async, [&] { newer_started.set_value(); return Hook::mutate(server, {{"model", name}, {"pinned", true}}, true); });
        newer_started.get_future().wait(); release.set_value();
        check(older.get().status == 200 && newer.get().status == 200 && Hook::pinned(server, name),
              "overlapping successful policy and legacy-pin updates apply effects in commit order");
        Hook::effect_barrier(server, {});
        Hook::stale_pin(server, name, stale_revision, old_generation);
        check(Hook::pinned(server, name), "older revision cannot unpin the latest committed Hot worker");
        Hook::mutate(server, {{"model", name}, {"policy", "disabled"}}); Hook::refresh(server);
        std::promise<void> snapshot, continue_reconcile; auto continued = continue_reconcile.get_future();
        Hook::reconcile_barrier(server, [&] { snapshot.set_value(); continued.wait(); });
        auto reconciliation = std::async(std::launch::async, [&] { Hook::reconcile(server); });
        snapshot.get_future().wait(); Hook::mutate(server, {{"model", name}, {"policy", "hot"}}); continue_reconcile.set_value(); reconciliation.get();
        Hook::reconcile_barrier(server, {});
        check(Hook::take_command(server).empty() && Hook::pinned(server, name), "stale Disabled reconciliation cannot enqueue unload after newer Hot intent");
        Hook::mutate(server, {{"model", name}, {"policy", "disabled"}}); Hook::refresh(server); Hook::reconcile(server);
        const auto command = Hook::take_command(server);
        check(command.value("action", "") == "unload", "current Disabled reconciliation queues a revision-bound unload");
        Hook::mutate(server, {{"model", name}, {"policy", "hot"}}); Hook::execute(server, command);
        check(Hook::generation(server, name) == old_generation && Hook::pinned(server, name), "queued Disabled unload becomes obsolete after a newer Hot commit");
        Hook::add_worker(server, name, true);
        const auto replacement = Hook::generation(server, name);
        Hook::mutate(server, {{"model", name}, {"policy", "hot"}});
        const auto current_revision = Hook::state(server).at("policy_revisions").at(public_name).get<uint64_t>();
        Hook::stale_pin(server, name, current_revision, old_generation);
        check(replacement != old_generation && Hook::pinned(server, name), "generation-bound policy effect does not touch a replacement worker");
        Hook::mutate(server, {{"model", name}, {"policy", "disabled"}}); Hook::execute(server, command);
        check(Hook::generation(server, name) == replacement, "obsolete queued revision cannot unload a replacement generation");
        Hook::refresh(server); Hook::reconcile(server);
        const auto same_revision = Hook::take_command(server);
        Hook::add_worker(server, name, true);
        const auto newest = Hook::generation(server, name);
        Hook::execute(server, same_revision);
        check(same_revision.value("action", "") == "unload" && newest != replacement && Hook::generation(server, name) == newest,
              "same-policy queued unload validates its exact generation before touching a replacement");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void cache_forget_failures() {
    using Hook = lemon::StatePublicationTestHook;
    for (bool staging : {false, true}) {
        const auto root = lemon::test::make_scratch_dir("cache-forget");
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server); Hook::seed_snapshots(server);
            const auto before = Hook::state(server); const auto target = root / "juiced/state.json";
            httplib::Response rejected;
            if (staging) {
                fs::create_directory(root / "juiced/state.json.tmp");
                rejected = Hook::cache(server, {{"session", "fixture-session"}}, "cache/forget");
                fs::remove(root / "juiced/state.json.tmp");
            } else {
                write(root / "juiced/state.json.tmp", json::object()); BlockReplacement block(target);
                rejected = Hook::cache(server, {{"session", "fixture-session"}}, "cache/forget");
            }
            check(rejected.status == 500 && Hook::state(server) == before && read(target) == before &&
                  fs::exists(root / "juiced/snapshots/first.bin") && fs::exists(root / "juiced/snapshots/second.bin"),
                  "failed multi-snapshot forget preserves metadata, revision and every saved computation file");
            fs::remove(root / "juiced/state.json.tmp");
            auto accepted = Hook::cache(server, {{"session", "fixture-session"}}, "cache/forget");
            check(accepted.status == 200 && Hook::state(server).at("snapshots").empty() &&
                  !fs::exists(root / "juiced/snapshots/first.bin") && !fs::exists(root / "juiced/snapshots/second.bin"),
                  "forget retry commits removal before deleting all selected files");
        }
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            check(Hook::state(server).at("snapshots").empty(), "forgotten computation does not reappear after restart");
            Hook::seed_snapshots(server);
            for (const auto& id : {"first", "second"}) {
                const auto file = root / "juiced/snapshots" / (std::string(id) + ".bin"); fs::remove(file); fs::create_directory(file); write(file / "block", json::object());
            }
            const auto accepted = Hook::cache(server, {{"session", "fixture-session"}}, "cache/forget");
            check(accepted.status == 200 && Hook::state(server).at("snapshots").empty() && Hook::state(server).at("snapshot_cleanup").size() == 2,
                  "post-commit filesystem failure keeps durable retryable cleanup without advertising forgotten snapshots");
        }
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            check(Hook::state(server).at("snapshot_cleanup").size() == 2, "cleanup tombstones survive restart");
            fs::remove_all(root / "juiced/snapshots/first.bin"); fs::remove_all(root / "juiced/snapshots/second.bin");
            const auto retry = Hook::cache(server, {{"session", "fixture-session"}}, "cache/forget");
            check(retry.status == 200 && Hook::state(server).at("snapshot_cleanup").empty(), "committed forget remains retryable after filesystem recovery");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
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

int main(int argc, char** argv) {
    if (argc > 1 && std::string(argv[1]) == "--ordering") {
        successful_profile_overlap(); profile_discovery(); policy_ordering(); cache_forget_failures();
    } else {
        cloud_failures(); retained_jobs(); model_files(); profile_concurrency(); policy_failures();
    }
    std::fprintf(stderr, "Persistence boundaries: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
