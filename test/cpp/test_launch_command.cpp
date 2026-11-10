#include "lemon/backend_manager.h"
#include "lemon/backends/hf_cache_util.h"
#include "lemon/backends/openmoss/openmoss_server.h"
#include "lemon/backends/parakeet/parakeet_server.h"
#include "lemon/backends/vllm/vllm_server.h"
#include "lemon/backends/whispercpp/whispercpp_server.h"
#include "lemon/config_file.h"
#include "lemon/runtime_config.h"
#include "lemon/system_info.h"
#include "lemon/utils/http_client.h"
#include "lemon/wrapped_server.h"
#include "support/test_state_isolation.h"

#include <httplib.h>

#include <atomic>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <functional>
#include <future>
#include <memory>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>
#ifdef __linux__
#include <sys/mman.h>
#include <fcntl.h>
#include <unistd.h>
#endif

namespace {

int failures = 0;

void check(const std::string& what, bool ok) {
    std::printf("%s %s\n", ok ? "PASS" : "FAIL", what.c_str());
    if (!ok) ++failures;
}

}  // namespace

namespace lemon {
struct SystemInfoCacheTestHook {
    static void replace(json snapshot) { SystemInfoCache::replace_snapshot_for_test(std::move(snapshot)); }
};

// A server that reports itself running under `pid` without spawning anything,
// so the test controls liveness and never signals a real process.
class FakeProcess : public ServerProcess {
public:
    explicit FakeProcess(int pid, bool* fail_stop = nullptr, std::function<void()> before_stop = {}, std::function<void()> after_start = {}, bool listener_owned = true)
        : ServerProcess(ProcessOutput{}), pid_(pid), fail_stop_(fail_stop), before_stop_(std::move(before_stop)), after_start_(std::move(after_start)), listener_owned_(listener_owned) {}
    ~FakeProcess() override { handle_ = {nullptr, 0}; }

    std::string start(const ServerCommand& command) override {
        command_line_ = {command.program};
        command_line_.insert(command_line_.end(), command.args.begin(), command.args.end());
        handle_.pid = pid_;
        if (after_start_) after_start_();
        return "127.0.0.1";
    }

    void stop() override {
        if (before_stop_) before_stop_();
        if (fail_stop_ && *fail_stop_) throw std::runtime_error("injected unconfirmed stop");
        handle_ = {nullptr, 0};
    }

    bool owns_loopback_listener(int) const override { return listener_owned_ && running(); }
    bool running() const override { return handle_.pid != 0; }
    bool exited() const override { return !running(); }

private:
    int pid_;
    bool* fail_stop_;
    std::function<void()> before_stop_;
    std::function<void()> after_start_;
    bool listener_owned_;
};

// Minimal WrappedServer that runs whatever ServerProcess the test hands it.
// start_server() and stop_server() are protected, so the stub republishes them.
class StubWrappedServer : public WrappedServer {
public:
    StubWrappedServer() : WrappedServer("stub", "error", nullptr, nullptr) {}

    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override {}

    using WrappedServer::start_server;
    using WrappedServer::stop_server;
    using WrappedServer::capture_runtime_inputs;
};

template<class T> class DerivedWorker : public T {
public:
    DerivedWorker(ModelManager* models = nullptr, BackendManager* runtimes = nullptr) : T("error", models, runtimes) {}
    using WrappedServer::start_server;
    std::function<std::unique_ptr<ServerProcess>(ProcessOutput)> process_factory;
protected:
    std::unique_ptr<ServerProcess> make_native_process(ProcessOutput output) const override {
        return process_factory ? process_factory(output) : T::make_native_process(output);
    }
};

namespace backends {
struct StopFailureTestHook {
    static std::filesystem::path shim(VLLMServer& worker) { return worker.rocm_shim_dir_; }
    static std::string design(OpenMossServer& worker, httplib::DataSink& sink,
                              std::chrono::steady_clock::time_point deadline) {
        worker.exe_path_ = "controlled-openmoss";
        worker.model_path_ = "speech"; worker.voicegen_path_ = "designer";
        return worker.design_reference_sample("controlled voice", sink, deadline);
    }
    static std::filesystem::path dependency(WhisperServer& worker) { return worker.temp_dir_; }
    static std::filesystem::path dependency(VLLMServer& worker) {
        worker.rocm_shim_dir_ = std::filesystem::temp_directory_path() /
            ("juiced-vllm-shim-" + std::to_string(worker.get_instance_id()));
        std::filesystem::create_directories(worker.rocm_shim_dir_);
        return worker.rocm_shim_dir_;
    }
    template<class T> static std::filesystem::path dependency(T&) { return {}; }
};
}

template<class T> void destruction_boundary(const ServerCommand& ready, const char* name) {
    for (bool failed_load : {false, true}) {
        ResourceAdmission ledger;
        bool fail_stop = true;
        std::filesystem::path dependency;
        uint64_t generation = 0;
        {
            DerivedWorker<T> worker;
            dependency = backends::StopFailureTestHook::dependency(static_cast<T&>(worker));
            if (!dependency.empty()) std::ofstream(dependency / "retained") << "owned";
            generation = worker.get_instance_id();
            ResourceAdmission::Work work;
            work.model = name; work.operation = ResourceAdmission::Operation::Load; work.generation = generation;
            auto load = ledger.acquire(work); load.started(); load.retain_residency();
            worker.retain_resource_residency(std::move(load));
            auto command = ready;
            if (failed_load) command.ready_endpoint = "/never-ready";
            bool rejected = false;
            try { worker.start_server(std::make_unique<FakeProcess>(808, &fail_stop), command, failed_load ? 1 : 5); }
            catch (const std::runtime_error&) { rejected = true; }
            check(std::string(name) + " startup outcome reaches the actual derived destructor",
                  rejected == failed_load);
        }
        check(std::string(name) + " destruction survives unconfirmed stop without releasing its generation",
              ledger.snapshot()["uncertain"] == 1 && ledger.snapshot()["resident"] == 1);
        if (!dependency.empty()) {
            check(std::string(name) + " dependency stays while termination is unconfirmed",
                  std::filesystem::exists(dependency / "retained"));
            std::filesystem::remove_all(dependency);
        }
        ledger.reconcile_stopped_generation(generation);
    }
}

}  // namespace lemon

namespace {
struct DesignerControl {
    std::atomic<int> starts{0};
    std::atomic<bool> running{false}, fail_stop{true};
};
class DesignerProcess : public lemon::ServerProcess {
public:
    explicit DesignerProcess(std::shared_ptr<DesignerControl> control) : ServerProcess(lemon::ProcessOutput{}), control_(std::move(control)) {}
    ~DesignerProcess() override { stop_http(); handle_ = {nullptr, 0}; }
    std::string start(const lemon::ServerCommand& command) override {
        if (control_->running.exchange(true)) throw std::runtime_error("overlapping designer process");
        handle_.pid = 900 + ++control_->starts;
        command_line_ = {command.program};
        http_.Get("/health", [](const auto&, auto& response) { response.set_content("{}", "application/json"); });
        http_.Post("/v1/audio/speech", [](const auto&, auto& response) { response.set_content("controlled audio bytes", "audio/wav"); });
        if (!http_.bind_to_port("127.0.0.1", command.port)) throw std::runtime_error("Cannot bind designer fixture");
        thread_ = std::thread([this] { http_.listen_after_bind(); });
        http_.wait_until_ready();
        return "127.0.0.1";
    }
    void stop() override {
        if (!handle_.pid) return;
        if (control_->fail_stop) throw std::runtime_error("controlled designer stop remains unknown");
        stop_http(); handle_ = {nullptr, 0}; control_->running = false;
    }
    bool owns_loopback_listener(int) const override { return running(); }
    bool running() const override { return handle_.pid != 0; }
    bool exited() const override { return !running(); }
private:
    void stop_http() { http_.stop(); if (thread_.joinable()) thread_.join(); }
    std::shared_ptr<DesignerControl> control_;
    httplib::Server http_;
    std::thread thread_;
};
void openmoss_caller_boundaries() {
    for (const auto& failure : {std::string("cancel"), std::string("timeout"), std::string("generated")}) {
        lemon::ResourceAdmission ledger;
        lemon::DerivedWorker<lemon::backends::OpenMossServer> worker;
        worker.set_state(lemon::ModelState::READY);
        auto control = std::make_shared<DesignerControl>();
        worker.process_factory = [control](auto) { return std::make_unique<DesignerProcess>(control); };
        lemon::ResourceAdmission::Work work; work.model = "openmoss-caller";
        work.operation = lemon::ResourceAdmission::Operation::Load; work.generation = worker.get_instance_id();
        auto residency = ledger.acquire(work); residency.started(); residency.retain_residency(); worker.retain_resource_residency(std::move(residency));
        auto request = worker.try_acquire_inference();
        httplib::DataSink sink; sink.is_writable = [&] { return failure != "cancel"; };
        bool rejected = false;
        try { lemon::backends::StopFailureTestHook::design(worker, sink, std::chrono::steady_clock::now() +
                (failure == "timeout" ? std::chrono::seconds(0) : std::chrono::seconds(5))); }
        catch (const std::runtime_error&) { rejected = true; }
        request.release();
        check("OpenMoss " + failure + " failure retains designer ownership and residency without speech replacement",
              rejected && control->starts == 1 && control->running && worker.get_process_info().pid == 901 && ledger.snapshot()["resident"] == 1);
        check("a failed designer cannot become speech-ready when its request lease ends",
              worker.cleanup_pending() && worker.get_state() == lemon::ModelState::UNLOADED &&
              !worker.try_acquire_inference() && !worker.try_acquire_ready_inference() && !worker.try_acquire_ready_state_operation());
        bool protocols_rejected = true;
        for (bool speech : {false, true}) {
            bool rejected_request = false;
            try {
                if (speech) worker.audio_speech({{"input", "not sent to a retained designer"}}, sink);
                else worker.audio_generations({{"prompt", "not sent to a retained designer"}}, sink);
            } catch (const lemon::AdmissionError& error) { rejected_request = error.code == "worker_cleanup_pending"; }
            protocols_rejected &= rejected_request;
        }
        check("queued speech and audio generation cannot forward to a cleanup-pending designer", protocols_rejected && control->starts == 1);
        rejected = false;
        try { lemon::backends::StopFailureTestHook::design(worker, sink, std::chrono::steady_clock::now() + std::chrono::seconds(5)); }
        catch (const std::runtime_error&) { rejected = true; }
        check("OpenMoss retry cannot overlap an unresolved designer", rejected && control->starts == 1 && worker.get_process_info().pid == 901);
        control->fail_stop = false;
        sink.is_writable = [] { return true; };
        const auto sample = lemon::backends::StopFailureTestHook::design(worker, sink, std::chrono::steady_clock::now() + std::chrono::seconds(5));
        check("OpenMoss confirmed cleanup permits exactly one designer and speech replacement",
              !sample.empty() && control->starts == 3 && control->running && worker.get_process_info().pid == 903);
        worker.unload(); worker.release_resource_residency();
        check("OpenMoss terminal retirement releases only the owned generation", !control->running && ledger.snapshot()["resident"] == 0);
    }
}
void start_throw_boundary(const lemon::ServerCommand& command) {
    lemon::ResourceAdmission ledger;
    lemon::StubWrappedServer worker;
    lemon::ResourceAdmission::Work work; work.model = "start-throw";
    work.operation = lemon::ResourceAdmission::Operation::Load; work.generation = worker.get_instance_id();
    auto residency = ledger.acquire(work); residency.started(); residency.retain_residency(); worker.retain_resource_residency(std::move(residency));
    bool fail_stop = true, rejected = false;
    try { worker.start_server(std::make_unique<lemon::FakeProcess>(818, &fail_stop, std::function<void()>{},
            [] { throw std::runtime_error("controlled post-dispatch start failure"); }), command, 5); }
    catch (const std::runtime_error&) { rejected = true; }
    check("post-dispatch startup exceptions retain the process and its reservation",
          rejected && worker.get_process_info().pid == 818 && ledger.snapshot()["resident"] == 1);
    fail_stop = false; worker.stop_server(); worker.release_resource_residency();
    check("confirmed retry cleans a process retained before startup returned", worker.get_process_info().pid == 0 && ledger.snapshot()["resident"] == 0);
}

void cancelled_retirement_boundary(const lemon::ServerCommand& command) {
    std::atomic<bool> cancelled{false};
    bool dispatched = false;
    lemon::StubWrappedServer worker;
    worker.set_load_cancel_flag(&cancelled);
    worker.start_server(std::make_unique<lemon::FakeProcess>(821, nullptr, [&] { cancelled = true; }), command, 5);
    bool rejected = false;
    try { worker.start_server(std::make_unique<lemon::FakeProcess>(822, nullptr, std::function<void()>{}, [&] { dispatched = true; }), command, 5); }
    catch (const std::runtime_error&) { rejected = true; }
    check("cancellation during prior retirement prevents replacement dispatch", rejected && !dispatched && worker.get_process_info().pid == 0);
    cancelled = false;
    worker.start_server(std::make_unique<lemon::FakeProcess>(823), command, 5);
    check("a later accepted startup remains available after pre-dispatch cancellation", worker.get_process_info().pid == 823);
    worker.stop_server();
}

void vllm_load_boundaries(const std::string& executable) {
#ifdef __linux__
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true;
    defaults["vllm"]["rocm_bin"] = executable;
    lemon::RuntimeConfig config(defaults); auto previous_config = lemon::RuntimeConfig::global(); lemon::RuntimeConfig::set_global(&config);
    const auto previous_timeout = lemon::utils::HttpClient::get_default_timeout(); lemon::utils::HttpClient::set_default_timeout(1);
    struct Restore { lemon::RuntimeConfig* config; long timeout; ~Restore() { lemon::RuntimeConfig::set_global(config); lemon::utils::HttpClient::set_default_timeout(timeout); } } restore{previous_config, previous_timeout};
    lemon::ModelManager models; lemon::BackendManager runtimes;
    auto previous_system_info = lemon::SystemInfoCache::get_system_info_with_cache();
    struct RestoreSystemInfo { lemon::json value; ~RestoreSystemInfo() { lemon::SystemInfoCacheTestHook::replace(std::move(value)); } } restore_system_info{previous_system_info};
    auto controlled = previous_system_info;
    controlled["recipes"]["vllm"] = {{"default_backend", "rocm"}, {"backends", {{"rocm", {{"state", "installed"}}}}}};
    lemon::SystemInfoCacheTestHook::replace(std::move(controlled));
    lemon::ModelInfo info; info.checkpoints["main"] = "fixture/vllm"; info.registry_source = "modelscope";
    const auto cache = std::filesystem::path(models.get_hf_cache_dir()) /
        lemon::backends::hf_cache::repo_id_to_cache_dir_name(info.checkpoint(), info.registry_source);
    std::filesystem::create_directories(cache / "refs"); std::filesystem::create_directories(cache / "snapshots/fixture");
    std::ofstream(cache / "refs/main") << "fixture";
    std::ofstream(cache / "snapshots/fixture/config.json") << R"({"quantization_config":{"quant_method":"none"}})";
    for (bool cancelled : {false, true}) {
        lemon::ResourceAdmission ledger;
        bool fail_stop = true; std::atomic<bool> cancel{false}; int starts = 0;
        lemon::DerivedWorker<lemon::backends::VLLMServer> worker(&models, &runtimes);
        worker.set_load_cancel_flag(&cancel);
        worker.process_factory = [&](auto) { ++starts; return std::make_unique<lemon::FakeProcess>(909, &fail_stop, std::function<void()>{}, [&] { cancel = cancelled; }, false); };
        lemon::ResourceAdmission::Work work; work.model = "vllm-caller"; work.operation = lemon::ResourceAdmission::Operation::Load; work.generation = worker.get_instance_id();
        auto residency = ledger.acquire(work); residency.started(); residency.retain_residency(); worker.retain_resource_residency(std::move(residency));
        bool rejected = false;
        try { worker.load("vllm-caller", info, lemon::RecipeOptions("vllm", {{"vllm_backend", "rocm"}, {"ctx_size", 2048}}), true); }
        catch (const std::runtime_error&) { rejected = true; }
        const auto shim = lemon::backends::StopFailureTestHook::shim(worker);
        check("actual vLLM failed load retains its shim and unconfirmed process",
              rejected && !shim.empty() && std::filesystem::is_directory(shim) && worker.get_process_info().pid == 909 && ledger.snapshot()["resident"] == 1);
        cancel = false; rejected = false;
        try { worker.load("vllm-caller", info, lemon::RecipeOptions("vllm", {{"vllm_backend", "rocm"}, {"ctx_size", 2048}}), true); }
        catch (const std::runtime_error&) { rejected = true; }
        check("vLLM failed-load retry cannot replace a retained shim or process", rejected && starts == 1 && std::filesystem::is_directory(shim));
        fail_stop = false; worker.unload(); worker.release_resource_residency();
        check("vLLM confirmed cleanup removes its shim before releasing residency", !std::filesystem::exists(shim) && worker.get_process_info().pid == 0 && ledger.snapshot()["resident"] == 0);
    }
#else
    (void)executable;
#endif
}
} // namespace

int main(int argc, char** argv) {
    if (argc > 1 && std::string(argv[1]) == "--listener-sleep-child") {
        std::this_thread::sleep_for(std::chrono::seconds(30));
        return 0;
    }
    if (argc > 2 && (std::string(argv[1]) == "--listener-server-child" ||
        std::string(argv[1]) == "--listener-slow-child" || std::string(argv[1]) == "--listener-large-child" ||
        std::string(argv[1]) == "--listener-relinquish-child")) {
        const auto mode = std::string(argv[1]);
        httplib::Server server;
#ifdef __linux__
        std::vector<void*> image_mappings;
        server.Get("/map-image", [&](const auto& request, auto& response) {
            if (argc < 4) { response.status = 400; return; }
            const int descriptor = ::open(argv[3], O_RDONLY | O_CLOEXEC);
            if (descriptor < 0) { response.status = 500; return; }
            const auto flags = PROT_READ | PROT_EXEC | (request.has_param("writable") ? PROT_WRITE : 0);
            void* mapping = ::mmap(nullptr, 4096, flags, MAP_PRIVATE, descriptor, 0);
            ::close(descriptor);
            if (mapping == MAP_FAILED) { response.status = 500; return; }
            image_mappings.push_back(mapping);
            response.set_content("{}", "application/json");
        });
#endif
        server.Get("/health", [&](const auto&, auto& response) {
            if (mode == "--listener-slow-child") std::this_thread::sleep_for(std::chrono::seconds(5));
            response.set_content(mode == "--listener-large-child" ? std::string(128 * 1024, 'x') : "{}", "application/json");
        });
        server.Get("/relinquish", [&](const auto&, auto& response) {
            response.set_content("{}", "application/json");
            std::thread([&] { std::this_thread::sleep_for(std::chrono::milliseconds(50)); server.stop(); }).detach();
        });
        const auto served = server.listen("127.0.0.1", std::stoi(argv[2]));
        if (mode == "--listener-relinquish-child") std::this_thread::sleep_for(std::chrono::seconds(30));
        return served ? 0 : 1;
    }
    httplib::Server health;
    health.Get("/health", [](const httplib::Request&, httplib::Response& res) {
        res.set_content("{}", "application/json");
    });
    const int port = health.bind_to_any_port("127.0.0.1");
    if (port <= 0) {
        std::printf("FAIL could not bind the ready endpoint\n");
        return 1;
    }
    std::thread health_thread([&health]() { health.listen_after_bind(); });
    health.wait_until_ready();

    lemon::ServerCommand command;
    command.program = "llama-server.exe";
    command.port = port;

    {
        lemon::StubWrappedServer worker;
        lemon::ServerCommand foreign;
        foreign.program = std::filesystem::absolute(argv[0]).string();
        foreign.args = {"--listener-sleep-child"};
        foreign.port = port;
        bool rejected = false;
        try { worker.start_server(std::make_unique<lemon::NativeProcess>(lemon::ProcessOutput{}), foreign, 1); }
        catch (const std::runtime_error&) { rejected = true; }
        check("a healthy foreign listener cannot make an unrelated native child ready", rejected);
        worker.stop_server();
        check("foreign-listener rejection leaves the unrelated service running", lemon::utils::HttpClient::is_reachable(
            "http://127.0.0.1:" + std::to_string(port) + "/health", 1, lemon::utils::HttpSecurityPolicy::TrustedLoopback));
    }

    {
        lemon::StubWrappedServer worker;
        lemon::ServerCommand owned;
        owned.program = std::filesystem::absolute(argv[0]).string();
        owned.port = lemon::utils::ProcessManager::find_free_port(8001);
        owned.args = {"--listener-server-child", std::to_string(owned.port)};
#ifdef __linux__
        const auto image_file = std::filesystem::temp_directory_path() /
            ("juiced-owned-image-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
        { std::ofstream file(image_file, std::ios::binary); file << std::string(4096, '\0'); }
        owned.args.push_back(image_file.string());
#endif
        auto process = std::make_unique<lemon::NativeProcess>(lemon::ProcessOutput{});
        const auto* child = process.get();
        worker.start_server(std::move(process), owned, 5);
        check("an actual child owning the healthy listener reaches readiness", worker.endpoint_owned());
        check("an owned child does not own the manager's foreign listener", !child->owns_loopback_listener(port));
#ifdef __linux__
        const auto image = worker.capture_runtime_inputs({});
        check("native image capture binds the actual executable and loaded modules", image.at("executable").at("path") == std::filesystem::canonical(argv[0]).string() &&
            image.at("executable").at("sha256").get<std::string>().size() == 64 && !image.at("modules").empty() && worker.runtime_inputs_current());
        bool cancelled = false;
        try { worker.capture_runtime_inputs([] { return true; }); }
        catch (const std::exception&) { cancelled = true; }
        check("cancelled image capture cannot retain a prior qualification projection", cancelled && !worker.runtime_inputs_current());
        worker.capture_runtime_inputs({});
        const auto address = "http://127.0.0.1:" + std::to_string(owned.port);
        const auto added = lemon::utils::HttpClient::get(address + "/map-image", {}, 2, lemon::utils::HttpSecurityPolicy::TrustedLoopback);
        check("late executable mapping invalidates the frozen runtime projection", added.status_code == 200 && !worker.runtime_inputs_current());
        const auto extended = worker.capture_runtime_inputs({});
        check("a freshly captured read-only mapping binds its exact content", worker.runtime_inputs_current() &&
            std::any_of(extended.at("modules").begin(), extended.at("modules").end(), [&](const auto& fact) { return fact.at("path") == image_file.string(); }));
        const auto mutable_image = lemon::utils::HttpClient::get(address + "/map-image?writable=1", {}, 2, lemon::utils::HttpSecurityPolicy::TrustedLoopback);
        bool mutable_rejected = false;
        try { worker.capture_runtime_inputs({}); } catch (const std::exception&) { mutable_rejected = true; }
        check("writable executable mappings refuse qualification while the owned service remains alive", mutable_image.status_code == 200 && mutable_rejected &&
            !worker.runtime_inputs_current() && worker.endpoint_owned());
#else
        check("unsupported native image association is explicit", !child->image_files().has_value());
#endif
        worker.stop_server();
        check("confirmed retirement clears runtime input association", !worker.runtime_inputs_current());
        check("confirmed retirement clears endpoint ownership", !worker.endpoint_owned());
#ifdef __linux__
        std::filesystem::remove(image_file);
#endif
    }

    {
        lemon::StubWrappedServer worker;
        lemon::ServerCommand command;
        command.program = std::filesystem::absolute(argv[0]).string();
        command.port = lemon::utils::ProcessManager::find_free_port(8001);
        command.args = {"--listener-relinquish-child", std::to_string(command.port)};
        worker.start_server(std::make_unique<lemon::NativeProcess>(lemon::ProcessOutput{}), command, 5);
        (void)lemon::utils::HttpClient::get("http://127.0.0.1:" + std::to_string(command.port) + "/relinquish", {}, 1,
            lemon::utils::HttpSecurityPolicy::TrustedLoopback);
        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(2);
        while (worker.endpoint_owned() && std::chrono::steady_clock::now() < deadline)
            std::this_thread::sleep_for(std::chrono::milliseconds(10));
        httplib::Server replacement;
        replacement.Get("/health", [](const auto&, auto& response) { response.set_content("{}", "application/json"); });
        const bool bound = replacement.bind_to_port("127.0.0.1", command.port);
        std::thread listener;
        if (bound) { listener = std::thread([&] { replacement.listen_after_bind(); }); replacement.wait_until_ready(); }
        check("a live child cannot inherit the identity of a replacement listener",
              bound && worker.is_backend_alive() && !worker.endpoint_owned());
        worker.stop_server();
        check("retiring that child preserves the replacement listener", bound && lemon::utils::HttpClient::is_reachable(
            "http://127.0.0.1:" + std::to_string(command.port) + "/health", 1, lemon::utils::HttpSecurityPolicy::TrustedLoopback));
        replacement.stop(); if (listener.joinable()) listener.join();
    }

    for (const auto& mode : {"--listener-slow-child", "--listener-large-child"}) {
        lemon::StubWrappedServer worker;
        lemon::ServerCommand command;
        command.program = std::filesystem::absolute(argv[0]).string();
        command.port = lemon::utils::ProcessManager::find_free_port(8001);
        command.args = {mode, std::to_string(command.port)};
        bool rejected = false;
        const auto started = std::chrono::steady_clock::now();
        try { worker.start_server(std::make_unique<lemon::NativeProcess>(lemon::ProcessOutput{}), command, 1); }
        catch (const std::runtime_error&) { rejected = true; }
        const auto elapsed = std::chrono::steady_clock::now() - started;
        check(mode == std::string("--listener-slow-child") ? "slow health cannot extend the startup polling deadline" :
            "an oversized health response cannot publish readiness", rejected && elapsed < std::chrono::seconds(6));
    }

    {
        lemon::StubWrappedServer server;

        check("a server that never started reports no command",
              server.get_process_info().launch_command.empty());

        command.args = {"-m", "model.gguf", "--ctx-size", "8192"};
        server.start_server(std::make_unique<lemon::FakeProcess>(101), command, 5);
        const lemon::WrappedServer::ProcessInfo first = server.get_process_info();
        check("executable lands at index 0",
              !first.launch_command.empty() && first.launch_command[0] == "llama-server.exe");
        check("arguments follow the executable in order",
              first.launch_command == std::vector<std::string>({"llama-server.exe", "-m",
                                                                "model.gguf", "--ctx-size",
                                                                "8192"}));
        check("the PID is the started process's", first.pid == 101);
        check("the standalone accessors agree with the snapshot",
              server.get_launch_command() == first.launch_command &&
                  server.get_process_id() == first.pid && server.get_backend_port() == port);

        // What a watchdog reset does: same object, second process.
        server.stop_server();
        command.args = {"-m", "model.gguf", "--port", "8082"};
        server.start_server(std::make_unique<lemon::FakeProcess>(202), command, 5);
        const lemon::WrappedServer::ProcessInfo second = server.get_process_info();
        check("a restart replaces the previous command instead of appending",
              second.launch_command == std::vector<std::string>({"llama-server.exe", "-m",
                                                                 "model.gguf", "--port",
                                                                 "8082"}));
        check("a restart reports the new PID", second.pid == 202);

        server.stop_server();
        const lemon::WrappedServer::ProcessInfo stopped = server.get_process_info();
        check("stopping erases pid and command in the same snapshot",
              stopped.pid == 0 && stopped.launch_command.empty());

        bool threw = false;
        try {
            server.start_server(std::make_unique<lemon::FakeProcess>(0), command, 5);
        } catch (const std::runtime_error&) {
            threw = true;
        }
        const lemon::WrappedServer::ProcessInfo failed = server.get_process_info();
        check("a server that exits before it is ready fails the start", threw);
        check("a failed start leaves no pid or command behind",
              failed.pid == 0 && failed.launch_command.empty());
    }

    {
        bool fail_stop = true;
        lemon::ResourceAdmission ledger;
        uint64_t generation = 0;
        {
            lemon::StubWrappedServer server;
            generation = server.get_instance_id();
            lemon::ResourceAdmission::Work work;
            work.model = "uncertain-process"; work.operation = lemon::ResourceAdmission::Operation::Load;
            work.generation = generation;
            auto residency = ledger.acquire(work); residency.started(); residency.retain_residency();
            server.retain_resource_residency(std::move(residency));
            server.start_server(std::make_unique<lemon::FakeProcess>(303, &fail_stop), command, 5);
            bool rejected = false;
            try { server.stop_server(); } catch (const std::runtime_error&) { rejected = true; }
            check("unconfirmed stop retains its process identity and residency",
                  rejected && server.get_process_info().pid == 303 && ledger.snapshot()["resident"] == 1);
            fail_stop = false;
            server.stop_server(); server.release_resource_residency();
            check("confirmed retry releases process ownership and residency",
                  server.get_process_info().pid == 0 && ledger.snapshot()["resident"] == 0);
            fail_stop = true;
            work.generation = server.get_instance_id();
            auto uncertain = ledger.acquire(work); uncertain.started(); uncertain.retain_residency();
            server.retain_resource_residency(std::move(uncertain));
            server.start_server(std::make_unique<lemon::FakeProcess>(404, &fail_stop), command, 5);
        }
        check("destruction retains uncertain logical residency",
              ledger.snapshot()["uncertain"] == 1 && ledger.snapshot()["resident"] == 1);
        ledger.reconcile_stopped_generation(generation);
        check("exact-generation stop evidence can reconcile retained uncertainty", ledger.snapshot()["resident"] == 0 && ledger.snapshot()["uncertain"] == 0);
    }
    {
        bool fail_stop = true;
        lemon::StubWrappedServer server;
        server.start_server(std::make_unique<lemon::FakeProcess>(505, &fail_stop), command, 5);
        bool rejected = false;
        try { server.start_server(std::make_unique<lemon::FakeProcess>(606), command, 5); }
        catch (const std::runtime_error&) { rejected = true; }
        check("failed replacement cleanup prevents dispatch and retains the previous process", rejected && server.get_process_info().pid == 505);
        rejected = false;
        try { server.stop_server(); } catch (const std::runtime_error&) { rejected = true; }
        check("cleanup retries preserve an older process whose stop remains unknown", rejected && server.get_process_info().pid == 505);
        fail_stop = false;
        server.stop_server();
        check("replacement cleanup succeeds after all owned processes confirm exit", server.get_process_info().pid == 0);
    }

    {
        lemon::StubWrappedServer server;
        std::promise<void> entered, release;
        auto resumed = release.get_future().share();
        server.start_server(std::make_unique<lemon::FakeProcess>(707, nullptr, [&] {
            entered.set_value(); resumed.wait();
        }), command, 5);
        auto stopping = std::async(std::launch::async, [&] { server.stop_server(); });
        const bool owned = entered.get_future().wait_for(std::chrono::seconds(10)) == std::future_status::ready;
        bool rejected = false;
        if (owned) {
            try { server.stop_server(); } catch (const std::runtime_error&) { rejected = true; }
        }
        check("a concurrent stop cannot certify a detached process still owned by another stop",
              owned && rejected && server.get_backend_health_state() == "stopping");
        release.set_value(); stopping.get();
        check("the original stop confirms exit before its in-flight ownership disappears",
              server.get_backend_health_state() == "stopped");
    }

    lemon::destruction_boundary<lemon::backends::WhisperServer>(command, "whispercpp");
    lemon::destruction_boundary<lemon::backends::ParakeetServer>(command, "parakeet");
    lemon::destruction_boundary<lemon::backends::VLLMServer>(command, "vllm");
    lemon::destruction_boundary<lemon::backends::OpenMossServer>(command, "openmoss");
    try {
        start_throw_boundary(command);
        cancelled_retirement_boundary(command);
        openmoss_caller_boundaries();
        vllm_load_boundaries(std::filesystem::absolute(argv[0]).string());
    } catch (const std::exception& error) { check(std::string("actual caller fixture: ") + error.what(), false); }

    health.stop();
    health_thread.join();

    if (failures == 0) {
        std::printf("\nAll launch command checks passed.\n");
        return 0;
    }
    std::printf("\n%d launch command check(s) failed.\n", failures);
    return 1;
}
