#include "lemon/cloud_provider_registry.h"
#include "lemon/config_file.h"
#include "lemon/eviction_engine.h"
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
#include <regex>

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
    void age(std::chrono::seconds age) { std::lock_guard<std::mutex> lock(state_mutex_); last_access_time_ = std::chrono::steady_clock::now() - age; }
    void unload() override { set_state(ModelState::UNLOADED); }
    bool is_backend_alive() const override { return get_state() != ModelState::UNLOADED; }
    json chat_completion(const json&) override { return json::object(); }
};
class LoadingPolicyWorker : public PolicyWorker {
public:
    LoadingPolicyWorker(const std::string& name, std::function<void()> load) : PolicyWorker(name), on_load_(std::move(load)) {}
    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override { if (on_load_) on_load_(); }
private:
    std::function<void()> on_load_;
};
struct StatePublicationTestHook {
    static void stop_eviction(Server& server) { server.router_->eviction_engine_->stop(); }
    static void factory(Server& server, std::function<void()> load) {
        server.router_->backend_factory_override_ = [load](const ModelInfo& info) { return std::make_unique<LoadingPolicyWorker>(info.model_name, load); };
    }
    static void load_entry(Server& server, std::function<void()> barrier) { server.router_->load_entry_barrier_ = std::move(barrier); }
    static httplib::Response native_load(Server& server, const std::string& name, int context = 2048) {
        httplib::Request req; req.method = "POST"; req.path = "/v1/load";
        req.body = json{{"model_name", name}, {"ctx_size", context}}.dump();
        httplib::Response res; res.status = 200; server.handle_load(req, res); return res;
    }
    static void auto_load(Server& server, const std::string& name) { server.auto_load_model_if_needed(name, {{"ctx_size", 2048}}); }
    static void unload(Server& server, const std::string& name) { server.router_->unload_model(name); }
    static void options_barrier(Server& server, std::function<void()> barrier) { server.options_response_barrier_ = std::move(barrier); }
    static httplib::Response options_response(Server& server, const std::string& name, const std::string& method, const json& body) {
        httplib::Request req; req.method = method; req.path = "/v1/models/" + name + "/options"; req.body = body.dump();
        const std::regex route("/v1/models/(.+)/options"); std::regex_match(req.path, req.matches, route);
        httplib::Response res; res.status = 200;
        if (method == "GET") server.handle_model_options_get(req, res);
        else if (method == "POST") server.handle_model_options_post(req, res);
        else server.handle_model_options_delete(req, res);
        return res;
    }
    static void downsize_barrier(Server& server, std::function<void()> barrier) { server.router_->eviction_engine_->downsize_selection_barrier_ = std::move(barrier); }
    static void evaluate(Server& server) { server.router_->eviction_engine_->evaluate_servers(-1.0); }
    static ModelState worker_state(Server& server, const std::string& name) {
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_);
        return server.router_->find_server_by_model_name(name)->get_state();
    }
    static void touch(Server& server, const std::string& name, bool age = false) {
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_);
        auto* worker = server.router_->find_server_by_model_name(name);
        if (age) dynamic_cast<PolicyWorker*>(worker)->age(std::chrono::seconds(2));
        else worker->update_access_time();
    }
    static void crash(Server& server, const std::string& name) {
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_); server.router_->find_server_by_model_name(name)->set_state(ModelState::UNLOADED);
    }
    static json intent_load(Server& server, const std::string& name, uint64_t revision) {
        try {
            auto info = server.model_manager_->get_model_info(name);
            server.router_->load_model(name, info, RecipeOptions(info.recipe, {{"ctx_size", 2048}}), true, false, true, LoadPurpose::UserInference, nullptr, revision);
            return {{"status", 200}};
        } catch (const AdmissionError& error) { return {{"status", error.status}, {"code", error.code}}; }
    }
    static json admission(Server& server) { return server.router_->admission_snapshot(); }
    static void idle_options(Server& server, const std::string& name, int idle = 0) {
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_);
        auto* worker = server.router_->find_server_by_model_name(name);
        worker->set_model_metadata(name, "fixture", ModelType::LLM, DEVICE_CPU,
            RecipeOptions("llamacpp", {{"ctx_size", 2048}, {"auto_evict", true}, {"downsize_idle_timeout", idle}, {"evict_idle_timeout", 3600}}));
    }
    static json disk(Server& server) { std::lock_guard<std::mutex> lock(server.juiced_->mutex_); return server.juiced_->cache_disk_usage_locked(); }
    static void seed_disk(Server& server, uint64_t budget, uint64_t first, uint64_t second, bool expired = false) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        server.juiced_->state_["cache"]["disk_budget_gib"] = double(budget) / (1024 * 1024 * 1024);
        server.juiced_->state_["snapshots"] = json::object(); server.juiced_->state_["snapshot_cleanup"] = json::object();
        for (const auto& item : std::vector<std::pair<std::string, uint64_t>>{{"first", first}, {"second", second}}) {
            if (!item.second) continue;
            const auto file = server.juiced_->snapshot_directory_ / (item.first + ".bin");
            { std::ofstream out(file); out << "fixture"; } std::filesystem::resize_file(file, item.second);
            server.juiced_->state_["snapshots"][item.first] = {{"session", "disk-fixture"}, {"bytes", item.second}, {"last_used", expired ? 0LL : 4102444800LL}, {"retained", false}};
        }
        server.juiced_->persist_locked();
    }
    static void prune(Server& server) { std::lock_guard<std::mutex> lock(server.juiced_->mutex_); server.juiced_->prune_cache_locked(); }
    static bool reserve_write(Server& server, const std::string& file, uint64_t bytes, uint64_t generation = 10001) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_); return server.juiced_->reserve_snapshot_write_locked(file, "disk-fixture", generation, bytes);
    }
    static json stage_publication(Server& server, bool existing) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        const auto directory = server.juiced_->snapshot_directory_;
        server.juiced_->state_["cache"]["disk_budget_gib"] = 20.0 / (1024 * 1024 * 1024);
        server.juiced_->persist_locked();
        if (existing) {
            { std::ofstream out(directory / "publication.bin"); out << "old"; }
            server.juiced_->state_["snapshots"]["publication"] = {{"bytes", 3}, {"checksum", "old-checksum"}, {"retained", true}};
            server.juiced_->persist_locked();
        }
        if (!server.juiced_->reserve_snapshot_write_locked("publication.pending", "fixture", 11, 16))
            throw std::runtime_error("Cannot reserve publication fixture");
        { std::ofstream out(directory / "publication.pending"); out << "new"; }
        server.juiced_->stage_snapshot_publication_locked("publication.pending", {
            {"id", "publication"}, {"session", "publication"}, {"bytes", 3}, {"retained", true},
            {"checksum", "11507a0e2f5e69d5dfa40a62a1bd7b6ee57e6bcd85c67c9b8431b36fff21c437"}});
        return server.juiced_->state_["snapshot_writes"].at("publication.pending");
    }
    static void publish(Server& server, const json& intent, std::function<void()> barrier) {
        server.juiced_->snapshot_publication_barrier_ = std::move(barrier);
        try { server.juiced_->publish_snapshot_write("publication.pending", intent); }
        catch (...) { server.juiced_->snapshot_publication_barrier_ = {}; throw; }
        server.juiced_->snapshot_publication_barrier_ = {};
    }
    static void recover_publications(Server& server) { server.juiced_->reconcile_snapshot_writes(); }
    static void clean_writes(Server& server) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_); server.juiced_->cleanup_snapshot_writes_locked();
    }
    static void uncertain_write(Server& server, const std::string& file) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_);
        server.juiced_->state_["snapshot_writes"][file]["state"] = "uncertain"; server.juiced_->persist_locked();
    }
    static void reconcile_writes(Server& server, uint64_t generation, bool stop) {
        if (stop) server.router_->admission_.reconcile_stopped_generation(generation);
        else server.router_->admission_.reconcile_idle_requests(generation);
        server.juiced_->reconcile_snapshot_writes();
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_); server.juiced_->cleanup_snapshot_writes_locked();
    }
    static void complete_write(Server& server, const std::string& file) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_); server.juiced_->state_["snapshot_writes"][file]["state"] = "cleanup";
        server.juiced_->persist_locked(); server.juiced_->cleanup_snapshot_writes_locked();
    }
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
    static void execute_live(Server& server, const json& command) {
        server.juiced_->stopping_ = false;
        server.juiced_->execute(command);
        server.juiced_->stopping_ = true;
    }
    static json operations(Server& server) { std::lock_guard<std::mutex> lock(server.juiced_->mutex_); return server.juiced_->operations_; }
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

    static void register_local_model(Server& server, const std::string& name) {
        const auto file = std::filesystem::path(server.config_dir_) / "native-fixture.gguf";
        if (!std::filesystem::exists(file)) {
            char header[24] = {'G', 'G', 'U', 'F', 3};
            std::ofstream out(file, std::ios::binary); out.write(header, sizeof(header));
        }
        server.model_manager_->register_user_model(name, {{"recipe", "llamacpp"}, {"source", "local_path"},
            {"checkpoints", {{"main", file.string()}}}, {"recipe_options", {{"ctx_size", 2048}, {"llamacpp_backend", "cpu"}}}});
    }
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

struct TestGate {
    std::promise<void> entered, released;
    std::shared_future<void> go = released.get_future().share();
    std::atomic<bool> signaled{false}, opened{false};
    void pause() { if (!signaled.exchange(true)) entered.set_value(); go.wait(); }
    bool await() { const bool ready = entered.get_future().wait_for(std::chrono::seconds(10)) == std::future_status::ready; if (!ready) open(); return ready; }
    void open() { if (!opened.exchange(true)) released.set_value(); }
    ~TestGate() { open(); }
};

static void native_load_publication() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("native-load-policy");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false; defaults["auto_evict"] = false; defaults["max_loaded_models"] = 20;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server); Hook::stop_eviction(server);
        int index = 0;
        for (bool automatic : {false, true}) for (bool reverse : {false, true}) for (bool retry : {false, true}) {
            const auto name = "user.LoadPublication" + std::to_string(index++);
            Hook::register_local_model(server, name);
            Hook::mutate(server, {{"model", name}, {"policy", reverse ? "hot" : "on-demand"}});
            TestGate startup; std::atomic<int> starts{0};
            Hook::factory(server, [&] { const auto start = ++starts; if (retry && start == 1) throw std::runtime_error("controlled first startup failure"); startup.pause(); });
            auto load = std::async(std::launch::async, [&] {
                if (automatic) { try { Hook::auto_load(server, name); return 200; } catch (...) { return 500; } }
                return Hook::native_load(server, name).status;
            });
            const auto entered = startup.await();
            const auto policy = Hook::mutate(server, {{"model", name}, {"policy", reverse ? "on-demand" : "hot"}});
            startup.open(); const auto status = load.get();
            check(entered && status == 200 && policy.status == 200 && Hook::pinned(server, name) == !reverse,
                  "native and automatic fresh/retry publication obey newer policy in both directions");
            Hook::factory(server, {});
            if (!automatic) {
                TestGate entry; Hook::load_entry(server, [&] { entry.pause(); });
                auto reuse = std::async(std::launch::async, [&] { return Hook::native_load(server, name); });
                const auto reached = entry.await();
                Hook::mutate(server, {{"model", name}, {"policy", reverse ? "hot" : "on-demand"}});
                entry.open(); const auto response = reuse.get(); Hook::load_entry(server, {});
                check(reached && response.status == 200 && Hook::pinned(server, name) == reverse,
                      "older native reuse cannot overwrite a newer committed pin");
                TestGate replacement; Hook::factory(server, [&] { replacement.pause(); });
                const auto old_generation = Hook::generation(server, name);
                auto reload = std::async(std::launch::async, [&] { return Hook::native_load(server, name, 4096); });
                const auto replacing = replacement.await(); Hook::mutate(server, {{"model", name}, {"policy", reverse ? "on-demand" : "hot"}});
                replacement.open(); const auto reloaded = reload.get();
                check(replacing && reloaded.status == 200 && Hook::generation(server, name) != old_generation && Hook::pinned(server, name) == !reverse,
                      "replacement publication applies current policy to its exact new generation");
            }
            Hook::unload(server, name);
        }
        const std::string name = "user.AutomaticIntent"; Hook::register_local_model(server, name);
        const auto public_name = Hook::models(server).get_public_model_name(name);
        Hook::mutate(server, {{"model", name}, {"policy", "hot"}});
        const auto revision = Hook::state(server).at("policy_revisions").at(public_name);
        const json old = {{"model", public_name}, {"action", "load"}, {"policy_revision", revision}};
        std::atomic<int> starts{0}; Hook::factory(server, [&] { ++starts; });
        Hook::mutate(server, {{"model", name}, {"policy", "on-demand"}}); Hook::execute(server, old);
        Hook::mutate(server, {{"model", name}, {"policy", "hot"}}); Hook::execute(server, old);
        check(starts == 0 && Hook::generation(server, name) == 0, "obsolete automatic Hot commands reject before acquisition, including away-and-back changes");
        const auto new_revision = Hook::state(server).at("policy_revisions").at(public_name).get<uint64_t>();
        TestGate in_progress; Hook::factory(server, [&] { in_progress.pause(); });
        auto automatic = std::async(std::launch::async, [&] { return Hook::intent_load(server, name, new_revision); });
        const auto begun = in_progress.await();
        Hook::mutate(server, {{"model", name}, {"policy", "on-demand"}});
        Hook::mutate(server, {{"model", name}, {"policy", "hot"}});
        in_progress.open(); const auto rejected = automatic.get();
        check(begun && rejected.at("status") == 409 && rejected.at("code") == "policy_intent_changed" && Hook::generation(server, name) == 0,
              "automatic startup revalidates away-and-back policy changes before worker publication");
        check(Hook::admission(server).at("resident") == 0 && Hook::admission(server).at("active") == 0,
              "superseded automatic generation releases residency and active reservations after confirmed stop");
        TestGate queued_start; Hook::factory(server, [&] { queued_start.pause(); });
        const auto current = Hook::state(server).at("policy_revisions").at(public_name);
        auto queued = std::async(std::launch::async, [&] { Hook::execute_live(server, {{"model", public_name}, {"action", "load"}, {"policy_revision", current}}); });
        const auto running = queued_start.await();
        Hook::mutate(server, {{"model", name}, {"policy", "on-demand"}});
        Hook::mutate(server, {{"model", name}, {"policy", "hot"}});
        queued_start.open(); queued.get();
        check(running && Hook::generation(server, name) == 0 && !Hook::operations(server).contains(public_name),
              "obsolete automatic startup clears its queued status instead of delaying newer Hot intent as a failure");
        Hook::factory(server, [&] { ++starts; });
        Hook::mutate(server, {{"model", name}, {"policy", "on-demand"}});
        check(Hook::native_load(server, name).status == 200 && starts == 1 && !Hook::pinned(server, name), "explicit On-demand loading remains available");
        Hook::unload(server, name);
        const std::string recovery = "user.WatchdogPolicy"; Hook::register_local_model(server, recovery);
        Hook::mutate(server, {{"model", recovery}, {"policy", "on-demand"}}); Hook::native_load(server, recovery);
        const auto recovery_generation = Hook::generation(server, recovery); Hook::crash(server, recovery);
        TestGate restart; Hook::factory(server, [&] { restart.pause(); });
        auto recovering = std::async(std::launch::async, [&] { return Hook::native_load(server, recovery); });
        const auto restarting = restart.await(); Hook::mutate(server, {{"model", recovery}, {"policy", "hot"}});
        restart.open(); const auto recovered = recovering.get();
        check(restarting && recovered.status == 200 && Hook::generation(server, recovery) != recovery_generation && Hook::pinned(server, recovery),
              "unavailable-worker recovery publishes a new generation with current Hot pinning");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void deferred_maintenance() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("deferred-downsize");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server); Hook::stop_eviction(server);
        const std::string name = "user.DownsizeOrdering"; Hook::register_model(server, name); Hook::add_worker(server, name); Hook::idle_options(server, name);
        for (bool replace : {false, true}) {
            Hook::mutate(server, {{"model", name}, {"policy", "on-demand"}});
            TestGate selected; Hook::downsize_barrier(server, [&] { selected.pause(); });
            auto evaluation = std::async(std::launch::async, [&] { Hook::evaluate(server); });
            const auto reached = selected.await();
            if (replace) { Hook::add_worker(server, name, true); Hook::idle_options(server, name); }
            else Hook::mutate(server, {{"model", name}, {"policy", "hot"}});
            selected.open(); evaluation.get(); Hook::downsize_barrier(server, {});
            check(reached && Hook::worker_state(server, name) == lemon::ModelState::READY,
                  "deferred downsizing rejects a newly Hot or replaced generation");
        }
        Hook::idle_options(server, name, 1); Hook::touch(server, name, true);
        TestGate idle; Hook::downsize_barrier(server, [&] { idle.pause(); });
        auto evaluation = std::async(std::launch::async, [&] { Hook::evaluate(server); });
        const auto selected = idle.await(); Hook::touch(server, name); idle.open(); evaluation.get(); Hook::downsize_barrier(server, {});
        check(selected && Hook::worker_state(server, name) == lemon::ModelState::READY, "maintenance revalidates recent access after deferred selection");
        Hook::idle_options(server, name);
        Hook::evaluate(server);
        check(Hook::worker_state(server, name) == lemon::ModelState::DOWNSIZED, "eligible unchanged idle generation can still claim maintenance");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void options_response_versions() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("options-response");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        const std::string name = "user.OptionsResponse"; Hook::register_model(server, name); auto& models = Hook::models(server);
        for (const auto& method : {"GET", "POST", "DELETE"}) for (bool profile : {false, true}) {
            models.select_execution_profile(name, {{"id", "registered"}});
            models.set_saved_model_options(name, {{"ctx_size", 2048}});
            TestGate derived; Hook::options_barrier(server, [&] { derived.pause(); });
            auto request = std::async(std::launch::async, [&] { return Hook::options_response(server, name, method, {{"ctx_size", 3072}}); });
            const auto reached = derived.await();
            if (profile) models.select_execution_profile(name, {{"id", "alternate"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 8192}}}});
            else models.set_saved_model_options(name, {{"ctx_size", 8192}});
            derived.open(); const auto response = request.get(); Hook::options_barrier(server, {});
            const auto body = json::parse(response.body);
            const auto expected = std::string(method) == "DELETE" ? json::object() : json{{"ctx_size", std::string(method) == "POST" ? 3072 : 2048}};
            check(reached && response.status == 200 && body.at("recipe") == "llamacpp" && body.at("execution_profile") == "registered" && body.at("saved") == expected &&
                  body.at("effective").at("ctx_size") == expected.value("ctx_size", -1),
                  "options GET/POST/DELETE responses retain one committed recipe, saved and effective version across successful concurrent updates");
        }
        models.select_execution_profile(name, {{"id", "registered"}}); models.set_saved_model_options(name, {{"ctx_size", 2048}});
        TestGate preview; Hook::options_barrier(server, [&] { preview.pause(); });
        auto request = std::async(std::launch::async, [&] { return Hook::options_response(server, name, "POST", {{"dry_run", true}, {"ctx_size", 3072}}); });
        const auto reached = preview.await(); models.select_execution_profile(name, {{"id", "alternate"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 8192}}}});
        preview.open(); const auto response = request.get(); Hook::options_barrier(server, {}); const auto body = json::parse(response.body);
        check(reached && response.status == 200 && body.at("recipe") == "llamacpp" && body.at("saved").at("ctx_size") == 2048 && body.at("effective").at("ctx_size") == 3072 && models.get_model_info(name).recipe == "vllm",
              "dry-run derives preview and unchanged saved values from one committed snapshot without publishing over another profile");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void cache_disk_budget() {
    using Hook = lemon::StatePublicationTestHook;
    for (bool ttl : {false, true}) {
        const auto root = lemon::test::make_scratch_dir("disk-budget");
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            Hook::seed_disk(server, 64, 40, 40, ttl);
            const auto before = Hook::state(server);
            for (bool staging : {false, true}) {
                bool rejected = false;
                if (staging) {
                    fs::create_directory(root / "juiced/state.json.tmp");
                    try { Hook::prune(server); } catch (...) { rejected = true; }
                    fs::remove(root / "juiced/state.json.tmp");
                } else {
                    write(root / "juiced/state.json.tmp", json::object()); BlockReplacement denied(root / "juiced/state.json");
                    try { Hook::prune(server); } catch (...) { rejected = true; }
                }
                check(rejected && Hook::state(server) == before && fs::file_size(root / "juiced/snapshots/first.bin") == 40 && fs::file_size(root / "juiced/snapshots/second.bin") == 40,
                      "TTL and budget pruning preserve populated files/metadata on staging and replacement failure");
                fs::remove(root / "juiced/state.json.tmp");
            }
            {
                BlockReplacement denied_first(root / "juiced/snapshots/first.bin");
#ifdef _WIN32
                BlockReplacement denied_second(root / "juiced/snapshots/second.bin");
#endif
                Hook::prune(server);
                check(Hook::disk(server).at("pending_cleanup_bytes") == 80 && Hook::disk(server).at("used_bytes") == 80 &&
                      !Hook::reserve_write(server, "new.pending", 32), "undeleted TTL/budget tombstones retain disk credit and block another key");
            }
        }
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            Hook::cleanup(server);
            check(Hook::disk(server).at("used_bytes") == 0 && Hook::reserve_write(server, "first.pending", 40), "confirmed cleanup restores admission after retry/restart");
            check(Hook::disk(server).at("reserved_write_bytes") == 40 && !Hook::reserve_write(server, "second.pending", 40) && !Hook::reserve_write(server, "first.pending", 1),
                  "outstanding writes reserve disk capacity and exclude concurrent reuse of their key");
        }
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            check(Hook::disk(server).at("reserved_write_bytes") == 40 && !Hook::reserve_write(server, "second.pending", 40),
                  "interrupted unknown-owner write remains conservatively charged after restart");
            Hook::reconcile_writes(server, 10001, true);
            check(Hook::disk(server).at("reserved_write_bytes") == 40, "a reused generation number in another manager run cannot release interrupted disk ownership");
            Hook::complete_write(server, "first.pending");
            check(Hook::disk(server).at("used_bytes") == 0 && Hook::reserve_write(server, "second.pending", 40, 10002), "explicitly confirmed complete write releases disk credit only after cleanup");
            Hook::uncertain_write(server, "second.pending"); Hook::reconcile_writes(server, 10002, false);
            check(Hook::disk(server).at("reserved_write_bytes") == 40, "idle inference does not establish termination of an uncertain snapshot write");
            Hook::reconcile_writes(server, 10002, true);
            check(Hook::disk(server).at("used_bytes") == 0, "exact-generation stop permits cleanup and release of the current manager's uncertain write");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
}

static void snapshot_publication_failures() {
    using Hook = lemon::StatePublicationTestHook;
    for (bool existing : {false, true}) for (bool staging : {false, true}) {
        const auto root = lemon::test::make_scratch_dir("snapshot-publication");
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            const auto intent = Hook::stage_publication(server, existing);
            check(!Hook::state(server).at("snapshots").contains("publication") && intent.at("previous").empty() == !existing,
                  "sealed publication hides previous metadata and durably retains previous ownership before replacement");
            const auto target = root / "juiced/state.json";
            std::unique_ptr<BlockReplacement> blocked;
            bool rejected = false;
            try {
                Hook::publish(server, intent, [&] {
                    check(!fs::exists(root / "juiced/snapshots/publication.pending") && fs::file_size(root / "juiced/snapshots/publication.bin") == 3,
                          "fault is injected after actual pending-to-final replacement");
                    if (staging) fs::create_directory(root / "juiced/state.json.tmp");
                    else blocked = std::make_unique<BlockReplacement>(target);
                });
            } catch (...) { rejected = true; }
            check(rejected && Hook::state(server).at("snapshot_writes").at("publication.pending") == intent &&
                  read(target).at("snapshot_writes").at("publication.pending") == intent && !Hook::state(server).at("snapshots").contains("publication"),
                  "failed final metadata staging/replacement preserves durable destination ownership without stale checksums");
            if (existing) {
                std::ifstream previous(root / "juiced/snapshots/publication.previous.bin"); std::string contents; previous >> contents;
                check(contents == "old", "failed replacement publication preserves the previous committed file under its durable intent");
            }
            Hook::clean_writes(server);
            check(Hook::disk(server).at("used_bytes").get<uint64_t>() >= 3 && fs::exists(root / "juiced/snapshots/publication.bin"),
                  "generic cleanup cannot remove publishing intent or release retained final-file charges");
            blocked.reset(); fs::remove_all(root / "juiced/state.json.tmp");
            check(!Hook::reserve_write(server, "other.pending", 10), "failed final publication cannot grant unavailable replacement credit");
            if (staging) {
                Hook::recover_publications(server);
                check(Hook::state(server).at("snapshot_writes").empty() && Hook::disk(server).at("used_bytes") == 3,
                      "publication retry in the same manager commits the verified final file");
            }
        }
        {
            std::unique_ptr<BlockReplacement> no_cleanup;
            if (existing && !staging)
                no_cleanup = std::make_unique<BlockReplacement>(root / "juiced/snapshots/publication.previous.bin");
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            Hook::recover_publications(server);
            const auto state = Hook::state(server);
            check(state.at("snapshot_writes").empty() && state.at("snapshots").at("publication").at("checksum") ==
                  "11507a0e2f5e69d5dfa40a62a1bd7b6ee57e6bcd85c67c9b8431b36fff21c437" && Hook::disk(server).at("used_bytes").get<uint64_t>() >= 3,
                  "restart recovery verifies final content and commits its matching metadata despite unavailable cleanup");
            if (no_cleanup)
                check(fs::exists(root / "juiced/snapshots/publication.previous.bin") && Hook::disk(server).at("pending_cleanup_bytes") == 3,
                      "failed previous-file cleanup after restart retains the old file and its exact disk charge");
            no_cleanup.reset();
            Hook::cleanup(server);
            check(Hook::disk(server).at("used_bytes") == 3, "previous-file cleanup releases its charge only after confirmed reclamation");
            check(Hook::reserve_write(server, "next.pending", 10), "publication recovery reopens only reclaimed storage credit");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
}

static void interrupted_snapshot_publication() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("snapshot-pre-rename");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        Hook::stage_publication(server, true);
    }
    { std::ofstream file(root / "juiced/snapshots/publication.pending"); file << "bad"; }
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        Hook::recover_publications(server);
        check(!Hook::state(server).at("snapshot_writes").empty() && Hook::disk(server).at("used_bytes").get<uint64_t>() >= 6 &&
              fs::exists(root / "juiced/snapshots/publication.bin"),
              "unverified pending content retains both file charges and previous ownership across restart");
        { std::ofstream file(root / "juiced/snapshots/publication.pending"); file << "new"; }
        Hook::recover_publications(server);
        check(Hook::state(server).at("snapshot_writes").empty() && Hook::disk(server).at("used_bytes") == 3,
              "sealed pre-rename restart completes replacement only after verifying candidate contents");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void options_clear_parity() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("options-clears");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        auto& models = Hook::models(server);
        const std::string name = "user.ClearPreview";
        models.register_user_model(name, {{"recipe", "sd-cpp"}, {"checkpoint", "fixture/repo:Q4_K_M"}, {"recipe_options", {{"steps", 7}}}});
        for (const json clear : {json(nullptr), json(""), json("auto"), json(-1)}) {
            models.set_saved_model_options(name, {{"steps", 11}});
            const auto preview = Hook::options_response(server, name, "POST", {{"dry_run", true}, {"steps", clear}});
            check(preview.status == 200 && models.get_saved_model_options(name).at("steps") == 11,
                  "clear preview validates without changing committed saved settings");
            const auto applied = Hook::options_response(server, name, "POST", {{"steps", clear}});
            check(preview.status == 200 && applied.status == 200 && json::parse(preview.body).at("effective").at("steps") == 7 &&
                  json::parse(preview.body).at("effective") == json::parse(applied.body).at("effective"),
                  "normalized null/empty/auto/minus-one preview restores the registry default exactly like real POST");
        }
        Hook::register_model(server, "user.ContextClearPreview");
        const auto preview = Hook::options_response(server, "user.ContextClearPreview", "POST", {{"dry_run", true}, {"ctx_size", -1}});
        const auto applied = Hook::options_response(server, "user.ContextClearPreview", "POST", {{"ctx_size", -1}});
        check(preview.status == 200 && applied.status == 200 && json::parse(preview.body).at("effective").at("ctx_size") == -1 &&
              models.get_saved_model_options("user.ContextClearPreview").at("ctx_size") == -1,
              "context minus-one remains an explicit setting in preview and committed mutation");
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

int main(int argc, char** argv) {
    if (argc > 1 && std::string(argv[1]) == "--snapshots") {
        snapshot_publication_failures(); interrupted_snapshot_publication(); options_clear_parity();
    } else if (argc > 1 && std::string(argv[1]) == "--lifecycle") {
        native_load_publication(); deferred_maintenance(); options_response_versions(); cache_disk_budget();
    } else if (argc > 1 && std::string(argv[1]) == "--ordering") {
        successful_profile_overlap(); profile_discovery(); policy_ordering(); cache_forget_failures();
    } else {
        cloud_failures(); retained_jobs(); model_files(); profile_concurrency(); policy_failures();
    }
    std::fprintf(stderr, "Persistence boundaries: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
