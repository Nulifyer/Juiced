#include "lemon/cloud_provider_registry.h"
#include "lemon/config_file.h"
#include "lemon/eviction_engine.h"
#include "lemon/error_types.h"
#include "lemon/jobs/job_manager.h"
#include "lemon/juiced/service.h"
#include "lemon/model_manager.h"
#include "lemon/mcp_server.h"
#include "lemon/runtime_config.h"
#include "lemon/server.h"
#include "lemon/utils/json_utils.h"
#include "lemon/utils/path_utils.h"
#include "support/test_state_isolation.h"

#include <atomic>
#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <functional>
#include <future>
#include <sstream>
#include <thread>
#include <regex>

#ifdef _WIN32
#include <windows.h>
#endif

namespace lemon::utils {
struct JsonPersistenceTestHook {
    static void save(const nlohmann::json& body, const std::filesystem::path& target,
                     std::function<void(const std::filesystem::path&)> before_replace) {
        JsonUtils::save_to_file(body, path_to_utf8(target), std::move(before_replace));
    }
};
}

namespace lemon {
struct ModelPublicationTestHook {
    static void removal_barrier(ModelManager& models, std::function<void(const std::string&)> callback) { models.removal_publication_barrier_ = std::move(callback); }
    static std::string builtin_name(ModelManager& models) { return models.server_models_.begin().key(); }
    static void read_barrier(ModelManager& models, std::function<void()> callback) { models.cache_read_barrier_ = std::move(callback); }
    static void write_barrier(ModelManager& models, std::function<void()> callback) { models.options_write_barrier_ = std::move(callback); }
    static void discovered(ModelManager& models, const std::string& name) {
        models.get_supported_models();
        std::lock_guard<std::mutex> lock(models.models_cache_mutex_);
        ModelInfo info; info.model_name = name; info.recipe = "cloud"; info.source = "discovered";
        info.checkpoints["main"] = "https://fixture.invalid/model"; info.labels = {"chat"};
        info.recipe_options = RecipeOptions("cloud", json::object());
        models.models_cache_[name] = std::move(info); models.rebuild_public_model_aliases_locked();
    }
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
class CacheBoundaryWorker : public PolicyWorker, public ISlotsServer {
public:
    CacheBoundaryWorker(const std::string& name, int port, const std::filesystem::path& directory) : PolicyWorker(name) {
        port_ = port;
        context_observation_ = {{"slot_ids", json::array({0})}};
        execution_identity_ = {{"recipe", "llamacpp"}, {"captured", true}, {"engine", "dde879b-controlled-save-boundary"}, {"scope", "controlled-test-inputs"}};
        set_model_metadata(name, "fixture", ModelType::LLM, DEVICE_CPU, RecipeOptions("llamacpp", {
            {"ctx_size", 2048}, {"llamacpp_backend", "cpu"}, {"llamacpp_args", "--slot-save-path \"" + directory.string() + "\""}}));
    }
    std::atomic<bool> owns_endpoint{true};
    bool endpoint_owned() const override { return owns_endpoint.load(); }
    std::atomic<bool> owns_runtime{true};
    bool runtime_inputs_current(const std::function<bool()>& cancelled = {}) const override { return owns_runtime.load() && !(cancelled && cancelled()); }
    bool observes_inference_completion() const override { return true; }
    json get_slots() override { return forward_get_request("/slots"); }
    json slots_action(int slot, const std::string& action, const json& body) override {
        return forward_request("/slots/" + std::to_string(slot) + "?action=" + action, body);
    }
    json chat_completion(const json&) override {
        note_request_started();
        note_request_completed();
        return {{"id", "fixture"}, {"choices", json::array({{{"index", 0}, {"message", {{"role", "assistant"}, {"content", "ok"}}}, {"finish_reason", "stop"}}})}};
    }
};
class LoadingPolicyWorker : public PolicyWorker {
public:
    LoadingPolicyWorker(const std::string& name, std::function<void()> load) : PolicyWorker(name), on_load_(std::move(load)) {}
    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override { if (on_load_) on_load_(); }
private:
    std::function<void()> on_load_;
};
class RuntimeStopWorker : public PolicyWorker {
public:
    RuntimeStopWorker(const std::string& name, int failure) : PolicyWorker(name), failure(failure) {
        set_model_metadata(name, "fixture", ModelType::LLM, DEVICE_CPU,
            RecipeOptions("llamacpp", {{"ctx_size", 2048}, {"llamacpp_backend", "cpu"}}));
    }
    void unload() override {
        if (failure == 1) throw std::runtime_error("injected unconfirmed runtime stop");
        if (failure == 2) throw RouterResidencyConflictException(get_model_name(), get_model_name(), "injected busy drain");
        PolicyWorker::unload();
    }
    bool is_backend_alive() const override { return failure == 1 || PolicyWorker::is_backend_alive(); }
    int failure;
};
struct StatePublicationTestHook {
    static httplib::Response configure(Server& server, const json& changes) {
        httplib::Request request; request.method = "POST"; request.path = "/internal/set"; request.body = changes.dump();
        httplib::Response response; response.status = 200; server.handle_config_set(request, response); return response;
    }
    static void config_effect(Server& server, std::function<void()> barrier) { server.config_effect_barrier_ = std::move(barrier); }
    static std::shared_ptr<WebSocketServer> listener(Server& server) { return std::atomic_load(&server.websocket_server_); }
    static void start_listener(Server& server) { server.running_ = true; server.restart_websocket_listener(); }
    static void cloud_publish(Server& server) {
        server.cloud_registry_->install("config-fixture", "https://fixture.invalid/v1", {},
            [&](const json& metadata) { server.persist_cloud_providers(metadata); });
    }
    static void install(Server& server, std::function<void(const std::string&, const std::string&)> callback) {
        server.backend_install_override_ = std::move(callback);
    }
    static std::shared_ptr<RuntimeStopWorker> failed_runtime(Server& server, const std::string& name, int failure) {
        auto worker = std::make_shared<RuntimeStopWorker>(name, failure);
        ResourceAdmission::Work work; work.model = name; work.generation = worker->get_instance_id(); work.operation = ResourceAdmission::Operation::Load;
        auto residency = server.router_->admission_.acquire(work); residency.started(); residency.retain_residency();
        worker->retain_resource_residency(std::move(residency));
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_); server.router_->loaded_servers_.push_back(worker);
        return worker;
    }
    static void cache_worker(Server& server, const std::string& name, int port) {
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_);
        server.router_->loaded_servers_.push_back(std::make_shared<CacheBoundaryWorker>(name, port, server.juiced_->snapshot_directory_));
    }
    static void endpoint_owner(Server& server, const std::string& name, bool owned) {
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_);
        dynamic_cast<CacheBoundaryWorker*>(server.router_->find_server_by_model_name(name))->owns_endpoint = owned;
    }
    static void runtime_owner(Server& server, const std::string& name, bool owned) {
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_);
        dynamic_cast<CacheBoundaryWorker*>(server.router_->find_server_by_model_name(name))->owns_runtime = owned;
    }
    static void cache_arguments(Server& server, const std::string& name, const std::string& args) {
        std::lock_guard<std::mutex> lock(server.router_->load_mutex_);
        server.router_->find_server_by_model_name(name)->set_model_metadata(name, "fixture", ModelType::LLM, DEVICE_CPU,
            RecipeOptions("llamacpp", {{"ctx_size", 2048}, {"llamacpp_backend", "cpu"}, {"llamacpp_args", args}}));
    }
    static void save_barrier(Server& server, std::function<void()> callback) { server.juiced_->snapshot_save_barrier_ = std::move(callback); }
    static httplib::Response cache_chat(Server& server, const std::string& model, const std::string& session, bool stream, std::atomic<bool>& closed) {
        httplib::Request request; request.method = "POST"; request.path = "/v1/chat/completions";
        request.set_header("X-Juiced-Session-Id", session);
        request.is_connection_closed = [&] { return closed.load(); };
        request.body = json{{"model", model}, {"messages", json::array({{{"role", "user"}, {"content", "fixture"}}})}, {"stream", stream}}.dump();
        httplib::Response response; response.status = 200;
        auto handler = server.contextual_handler([&](const auto& req, auto& res) { server.juiced_->chat(req, res); });
        handler(request, response);
        if (response.content_provider_) {
            httplib::DataSink sink;
            sink.write = [](const char*, size_t) { return true; }; sink.is_writable = [] { return true; }; sink.done = [] {};
            response.content_provider_(0, 0, sink);
            if (response.content_provider_resource_releaser_) { response.content_provider_resource_releaser_(true); response.content_provider_resource_releaser_ = nullptr; }
            response.content_provider_ = nullptr;
        }
        return response;
    }
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
    static lemon::ResourceAdmission writer_admission(Server& server) { return server.router_->admission_view(); }
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
            server.juiced_->state_["snapshots"]["publication"] = {{"bytes", 3}, {"checksum", "cba06b5736faf67e54b07b561eae94395e774c517a7d910a54369e1263ccfbd4"}, {"retained", true}};
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
    static void verification_barrier(Server& server, std::function<void()> barrier) { server.juiced_->snapshot_verification_barrier_ = std::move(barrier); }
    static void recover_publications(Server& server) { server.juiced_->reconcile_snapshot_writes(); }
    static void clean_writes(Server& server) {
        std::lock_guard<std::mutex> lock(server.juiced_->mutex_); server.juiced_->cleanup_snapshot_writes_locked();
    }
    static httplib::Response import_request(Server& server, const std::string& operation, const json& body, const std::string& chunk = "") {
        httplib::Request request; request.method = "POST"; request.path = "/v1/juiced/import/" + operation;
        request.body = operation == "chunk" ? chunk : body.dump();
        if (operation == "chunk") { request.params.emplace("id", body.at("id").get<std::string>()); request.params.emplace("offset", body.value("offset", "0")); }
        httplib::Response response; response.status = 200; server.juiced_->import_file(request, response, operation); return response;
    }
    static httplib::Response remove(Server& server, const std::string& name, bool native = false, bool review = false) {
        httplib::Request request; request.method = review ? "GET" : "POST";
        request.path = native ? "/v1/delete" : "/v1/juiced/removal";
        request.params.emplace("model", name);
        json body = {{"model", name}, {"confirmation", name}};
        if (!native && !review) { body["expected"] = server.juiced_->removal_plan(name); body["confirmation"] = body["expected"]["model"]; }
        request.body = body.dump();
        httplib::Response response; response.status = 200;
        if (native) server.handle_delete(request, response);
        else server.juiced_->handle(request, response, "removal");
        return response;
    }
    static void removal_barrier(Server& server, std::function<void()> callback) { server.juiced_->removal_claim_barrier_ = std::move(callback); }
    static void import_barrier(Server& server, std::function<void(const std::string&)> barrier) { server.juiced_->import_publication_barrier_ = std::move(barrier); }
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
        auto cmd = std::move(server.juiced_->pending_.front()); server.juiced_->pending_.pop_front(); return cmd.command;
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
    static httplib::Response prepared_remove(Server& server, const std::string& id) {
        httplib::Request req; req.method = "POST"; req.path = "/v1/juiced/preparation";
        req.body = json{{"action", "delete_cache"}, {"id", id}}.dump();
        httplib::Response response; response.status = 200; server.juiced_->preparation_api(req, response); return response;
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
            lemon::jobs::JobManager jobs(root.string(), lemon::ResourceAdmission{});
            auto before = jobs.get("retained-1"); const auto disk = read(target);
            bool rejected = false;
            if (staging) {
                fs::create_directory(root / "jobs.json.tmp");
                try { jobs.remove("retained-1"); } catch (const lemon::jobs::JobError& e) { rejected = e.status == 500; }
                fs::remove(root / "jobs.json.tmp");
            } else {
                write(root / "jobs.json.tmp", json::object());
                BlockReplacement block(target);
                try { jobs.remove("retained-1"); } catch (const lemon::jobs::JobError& e) { rejected = e.status == 500; }
            }
            check(rejected && jobs.get("retained-1") == before && read(target) == disk,
                  "failed retained deletion or tombstone preserves memory and committed record");
            check(jobs.remove("retained-1"), "failed retained deletion remains retryable");
        }
        lemon::jobs::JobManager restarted(root.string(), lemon::ResourceAdmission{});
        check(!restarted.get("retained-1"), "durable deletion does not reappear after restart");
    }
}

static void metadata_json_files() {
    const auto root = lemon::test::make_scratch_dir("metadata-json");
    const json previous = {{"revision", 1}, {"value", "committed"}};
    const json candidate = {{"revision", 2}, {"value", "candidate"}};
    for (const auto* name : {"user_models.json", "recipe_options.json", "execution_profiles.json",
                             ".download_manifest.json", ".lemonade_registry.json"}) {
        const auto target = root / name;
        write(target, previous);
#ifndef _WIN32
        fs::permissions(target, fs::perms::owner_read | fs::perms::owner_write);
#endif
        bool rejected = false;
        try { lemon::utils::JsonUtils::save_to_file({{"value", std::string(1, static_cast<char>(0xff))}}, target.string()); }
        catch (const std::exception&) { rejected = true; }
        check(rejected && read(target) == previous, "serialization failure never truncates committed model JSON");
        fs::path staged;
        bool complete = false; rejected = false;
        try {
            lemon::utils::JsonPersistenceTestHook::save(candidate, target, [&](const auto& temporary) {
                staged = temporary; complete = read(temporary) == candidate && read(target) == previous;
                throw std::runtime_error("controlled pre-replacement failure");
            });
        } catch (const std::exception&) { rejected = true; }
        check(rejected && complete && !fs::exists(staged) && read(target) == previous,
              "failed staged publication cleans its complete candidate and preserves committed JSON");
        std::unique_ptr<BlockReplacement> block;
        std::string failure;
        try {
            lemon::utils::JsonPersistenceTestHook::save(candidate, target, [&](const auto& temporary) {
                staged = temporary; block = std::make_unique<BlockReplacement>(target);
            });
        } catch (const std::exception& error) { failure = error.what(); }
        block.reset();
        check(!failure.empty() && read(target) == previous,
              "replacement failure after complete staging preserves the previous JSON record");
        check(!fs::exists(staged) || (failure.find(lemon::utils::path_to_utf8(staged)) != std::string::npos && read(staged) == candidate),
              "failed cleanup retains an explicit path to the uncommitted candidate");
        if (fs::exists(staged)) fs::remove(staged);
        lemon::utils::JsonUtils::save_to_file(candidate, target.string());
        check(read(target) == candidate, "retry publishes the complete model JSON candidate");
#ifndef _WIN32
        check(fs::status(target).permissions() == (fs::perms::owner_read | fs::perms::owner_write),
              "atomic JSON replacement preserves restrictive existing file permissions");
#endif
    }
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["auto_check_model_updates"] = false;
    lemon::RuntimeConfig runtime(defaults); lemon::RuntimeConfig::set_global(&runtime);
    std::string name;
    { lemon::ModelManager initial; name = lemon::ModelPublicationTestHook::builtin_name(initial); }
    const auto options = fs::path(lemon::utils::get_config_dir()) / "recipe_options.json";
    const json original = {{name, {{"ctx_size", 2048}}}};
    write(options, original);
    {
        BlockReplacement blocked(options);
        lemon::ModelManager failed;
        check(read(options) == original, "failed startup option migration preserves its committed input file");
    }
    {
        lemon::ModelManager restarted;
        const auto migrated = read(options);
        check(!migrated.contains(name) && migrated.size() == 1 && migrated.begin().value() == original.at(name),
              "startup retry commits one complete canonical option migration");
        check(restarted.get_saved_model_options(name) == original.at(name),
              "restarted ModelManager reads the same effective saved values after migration");
    }
    lemon::RuntimeConfig::set_global(nullptr);
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
        const auto root = lemon::utils::get_config_dir();
        const auto profiles_path = fs::path(root) / "execution_profiles.json";
        if (!fs::exists(profiles_path)) lemon::utils::JsonUtils::save_to_file(json::object(), profiles_path.string());
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
        bool failed = false;
        {
            BlockReplacement block(profiles_path);
            try { models.select_execution_profile("user.Persistence", {{"id", "registered"}}); }
            catch (const std::exception&) { failed = true; }
        }
        check(failed && read(profiles_path) == committed_profiles && models.get_model_info("user.Persistence").recipe == "vllm" &&
              models.get_model_info("user.Persistence").recipe_options.get_option("ctx_size") == 4096,
              "failed authoritative settings publication preserves selection and options together");
        // Block only the legacy pathname. POSIX BlockReplacement protects the
        // whole parent and would also prohibit this positive profile write.
        const auto held_options = config_root / "recipe_options.backup";
        fs::rename(options_path, held_options);
        fs::create_directory(options_path);
        bool selected_registered = false;
        try {
            models.select_execution_profile("user.Persistence", {{"id", "registered"}});
            selected_registered = models.get_model_info("user.Persistence").recipe == "llamacpp" &&
                models.get_model_info("user.Persistence").recipe_options.get_option("ctx_size") == 8192;
        } catch (const std::exception& error) {
            std::fprintf(stderr, "Unexpected profile publication failure: %s\n", error.what());
        }
        fs::remove(options_path);
        fs::rename(held_options, options_path);
        check(selected_registered, "profile-owned publication is independent of the preserved legacy options file");
        models.select_execution_profile("user.Persistence", {{"id", "changed"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 4096}}}});
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

struct ListenerPortFixture {
    httplib::Server http;
    int port = http.bind_to_any_port("127.0.0.1");
    std::thread worker;
    ListenerPortFixture() {
        if (port <= 0) throw std::runtime_error("Cannot bind listener port fixture");
        worker = std::thread([this] { http.listen_after_bind(); });
        http.wait_until_ready();
    }
    void release() { http.stop(); if (worker.joinable()) worker.join(); }
    ~ListenerPortFixture() { release(); }
};

static void configuration_effects() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("configuration-effects");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    defaults["auto_evict"] = false; defaults["auto_check_model_updates"] = false; defaults["host"] = "127.0.0.1";
    ListenerPortFixture initial_port, next_port;
    const auto initial = initial_port.port, next = next_port.port;
    initial_port.release(); next_port.release(); defaults["websocket_port"] = initial;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server); Hook::stop_eviction(server);
        Hook::start_listener(server); const auto original = Hook::listener(server);
        check(original && original->is_running(), "native configuration fixture starts one loopback listener");
        TestGate first; std::atomic<int> calls{0};
        Hook::config_effect(server, [&] { if (++calls == 1) first.pause(); });
        auto host = std::async(std::launch::async, [&] { return Hook::configure(server, {{"host", "localhost"}}); });
        const auto entered = first.await();
        std::promise<void> queued;
        auto port = std::async(std::launch::async, [&] { queued.set_value(); return Hook::configure(server, {{"websocket_port", next}}); });
        queued.get_future().wait();
        check(entered && port.wait_for(std::chrono::milliseconds(30)) == std::future_status::timeout &&
              runtime->host() == "localhost" && runtime->websocket_port() == initial,
              "later settings wait until the earlier committed listener effect finishes");
        auto cloud = std::async(std::launch::async, [&] { Hook::cloud_publish(server); });
        const bool independent = cloud.wait_for(std::chrono::seconds(5)) == std::future_status::ready;
        first.open(); cloud.get();
        check(independent, "registry publication does not acquire the listener effects owner");
        check(host.get().status == 200 && port.get().status == 200,
              "overlapping host and port changes complete through the native settings handler");
        Hook::config_effect(server, {});
        const auto current = Hook::listener(server);
        check(current != original && !original->is_running() && current->is_running() && current->get_port() == next &&
              runtime->host() == "localhost" && runtime->websocket_port() == next,
              "one active listener matches the latest committed host and port");
        ListenerPortFixture occupied; const auto blocked = occupied.port;
        const auto failed = Hook::configure(server, {{"websocket_port", blocked}});
        const auto failure = json::parse(failed.body);
        check(failed.status == 503 && failure["saved"] == true && failure["applied"] == false &&
              failure["error"]["code"] == "configuration_application_failed" && runtime->websocket_port() == blocked,
              "listener bind failure reports saved intent separately from failed application");
        check(!current->is_running() && !Hook::listener(server)->is_running(),
              "failed replacement cannot advertise its stopped previous listener as active");
        occupied.release();
        const auto retry = Hook::configure(server, {{"websocket_port", blocked}});
        check(retry.status == 200 && Hook::listener(server)->is_running() && Hook::listener(server)->get_port() == blocked,
              "identical settings retry reapplies a previously failed committed effect");
        TestGate shutdown;
        Hook::config_effect(server, [&] { shutdown.pause(); });
        auto changing = std::async(std::launch::async, [&] { return Hook::configure(server, {{"host", "127.0.0.1"}}); });
        check(shutdown.await(), "shutdown overlap reaches the committed effect boundary");
        server.stop(); shutdown.open();
        check(changing.get().status == 200 && !Hook::listener(server)->is_running(),
              "shutdown prevents a delayed effect from restarting its listener");
        Hook::config_effect(server, {});
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void runtime_swap_failures() {
    using Hook = lemon::StatePublicationTestHook;
    for (int failure : {1, 2}) {
        const auto root = lemon::test::make_scratch_dir("runtime-swap");
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        defaults["auto_evict"] = false; defaults["auto_check_model_updates"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server); Hook::stop_eviction(server);
            const std::string name = "user.Swap", other = "user.BlockedSwap";
            Hook::register_local_model(server, name); Hook::register_local_model(server, other);
            Hook::mutate(server, {{"model", name}, {"policy", "on-demand"}});
            Hook::mutate(server, {{"model", other}, {"policy", "on-demand"}});
            auto worker = Hook::failed_runtime(server, name, failure);
            const auto generation = worker->get_instance_id();
            const auto directory = root / "runtime"; fs::create_directories(directory);
            { std::ofstream file(directory / "referenced"); file << "original"; }
            std::atomic<int> installs{0}, starts{0};
            Hook::install(server, [&](const std::string&, const std::string&) { ++installs; fs::remove_all(directory); });
            Hook::factory(server, [&] { ++starts; });
            const json change = {{"llamacpp", {{"cpu_bin", "fixture-version"}}}};
            const auto rejected = Hook::configure(server, change);
            check(rejected.status == 503 && json::parse(rejected.body)["saved"] == true &&
                  json::parse(rejected.body)["applied"] == false,
                  "busy or unconfirmed runtime retirement reports saved but unapplied configuration");
            check(installs == 0 && starts == 0 && fs::exists(directory / "referenced") &&
                  Hook::generation(server, name) == generation && Hook::admission(server)["resident"] == 1,
                  "unresolved worker ownership prevents runtime promotion, reclamation and reload");
            bool blocked = false;
            try { Hook::auto_load(server, other); }
            catch (const lemon::AdmissionError& error) { blocked = error.code == "backend_update_pending"; }
            check(blocked && installs == 0 && starts == 0,
                  "another load cannot bypass a pending runtime update for the same recipe");
            worker->failure = 0;
            const auto retried = Hook::configure(server, change);
            check(retried.status == 200 && installs == 1 && starts == 1 && Hook::generation(server, name) != generation &&
                  Hook::admission(server)["resident"] == 1 && Hook::admission(server)["uncertain"] == 0,
                  "identical configuration retry confirms the old stop before installing and restoring its service");
            const auto loaded = Hook::native_load(server, other);
            check(loaded.status == 200 && starts == 2, "successful runtime application reopens native loading");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
}

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
        const auto superseded_ledger = Hook::admission(server);
        const bool superseded_released = superseded_ledger.at("resident") == 0 && superseded_ledger.at("active") == 0 &&
            superseded_ledger.at("uncertain") == 0 && superseded_ledger.at("model_cache").at("references") == 0;
        if (!superseded_released)
            std::fprintf(stderr, "Superseded automatic ledger: %s\n", superseded_ledger.dump().c_str());
        check(superseded_released,
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
        const std::string cleanup = "user.CleanupPending"; Hook::register_local_model(server, cleanup);
        Hook::mutate(server, {{"model", cleanup}, {"policy", "on-demand"}});
        auto failed = Hook::failed_runtime(server, cleanup, 1);
        const auto failed_generation = failed->get_instance_id();
        bool stop_failed = false;
        try { Hook::unload(server, cleanup); } catch (const std::runtime_error&) { stop_failed = true; }
        check(stop_failed && failed->cleanup_pending() && failed->get_state() == lemon::ModelState::UNLOADED && failed->is_backend_alive(),
              "failed unload retains a live but unavailable cleanup-pending worker");
        starts = 0; Hook::factory(server, [&] { ++starts; });
        const auto rejected_load = Hook::native_load(server, cleanup);
        bool automatic_rejected = false;
        try { Hook::auto_load(server, cleanup); }
        catch (const lemon::AdmissionError& error) { automatic_rejected = error.code == "worker_cleanup_pending" && error.status == 503; }
        check(rejected_load.status == 503 && automatic_rejected && starts == 0 && Hook::generation(server, cleanup) == failed_generation,
              "native and automatic loads reject unresolved cleanup without success or duplicate startup");
        failed->failure = 0; Hook::unload(server, cleanup);
        check(Hook::generation(server, cleanup) == 0 && Hook::generation(server, recovery) != 0,
              "confirmed cleanup retires only the failed generation and preserves another service");
        const auto reloaded = Hook::native_load(server, cleanup);
        check(reloaded.status == 200 && starts == 1 && Hook::generation(server, cleanup) != failed_generation,
              "one usable replacement loads after confirmed cleanup");
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
                    else { write(root / "juiced/state.json.tmp", json::object()); blocked = std::make_unique<BlockReplacement>(target); }
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

static void terminal_snapshot_recovery() {
    using Hook = lemon::StatePublicationTestHook;
    for (bool existing : {false, true}) for (bool renamed : {false, true}) for (bool missing : {false, true}) {
        const auto root = lemon::test::make_scratch_dir("snapshot-terminal");
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            const auto intent = Hook::stage_publication(server, existing);
            if (renamed) try { Hook::publish(server, intent, [] { throw std::runtime_error("Interrupted before metadata commit"); }); } catch (...) {}
        }
        const auto failed = root / "juiced/snapshots" / (renamed ? "publication.bin" : "publication.pending");
        if (missing) fs::remove(failed);
        else { std::ofstream file(failed); file << "bad"; }
        {
            std::unique_ptr<BlockReplacement> denied;
            if (!missing) denied = std::make_unique<BlockReplacement>(failed);
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            const auto recovery = Hook::cache(server, json::object(), "cache/recover");
            check(recovery.status == 200, "native recovery accepts sealed publication without file paths or byte rewriting");
            if (denied) check(!Hook::state(server).at("snapshot_writes").empty() && Hook::disk(server).at("used_bytes").get<uint64_t>() >= 3,
                              "failed terminal restoration/cleanup retains owned corrupt bytes and accounting");
            denied.reset();
            const auto retry = Hook::cache(server, json::object(), "cache/recover");
            const auto state = Hook::state(server);
            check(retry.status == 200 && state.at("snapshot_writes").empty() && Hook::disk(server).at("used_bytes") == (existing ? 3 : 0),
                  "retry reaches a terminal recovery disposition and releases only confirmed reclaimed files");
            if (existing) {
                std::ifstream file(root / "juiced/snapshots/publication.bin"); std::string content; file >> content;
                check(content == "old" && state.at("snapshots").at("publication").at("checksum") ==
                      "cba06b5736faf67e54b07b561eae94395e774c517a7d910a54369e1263ccfbd4",
                      "missing/corrupt candidate restores verified previous content with matching usable metadata");
            } else check(!state.at("snapshots").contains("publication"), "unrecoverable new snapshot leaves no advertised saved computation");
            check(Hook::reserve_write(server, "publication.pending", 10), "terminal recovery reopens the same key without an idle-inference stop shortcut");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
}

static void concurrent_snapshot_recovery() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("snapshot-owner");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        const auto intent = Hook::stage_publication(server, true);
        TestGate verifying; std::atomic<int> verifications{0};
        Hook::verification_barrier(server, [&] { if (++verifications == 1) verifying.pause(); });
        auto first = std::async(std::launch::async, [&] { Hook::publish(server, intent, {}); });
        const auto reached = verifying.await();
        auto second = std::async(std::launch::async, [&] { return Hook::cache(server, json::object(), "cache/recover"); });
        const bool prompt = second.wait_for(std::chrono::seconds(2)) == std::future_status::ready;
        const bool pending = Hook::state(server).at("snapshot_writes").contains("publication.pending");
        verifying.open(); first.get(); const auto response = second.get(); Hook::verification_barrier(server, {});
        check(reached && prompt && pending && response.status == 200 && verifications == 1,
              "duplicate recovery returns pending without re-verifying or mutating another publication's files while hashing stays outside the state lock");
        check(Hook::state(server).at("snapshot_writes").empty() && Hook::disk(server).at("saved_bytes") == 3,
              "the sole publication owner commits verified metadata and releases its claim after completion");
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

static void cache_pre_dispatch() {
    using Hook = lemon::StatePublicationTestHook;
    for (bool stream : {false, true}) {
#ifdef _WIN32
        const auto root = lemon::test::make_scratch_dir("save pre dispatch");
#else
        const auto root = lemon::test::make_scratch_dir("save\\pre dispatch");
#endif
        const auto snapshots = root / "juiced/snapshots";
        std::atomic<int> saves{0}, inspections{0};
        httplib::Server engine;
        engine.Get("/props", [&](const auto&, auto& response) { ++inspections; response.set_content(R"({"build_info":"dde879b-controlled-save-boundary"})", "application/json"); });
        engine.Get("/slots", [](const auto&, auto& response) { response.set_content(R"([{"id":0,"is_processing":false}])", "application/json"); });
        engine.Post("/slots/0", [&](const auto& request, auto& response) {
            const auto body = json::parse(request.body);
            if (request.get_param_value("action") == "save") {
                ++saves; std::ofstream file(snapshots / body.at("filename").template get<std::string>()); file << "new";
            }
            response.set_content(R"({"n_saved":1,"n_restored":1})", "application/json");
        });
        engine.Post("/v1/chat/completions", [](const auto&, auto& response) {
            response.set_content("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n", "text/event-stream");
        });
        const auto port = engine.bind_to_any_port("127.0.0.1");
        std::thread listening([&] { engine.listen_after_bind(); });
        struct StopEngine { httplib::Server& engine; std::thread& thread; ~StopEngine() { engine.stop(); thread.join(); } } stop{engine, listening};
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server); Hook::stop_eviction(server);
            const std::string name = "user.CacheBoundary"; Hook::register_local_model(server, name);
            Hook::cache_worker(server, name, port);
            Hook::mutate(server, {{"model", name}, {"policy", "on-demand"}});
            const auto quoted = "\"" + snapshots.string() + "\"";
            Hook::cache_arguments(server, name, "--prompt " + quoted);
            const auto unrelated = Hook::cache(server, {{"model", name}}, "cache/profile");
            check(unrelated.status == 200 && !json::parse(unrelated.body).at("configured").get<bool>(),
                  "a directory appearing in an unrelated option cannot qualify prompt-state storage");
            Hook::cache_arguments(server, name, "--slot-save-path " + quoted + " --slot-save-path " + quoted);
            const auto duplicated = Hook::cache(server, {{"model", name}}, "cache/profile");
            check(duplicated.status == 200 && !json::parse(duplicated.body).at("configured").get<bool>(),
                  "ambiguous duplicate storage flags cannot qualify a cache profile");
            Hook::cache_arguments(server, name, "--slot-save-path " + quoted);
            const auto profile_response = Hook::cache(server, {{"model", name}}, "cache/profile");
            if (profile_response.status != 200) throw std::runtime_error(profile_response.body);
            const auto profile = json::parse(profile_response.body);
            const auto inspected = inspections.load();
            Hook::endpoint_owner(server, name, false);
            const auto foreign = Hook::cache(server, {{"model", name}}, "cache/profile");
            check(foreign.status == 503 && foreign.body.find("backend_listener_unverified") != std::string::npos && inspections == inspected,
                  "lost listener ownership rejects inspection before any backend exchange");
            Hook::endpoint_owner(server, name, true);
            Hook::runtime_owner(server, name, false);
            const auto stale_image_response = Hook::cache(server, {{"model", name}}, "cache/profile");
            const auto stale_image = json::parse(stale_image_response.body);
            check(stale_image_response.status == 200 && stale_image.at("fingerprint") == profile.at("fingerprint") &&
                  stale_image.at("configured") == false && stale_image.at("qualified") == false &&
                  stale_image.at("configuration_reason").get<std::string>().find("Runtime executable") != std::string::npos,
                  "changed runtime association keeps the frozen profile but refuses persistent qualification");
            Hook::runtime_owner(server, name, true);

            check(Hook::cache(server, {{"model", name}, {"fingerprint", profile.at("fingerprint")}, {"evidence", "Controlled pre-dispatch ownership fixture only"}}, "cache/qualify").status == 200,
                  ("controlled save boundary qualifies only the fixture profile: " + profile.dump()).c_str());
            check(Hook::cache(server, {{"mode", "persistent"}, {"disk_budget_gib", 1}}).status == 200, "full-budget legacy snapshot reservation is enabled for the boundary fixture");
            std::atomic<bool> closed{false};
            Hook::save_barrier(server, [&] { closed = true; });
            const auto cancelled = Hook::cache_chat(server, name, "cancel-before-save", stream, closed);
            Hook::save_barrier(server, {});
            check(cancelled.status == 200 && saves == 0 && Hook::state(server).at("snapshot_writes").empty() && Hook::disk(server).at("reserved_write_bytes") == 0,
                  "JSON/SSE pre-dispatch cancellation issues no save and durably releases the unused full-budget reservation");
            closed = false;
            const auto next = Hook::cache_chat(server, name, "next-session", stream, closed);
            check(next.status == 200 && saves == 1 && Hook::state(server).at("snapshot_writes").empty() && Hook::disk(server).at("saved_bytes") == 3,
                  "another JSON/SSE session saves on the same worker without unload after pre-dispatch cancellation");
            Hook::save_barrier(server, [&] { Hook::endpoint_owner(server, name, false); });
            const auto lost = Hook::cache_chat(server, name, "lost-before-save", stream, closed);
            Hook::save_barrier(server, {});
            check(lost.status == 200 && saves == 1 && Hook::state(server).at("snapshot_writes").empty() &&
                  Hook::disk(server).at("reserved_write_bytes") == 0,
                  "lost listener before save preserves inference and durably releases undispatched write credit");
            Hook::endpoint_owner(server, name, true);
            const auto recovered = Hook::cache_chat(server, name, "owner-restored", stream, closed);
            check(recovered.status == 200 && saves == 2,
                  "restored listener ownership permits a later save without unloading the worker");
            Hook::save_barrier(server, [&] { Hook::runtime_owner(server, name, false); });
            const auto lost_image = Hook::cache_chat(server, name, "runtime-lost-before-save", stream, closed);
            Hook::save_barrier(server, {});
            check(lost_image.status == 200 && saves == 2 && Hook::state(server).at("snapshot_writes").empty() &&
                  Hook::disk(server).at("reserved_write_bytes") == 0,
                  "lost runtime association before save preserves inference and releases undispatched disk credit");
            Hook::runtime_owner(server, name, true);
            const auto image_recovered = Hook::cache_chat(server, name, "runtime-owner-restored", stream, closed);
            check(image_recovered.status == 200 && saves == 3,
                  "verified runtime association permits another JSON/SSE save without unloading");
            auto admission = Hook::writer_admission(server);
            bool held = false;
            Hook::save_barrier(server, [&] {
                admission.maintenance(true);
                held = !admission.wait_for_writes(std::chrono::milliseconds(1));
                const auto denied = std::async(std::launch::async, [&] { return Hook::cache(server, {{"mode", "memory"}}); }).get();
                held = held && denied.status == 503 && Hook::state(server)["cache"]["mode"] == "persistent";
            });
            const auto continued = Hook::cache_chat(server, name, "maintenance-finalizer", stream, closed);
            Hook::save_barrier(server, {});
            check(held && continued.status == 200 && saves == 4 && Hook::state(server)["snapshot_writes"].empty(),
                  "accepted JSON/SSE finalizer retains writer ownership and publishes its snapshot after maintenance closes new work");
            check(admission.wait_for_writes(std::chrono::milliseconds(100)) && Hook::disk(server)["reserved_write_bytes"] == 0,
                  "terminal JSON/SSE snapshot publication releases its writer only after coherent file and metadata effects");
            admission.maintenance(false);

        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
}

static std::string import_payload() {
    std::ostringstream out;
    const auto number = [&](auto value) { out.write(reinterpret_cast<const char*>(&value), sizeof(value)); };
    const auto string = [&](const std::string& value) { number(uint64_t(value.size())); out << value; };
    out << "GGUF"; number(uint32_t(3)); number(uint64_t(0)); number(uint64_t(1));
    string("general.architecture"); number(uint32_t(8)); string("llama");
    return out.str();
}
static std::string upload_fixture(lemon::Server& server) {
    using Hook = lemon::StatePublicationTestHook;
    const auto payload = import_payload();
    const auto start = Hook::import_request(server, "start", {{"filename", "native-import.gguf"}, {"size", payload.size()}});
    if (start.status != 201) throw std::runtime_error(start.body);
    const auto id = json::parse(start.body).at("id").get<std::string>();
    const auto chunk = Hook::import_request(server, "chunk", {{"id", id}}, payload);
    if (chunk.status != 200) throw std::runtime_error(chunk.body);
    return id;
}
static void import_publication_failures() {
    using Hook = lemon::StatePublicationTestHook;
    for (const auto& phase : {"sealed", "renamed", "registered"}) for (const auto& fault : {"interrupt", "staging", "replacement"}) {
        if (std::string(phase) != "registered" && std::string(fault) != "interrupt") continue;
        const auto root = lemon::test::make_scratch_dir("import-publication");
        const auto name = "ImportedNative-" + root.filename().string();
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        std::string id;
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            id = upload_fixture(server);
            std::unique_ptr<BlockReplacement> denied;
            Hook::import_barrier(server, [&](const std::string& at) {
                if (at != phase) return;
                if (std::string(fault) == "interrupt") throw std::runtime_error("Controlled import interruption");
                if (std::string(fault) == "staging") fs::create_directory(root / "juiced/state.json.tmp");
                else { write(root / "juiced/state.json.tmp", json::object()); denied = std::make_unique<BlockReplacement>(root / "juiced/state.json"); }
            });
            const auto failed = Hook::import_request(server, "finish", {{"id", id}, {"name", name}});
            Hook::import_barrier(server, {});
            check(failed.status == 500 && Hook::state(server).at("imports").at(id).at("status") == "publishing",
                  "import interruption or final metadata failure retains a committed publication intent");
            const auto publication = Hook::state(server).at("imports").at(id).at("publication");
            check(fs::exists(root / "juiced/imports" / (id + ".part")) || fs::exists(publication.at("destination").get<std::string>()),
                  "failed import publication retains upload or final artifact ownership");
            const auto cancel = Hook::import_request(server, "cancel", {{"id", id}});
            check(cancel.status == 409, "cancel cannot erase files owned by a committed import publication");
            denied.reset(); fs::remove_all(root / "juiced/state.json.tmp");
        }
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            const auto retry = Hook::import_request(server, "finish", {{"id", id}});
            const auto state = Hook::state(server).at("imports").at(id);
            const auto registered = read(fs::path(lemon::utils::get_config_dir()) / "user_models.json").at(name);
            check(retry.status == 201 && state.at("status") == "complete" && registered.at("juiced_import") == id &&
                  fs::exists(state.at("publication").at("destination").get<std::string>()),
                  "restart and retry reconcile file, registration, provenance and complete status after every publication boundary");
            const auto before = read(fs::path(lemon::utils::get_config_dir()) / "user_models.json");
            check(Hook::import_request(server, "finish", {{"id", id}}).status == 201 && read(fs::path(lemon::utils::get_config_dir()) / "user_models.json") == before,
                  "completed import retry is idempotent without replacing registered settings");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
    for (bool staging : {false, true}) {
        const auto root = lemon::test::make_scratch_dir("import-cancel");
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            const auto id = upload_fixture(server);
            std::unique_ptr<BlockReplacement> denied;
            if (staging) fs::create_directory(root / "juiced/state.json.tmp");
            else { write(root / "juiced/state.json.tmp", json::object()); denied = std::make_unique<BlockReplacement>(root / "juiced/state.json"); }
            check(Hook::import_request(server, "cancel", {{"id", id}}).status == 500 &&
                  Hook::state(server).at("imports").at(id).at("status") == "uploading" && fs::exists(root / "juiced/imports" / (id + ".part")),
                  "failed cancellation metadata commit preserves the upload and its writable state");
            denied.reset(); fs::remove_all(root / "juiced/state.json.tmp");
            check(Hook::import_request(server, "cancel", {{"id", id}}).status == 200 && !fs::exists(root / "juiced/imports" / (id + ".part")),
                  "cancel retry commits intent before confirmed deletion");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
}
static void import_publication_races() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("import-races");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        const auto id = upload_fixture(server);
        TestGate validated;
        Hook::import_barrier(server, [&](const std::string& phase) { if (phase == "validated") validated.pause(); });
        auto finish = std::async(std::launch::async, [&] { return Hook::import_request(server, "finish", {{"id", id}, {"name", "RacedImport"}}); });
        const auto reached = validated.await();
        const auto cancel = Hook::import_request(server, "cancel", {{"id", id}});
        const auto chunk = Hook::import_request(server, "chunk", {{"id", id}}, "x");
        const auto duplicate = Hook::import_request(server, "finish", {{"id", id}, {"name", "RacedImport"}});
        validated.open(); const auto completed = finish.get(); Hook::import_barrier(server, {});
        check(reached && cancel.status == 409 && chunk.status == 409 && duplicate.status == 409 && completed.status == 201,
              "one import owner serializes finish against cancel, late chunks and another finish");
        const auto other = upload_fixture(server);
        Hook::import_barrier(server, [&](const std::string& phase) {
            if (phase == "renamed") Hook::models(server).register_user_model("ConflictedImport", {{"recipe", "llamacpp"}, {"checkpoint", "other/repo:Q4_K_M"}});
        });
        const auto conflict = Hook::import_request(server, "finish", {{"id", other}, {"name", "ConflictedImport"}});
        Hook::import_barrier(server, {});
        const auto preserved = read(fs::path(lemon::utils::get_config_dir()) / "user_models.json").at("ConflictedImport");
        check(conflict.status == 409 && Hook::state(server).at("imports").at(other).at("publication").at("conflict") == true && !preserved.contains("juiced_import"),
              "a competing registration is preserved by the native no-overwrite gate and leaves explicit import conflict ownership");
        check(Hook::import_request(server, "finish", {{"id", other}, {"name", "ResolvedImport"}}).status == 201 &&
              read(fs::path(lemon::utils::get_config_dir()) / "user_models.json").at("ConflictedImport") == preserved,
              "conflicted publication can finish under a distinct name without overwriting the competing model");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void import_removal_ownership() {
    using Hook = lemon::StatePublicationTestHook;
    for (const auto& fault : {"interrupt", "staging", "replacement"}) {
        const auto root = lemon::test::make_scratch_dir("import-removal");
        const auto name = (std::string(fault) == "interrupt" ? "user.PendingRemoval-" : "PendingRemoval-") + root.filename().string();
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        std::string id; fs::path destination; std::unique_ptr<BlockReplacement> denied;
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            id = upload_fixture(server);
            bool rejected = false;
            Hook::import_barrier(server, [&](const std::string& phase) {
                if (phase != "registered") return;
                const auto plan = Hook::remove(server, name, false, true);
                rejected = !json::parse(plan.body).at("allowed").get<bool>() && Hook::remove(server, name).status == 409 && Hook::remove(server, name, true).status == 409;
                if (std::string(fault) == "interrupt") throw std::runtime_error("Controlled interruption after registration");
                if (std::string(fault) == "staging") fs::create_directory(root / "juiced/state.json.tmp");
                else { write(root / "juiced/state.json.tmp", json::object()); denied = std::make_unique<BlockReplacement>(root / "juiced/state.json"); }
            });
            const auto failed = Hook::import_request(server, "finish", {{"id", id}, {"name", name}}); Hook::import_barrier(server, {});
            destination = Hook::state(server).at("imports").at(id).at("publication").at("destination").get<std::string>();
            check(rejected && failed.status == 500 && fs::exists(destination) && Hook::models(server).model_exists(name),
                  "native and Juiced removal reject the post-registration window without deleting an owned artifact or registration");
            check(Hook::remove(server, name).status == 409 && Hook::remove(server, name, true).status == 409,
                  "failed final import metadata publication keeps both removal paths rejected");
            if (std::string(fault) == "interrupt") fs::create_directory(root / "juiced/state.json.tmp");
        }
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            check(Hook::remove(server, name).status == 409 && Hook::remove(server, name, true).status == 409 && fs::exists(destination),
                  "restart preserves unresolved import ownership against both removal interfaces");
            denied.reset(); fs::remove_all(root / "juiced/state.json.tmp");
            check(Hook::import_request(server, "finish", {{"id", id}}).status == 201,
                  "owned import finishes after synchronous publication faults are repaired");
            check(Hook::remove(server, name, std::string(fault) == "replacement").status == 200 && !Hook::models(server).model_exists(name),
                  "ordinary removal succeeds after completion and cannot be undone by publishing reconciliation");
            check(Hook::import_request(server, "finish", {{"id", id}}).status == 410,
                  "completed import replay preserves deletion instead of recreating registration");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
}
static void import_deletion_overlap() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("import-delete-overlap");
    const auto name = "ConcurrentRemoval-" + root.filename().string();
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        const auto id = upload_fixture(server); TestGate registered;
        Hook::import_barrier(server, [&](const std::string& phase) {
            if (phase == "registered") { fs::create_directory(root / "juiced/state.json.tmp"); registered.pause(); }
        });
        auto finish = std::async(std::launch::async, [&] { return Hook::import_request(server, "finish", {{"id", id}, {"name", name}}); });
        const auto reached = registered.await();
        const auto denied = Hook::remove(server, name);
        auto deletion = std::async(std::launch::async, [&] { return Hook::remove(server, name, true); });
        const auto blocked = deletion.wait_for(std::chrono::milliseconds(30)) == std::future_status::timeout;
        registered.open(); const auto failed = finish.get(); const auto rejected = deletion.get(); Hook::import_barrier(server, {});
        check(reached && blocked && denied.status == 409 && failed.status == 500 && rejected.status == 409 && Hook::models(server).model_exists(name),
              "concurrent native deletion serializes with publication then rejects the unresolved durable intent without removing registration");
        fs::remove_all(root / "juiced/state.json.tmp");
        check(Hook::import_request(server, "finish", {{"id", id}}).status == 201 && Hook::remove(server, name).status == 200,
              "overlapped failed publication remains recoverable and ordinary removal succeeds after completion");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}
static void import_definition_replays() {
    using Hook = lemon::StatePublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("import-definitions");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false; defaults["disable_model_filtering"] = true;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    std::vector<json> bodies;
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        for (const auto& recipe : {"llamacpp", "asr4all", "parakeet", "whispercpp"}) for (bool explicit_purpose : {false, true}) {
            const auto id = upload_fixture(server);
            json body = {{"id", id}, {"name", "Replay-" + root.filename().string() + "-" + recipe + (explicit_purpose ? "-explicit" : "-default")}, {"recipe", recipe}};
            if (explicit_purpose) body["purpose"] = "chat";
            Hook::import_barrier(server, [&](const std::string& phase) { if (phase == "registered") fs::create_directory(root / "juiced/state.json.tmp"); });
            const auto failed = Hook::import_request(server, "finish", body); Hook::import_barrier(server, {}); fs::remove_all(root / "juiced/state.json.tmp");
            check(failed.status == 500, "all accepted recipes retain publication intent after a final metadata failure");
            bodies.push_back(body);
        }
    }
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        for (const auto& body : bodies) {
            const auto before = read(fs::path(lemon::utils::get_config_dir()) / "user_models.json").at(body.at("name").get<std::string>());
            const auto retry = Hook::import_request(server, "finish", body);
            const auto replay = Hook::import_request(server, "finish", body);
            const auto state = Hook::state(server).at("imports").at(body.at("id").get<std::string>());
            check(retry.status == 201 && replay.status == 201 && state.at("status") == "complete" &&
                  read(fs::path(lemon::utils::get_config_dir()) / "user_models.json").at(body.at("name").get<std::string>()) == before,
                  "identical omitted/explicit purpose requests recover and replay without replacing any recipe's registration");
            auto changed = body; changed["recipe"] = body.at("recipe") == "llamacpp" ? "whispercpp" : "llamacpp";
            check(Hook::import_request(server, "finish", changed).status == 400,
                  "replay still rejects genuinely changed effective definitions");
        }
        const auto id = upload_fixture(server);
        const auto occupied = "OccupiedDiscovery-" + root.filename().string();
        const auto builtins = Hook::models(server).get_supported_models();
        const auto builtin = builtins.find("Whisper-Tiny");
        if (builtin == builtins.end()) throw std::runtime_error("Missing builtin namespace fixture");
        const auto original = builtin->second.checkpoint();
        auto conflict = Hook::import_request(server, "finish", {{"id", id}, {"name", "Whisper-Tiny"}});
        check(conflict.status == 409 && Hook::models(server).get_model_info("Whisper-Tiny").checkpoint() == original,
              "initial no-overwrite rejects an occupied builtin public name through ModelManager");
        lemon::ModelPublicationTestHook::discovered(Hook::models(server), occupied);
        conflict = Hook::import_request(server, "finish", {{"id", id}, {"name", occupied}});
        check(conflict.status == 409 && Hook::models(server).get_model_info(occupied).source == "discovered",
              "conflict renaming cannot shadow a discovered public model");
        conflict = Hook::import_request(server, "finish", {{"id", id}, {"name", "Whisper-Tiny"}});
        check(conflict.status == 409 && Hook::models(server).get_model_info("Whisper-Tiny").checkpoint() == original,
              "conflict renaming cannot shadow a builtin public model");
        check(Hook::import_request(server, "finish", {{"id", id}, {"name", "Unused-" + root.filename().string()}}).status == 201,
              "conflicted publication remains recoverable under a genuinely unused public name");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void prepared_reference_ownership() {
    using Hook = lemon::StatePublicationTestHook;
    using ModelHook = lemon::ModelPublicationTestHook;
    const auto root = lemon::test::make_scratch_dir("prepared-reference-ownership");
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
    auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
    const auto id = std::string(64, 'a'), failed_id = std::string(64, 'b'), partial_id = std::string(64, 'c');
    const auto file = root / "juiced/prepared" / id / "model.gguf";
    auto create = [&](const std::string& key) {
        const auto dir = root / "juiced/prepared" / key; fs::create_directories(dir);
        char header[24] = {'G', 'G', 'U', 'F', 3};
        { std::ofstream output(dir / "model.gguf", std::ios::binary); output.write(header, sizeof(header)); }
        write(dir / "manifest.json", {{"recipe", {{"quant", "Q4_K_M"}}}});
    };
    json definition = {{"recipe", "llamacpp"}, {"source", "local_path"}, {"checkpoint", file.string()}};
    auto alternate_case = file.string();
    for (auto& c : alternate_case) if (c >= 'a' && c <= 'z') c = char(c - 'a' + 'A');
    bool case_alias = false;
    {
        lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
        create(id);
        auto& models = Hook::models(server);
        for (bool plural : {false, true}) {
            const auto name = plural ? "Prepared-Plural" : "Prepared-Singular";
            auto registration = definition;
            if (plural) { registration.erase("checkpoint"); registration["checkpoints"] = {{"main", file.string()}}; }
            models.register_user_model(name, registration);
            const auto artifacts = models.prepared_artifacts();
            check(artifacts.size() == 1 && artifacts[0]["models"] == json::array({name}) && artifacts[0]["removable"] == false,
                  "prepared inventory resolves singular and plural checkpoint references through ModelManager");
            check(Hook::prepared_remove(server, id).status == 409 && fs::is_regular_file(file),
                  "prepared cleanup preserves weights referenced by either supported checkpoint representation");
            models.unregister_user_model(name);
        }
        std::error_code case_error;
        case_alias = fs::equivalent(file, fs::path(alternate_case), case_error) && !case_error;
        if (case_alias) {
            auto alias = definition; alias["checkpoint"] = alternate_case;
            models.register_user_model("Prepared-Case-Reference", alias);
            check(Hook::prepared_remove(server, id).status == 409 && fs::is_regular_file(file),
                  "filesystem-equivalent path casing still protects a prepared reference");
            models.unregister_user_model("Prepared-Case-Reference");
        }
        const auto unused = models.prepared_artifacts();
        models.register_user_model("Prepared-New-Reference", definition);
        check(unused[0]["removable"] == true && Hook::prepared_remove(server, id).status == 409 && fs::is_regular_file(file),
              "a reference committed after inventory inspection invalidates the prepared deletion decision");
        models.unregister_user_model("Prepared-New-Reference");
        TestGate deleting;
        ModelHook::removal_barrier(models, [&](const std::string& phase) { if (phase == "prepared") deleting.pause(); });
        auto removal = std::async(std::launch::async, [&] { return Hook::prepared_remove(server, id); });
        const bool reached = deleting.await();
        std::promise<void> attempted;
        auto reference = std::async(std::launch::async, [&] {
            attempted.set_value();
            try { models.register_user_model("Prepared-Late-Reference", definition); }
            catch (const lemon::ModelRegistrationConflict&) { return true; }
            return false;
        });
        attempted.get_future().wait();
        const bool serialized = reference.wait_for(std::chrono::milliseconds(100)) == std::future_status::timeout;
        auto state = std::async(std::launch::async, [&] { return Hook::state(server); });
        const bool state_available = state.wait_for(std::chrono::seconds(1)) == std::future_status::ready;
        const auto duplicate = Hook::prepared_remove(server, id);
        deleting.open(); const auto removed = removal.get(); const bool rejected = reference.get(); state.get();
        ModelHook::removal_barrier(models, {});
        check(reached && serialized && state_available && duplicate.status == 409,
              "prepared deletion serializes reference acquisition and duplicate cleanup without holding the Juiced state mutex");
        check(removed.status == 200 && rejected && !fs::exists(file) && models.user_model_definition("Prepared-Late-Reference").is_null(),
              "registration overlapping prepared deletion rejects the missing managed file after cleanup");
        create(failed_id);
        const auto failed_file = root / "juiced/prepared" / failed_id / "model.gguf";
        std::unique_ptr<BlockReplacement> blocked;
        ModelHook::removal_barrier(models, [&](const std::string& phase) { if (phase == "prepared") blocked = std::make_unique<BlockReplacement>(failed_file); });
        const auto failure = Hook::prepared_remove(server, failed_id);
        blocked.reset(); ModelHook::removal_barrier(models, {});
        check(failure.status == 400 && fs::is_regular_file(failed_file) && !models.prepared_artifacts().empty(),
              "synchronous prepared cleanup failure preserves the remaining file and visible retry path");
        check(Hook::prepared_remove(server, failed_id).status == 200 && !fs::exists(failed_file),
              "prepared cleanup retry revalidates references and reclaims the owned directory");
        fs::create_directories(root / "juiced/prepared" / partial_id);
        { std::ofstream output(root / "juiced/prepared" / partial_id / "remaining-output"); output << "incomplete copy"; }
        const auto partial = models.prepared_artifacts();
        check(partial.size() == 1 && partial[0]["incomplete"] == true && partial[0]["removable"] == true &&
              Hook::prepared_remove(server, partial_id).status == 200,
              "incomplete prepared directories remain visible and reclaimable after partial cleanup");
    }
    {
        lemon::Server restarted(runtime, root.string(), root.string()); Hook::stop(restarted);
        bool rejected = false;
        try { Hook::models(restarted).register_user_model("Prepared-After-Restart", definition); }
        catch (const lemon::ModelRegistrationConflict&) { rejected = true; }
        check(rejected && Hook::models(restarted).prepared_artifacts().empty(),
              "restart preserves managed prepared-root validation and cannot register deleted weights");
        if (case_alias) {
            auto alias = definition; alias["checkpoint"] = alternate_case;
            rejected = false;
            try { Hook::models(restarted).register_user_model("Prepared-Deleted-Case-Reference", alias); }
            catch (const lemon::ModelRegistrationConflict&) { rejected = true; }
            check(rejected, "different accepted path casing cannot register a deleted managed prepared artifact");
        }
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

static void artifact_removal_ownership() {
    using Hook = lemon::StatePublicationTestHook;
    using ModelHook = lemon::ModelPublicationTestHook;
    const auto registry = fs::path(lemon::utils::get_config_dir()) / "user_models.json";
    // Removal must reconsider both the target definition and other registrations
    // after obtaining the model claim, rather than trusting its earlier plan.
    for (bool shared : {false, true}) {
        const auto root = lemon::test::make_scratch_dir("artifact-removal-race");
        const auto name = "Removal-" + root.filename().string();
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            const auto id = upload_fixture(server);
            const auto imported = Hook::import_request(server, "finish", {{"id", id}, {"name", name}});
            if (imported.status != 201) std::fprintf(stderr, "Import fixture response: %s\n", imported.body.c_str());
            check(imported.status == 201, "removal fixture publishes an actual uploaded GGUF");
            const auto original = Hook::models(server).user_model_definition(name);
            const auto file = original.at("checkpoints").at("main").get<std::string>();
            TestGate planned;
            Hook::removal_barrier(server, [&] { planned.pause(); });
            auto removal = std::async(std::launch::async, [&] { return Hook::remove(server, name); });
            const bool reached = planned.await();
            auto replacement = original;
            if (!shared) { const auto other = root / "replacement.gguf"; fs::copy_file(file, other); replacement["checkpoints"]["main"] = other.string(); }
            Hook::models(server).register_user_model(shared ? name + "-shared" : name, replacement);
            const auto committed = Hook::models(server).user_model_definition(shared ? name + "-shared" : name);
            planned.open(); const auto result = removal.get(); Hook::removal_barrier(server, {});
            check(reached && result.status == 409 && fs::is_regular_file(file) && Hook::models(server).user_model_definition(shared ? name + "-shared" : name) == committed,
                  "stale removal rejects a replaced definition or new shared reference without mutation");
            const auto plan = json::parse(Hook::remove(server, name, false, true).body);
            check(plan.at("action") == "unregister", "fresh removal describes the current external/shared artifact ownership");
            check(Hook::remove(server, name).status == 200 && fs::is_regular_file(file), "fresh registration-only removal preserves referenced or replaced source files");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
    for (const auto& phase : {"before", "sealed", "removed"}) {
        const auto root = lemon::test::make_scratch_dir("artifact-removal-recovery");
        const auto name = "Cleanup-" + root.filename().string();
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        fs::path file; json definition;
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            const auto id = upload_fixture(server);
            check(Hook::import_request(server, "finish", {{"id", id}, {"name", name}}).status == 201, "cleanup fixture commits its uploaded artifact");
            definition = Hook::models(server).user_model_definition(name); file = definition.at("checkpoints").at("main").get<std::string>();
            std::unique_ptr<BlockReplacement> blocked;
            if (std::string(phase) == "before") blocked = std::make_unique<BlockReplacement>(registry);
            else ModelHook::removal_barrier(Hook::models(server), [&](const std::string& reached) {
                if (reached != phase) return;
                if (reached == "sealed") blocked = std::make_unique<BlockReplacement>(file);
                else blocked = std::make_unique<BlockReplacement>(registry);
            });
            const auto failed = Hook::remove(server, name); ModelHook::removal_barrier(Hook::models(server), {});
            blocked.reset();
            const auto retained = Hook::models(server).user_model_definition(name);
            check(failed.status >= 400 && fs::exists(file) == (std::string(phase) != "removed") &&
                  retained.contains("juiced_removal") == (std::string(phase) != "before"),
                  "failed intent/cleanup/final registry commit retains the correct file and recoverable definition");
            if (std::string(phase) != "before") {
                bool name_rejected = false, reference_rejected = false;
                try { Hook::models(server).register_user_model(name, definition); } catch (const lemon::ModelRegistrationConflict&) { name_rejected = true; }
                try { Hook::models(server).register_user_model(name + "-late", definition); } catch (const lemon::ModelRegistrationConflict&) { reference_rejected = true; }
                bool unfiltered_rejected = false;
                try { Hook::models(server).get_model_info_unfiltered("user." + name); }
                catch (const lemon::AdmissionError& error) { unfiltered_rejected = error.code == "artifact_cleanup_pending"; }
                check(name_rejected && reference_rejected && !Hook::models(server).model_exists(name) &&
                      !Hook::models(server).model_exists_unfiltered("user." + name) && unfiltered_rejected,
                      "pending cleanup reserves its name and artifact without exposing a filtered or unfiltered usable model");
            } else check(retained == definition && Hook::models(server).model_exists(name), "failed removal intent preserves the published model and complete definition");
        }
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            check(Hook::remove(server, name, std::string(phase) == "removed").status == 200 && !fs::exists(file) && Hook::models(server).user_model_definition(name).is_null(),
                  "restart and ordinary/native retry finish confirmed artifact cleanup and unregister exactly its owner");
        }
        lemon::RuntimeConfig::set_global(nullptr);
    }
    {
        const auto root = lemon::test::make_scratch_dir("artifact-reference-commit"); const auto name = "Reference-" + root.filename().string();
        auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true; defaults["broadcast"] = false;
        auto runtime = std::make_shared<lemon::RuntimeConfig>(defaults); lemon::RuntimeConfig::set_global(runtime.get());
        {
            lemon::Server server(runtime, root.string(), root.string()); Hook::stop(server);
            const auto id = upload_fixture(server); Hook::import_request(server, "finish", {{"id", id}, {"name", name}});
            const auto definition = Hook::models(server).user_model_definition(name);
            const auto file = definition.at("checkpoints").at("main").get<std::string>();
            TestGate sealed; ModelHook::removal_barrier(Hook::models(server), [&](const std::string& phase) { if (phase == "sealed") sealed.pause(); });
            auto removal = std::async(std::launch::async, [&] { return Hook::remove(server, name); });
            const bool reached = sealed.await(); std::promise<void> entered;
            auto registration = std::async(std::launch::async, [&] { entered.set_value(); try { Hook::models(server).register_user_model(name + "-late", definition); return false; } catch (const lemon::ModelRegistrationConflict&) { return true; } });
            entered.get_future().wait();
            const bool waited = registration.wait_for(std::chrono::milliseconds(50)) == std::future_status::timeout;
            sealed.open(); const auto removed = removal.get(); const bool rejected = registration.get(); ModelHook::removal_barrier(Hook::models(server), {});
            check(reached && waited && removed.status == 200 && rejected && !fs::exists(file) && Hook::models(server).user_model_definition(name + "-late").is_null(),
                  "a new reference cannot commit between artifact validation and deletion or register the deleted upload afterward");
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
    if (argc > 1 && std::string(argv[1]) == "--removal-ownership") {
        artifact_removal_ownership(); prepared_reference_ownership();
    } else if (argc > 1 && std::string(argv[1]) == "--config-effects") {
        configuration_effects(); runtime_swap_failures();
    } else if (argc > 1 && std::string(argv[1]) == "--metadata-files") {
        metadata_json_files();
    } else if (argc > 1 && std::string(argv[1]) == "--import-ownership") {
        import_removal_ownership(); import_deletion_overlap(); import_definition_replays();
    } else if (argc > 1 && std::string(argv[1]) == "--save-dispatch") {
        cache_pre_dispatch();
    } else if (argc > 1 && std::string(argv[1]) == "--cache-recovery") {
        terminal_snapshot_recovery(); concurrent_snapshot_recovery();
    } else if (argc > 1 && std::string(argv[1]) == "--imports") {
        import_publication_failures(); import_publication_races();
    } else if (argc > 1 && std::string(argv[1]) == "--snapshots") {
        snapshot_publication_failures(); options_clear_parity();
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
