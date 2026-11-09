// Checks that WrappedServer::get_process_info(), which /health reports, takes the
// PID and launch command from the same ServerProcess: a start publishes both, a
// restart replaces the previous command, and a stop or failed start clears both.
// An integration test against /health cannot reach these states: a restart only
// happens on a watchdog reset, and Router::get_all_loaded_models() drops dead
// backends before it builds the response.

#include "lemon/wrapped_server.h"

#include <httplib.h>

#include <cstdio>
#include <future>
#include <memory>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

namespace {

int failures = 0;

void check(const std::string& what, bool ok) {
    std::printf("%s %s\n", ok ? "PASS" : "FAIL", what.c_str());
    if (!ok) ++failures;
}

}  // namespace

namespace lemon {

// A server that reports itself running under `pid` without spawning anything,
// so the test controls liveness and never signals a real process.
class FakeProcess : public ServerProcess {
public:
    explicit FakeProcess(int pid, bool* fail_stop = nullptr, std::function<void()> before_stop = {})
        : ServerProcess(ProcessOutput{}), pid_(pid), fail_stop_(fail_stop), before_stop_(std::move(before_stop)) {}
    ~FakeProcess() override { handle_ = {nullptr, 0}; }

    std::string start(const ServerCommand& command) override {
        command_line_ = {command.program};
        command_line_.insert(command_line_.end(), command.args.begin(), command.args.end());
        handle_.pid = pid_;
        return "127.0.0.1";
    }

    void stop() override {
        if (before_stop_) before_stop_();
        if (fail_stop_ && *fail_stop_) throw std::runtime_error("injected unconfirmed stop");
        handle_ = {nullptr, 0};
    }

    bool running() const override { return handle_.pid != 0; }
    bool exited() const override { return !running(); }

private:
    int pid_;
    bool* fail_stop_;
    std::function<void()> before_stop_;
};

// Minimal WrappedServer that runs whatever ServerProcess the test hands it.
// start_server() and stop_server() are protected, so the stub republishes them.
class StubWrappedServer : public WrappedServer {
public:
    StubWrappedServer() : WrappedServer("stub", "error", nullptr, nullptr) {}

    void load(const std::string&, const ModelInfo&, const RecipeOptions&, bool) override {}

    using WrappedServer::start_server;
    using WrappedServer::stop_server;
};

}  // namespace lemon

int main() {
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
        check("failed replacement cleanup retains the newly started process", rejected && server.get_process_info().pid == 606);
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

    health.stop();
    health_thread.join();

    if (failures == 0) {
        std::printf("\nAll launch command checks passed.\n");
        return 0;
    }
    std::printf("\n%d launch command check(s) failed.\n", failures);
    return 1;
}
