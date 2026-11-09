#include "lemon/server_process.h"

#include <chrono>
#include <cstdio>
#include <exception>
#include <filesystem>
#include <string>
#include <thread>
#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

int main(int argc, char** argv) {
    if (argc > 1 && std::string(argv[1]) == "--child") {
        std::this_thread::sleep_for(std::chrono::seconds(30));
        return 0;
    }
    if (argc > 1 && std::string(argv[1]) == "--exit") return 7;
    int checks = 0, failures = 0;
    const auto check = [&](bool ok, const char* label) {
        ++checks;
        std::fprintf(stderr, "%s: %s\n", ok ? "PASS" : "FAIL", label);
        if (!ok) ++failures;
    };
    using lemon::utils::ProcessManager;
    try {
        const auto executable = std::filesystem::absolute(argv[0]).u8string();
        lemon::NativeProcess process({false, false});
        check(process.exited(), "an unstarted native owner has no outstanding process");
        process.start({executable, {"--child"}});
        check(process.running() && !process.exited(), "a running owned child is not exit evidence");
        process.stop();
        check(process.exited() && !process.running() && process.handle().pid == 0,
              "confirmed native stop retires the direct child handle");
        process.stop();
        check(process.exited(), "confirmed stop is idempotent");
        process.start({executable, {"--exit"}});
        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
        while (!process.exited() && std::chrono::steady_clock::now() < deadline)
            std::this_thread::sleep_for(std::chrono::milliseconds(10));
        check(process.exited() && process.exited(), "positive exit observation does not consume cleanup ownership");
        process.stop();
        check(process.handle().pid == 0, "an observed exited child is cleaned up explicitly");
#ifdef _WIN32
        const auto query_only = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, GetCurrentProcessId());
        check(query_only != nullptr, "query-only fixture handle opens without termination rights");
        if (query_only) {
            lemon::utils::ProcessHandle uncertain{query_only, static_cast<int>(GetCurrentProcessId())};
            check(!ProcessManager::has_exited(uncertain) && !ProcessManager::stop_process(uncertain),
                  "an unsynchronizable handle cannot certify exit or trigger termination");
            DWORD code = 0;
            check(GetExitCodeProcess(query_only, &code) && code == STILL_ACTIVE,
                  "failed observation preserves its handle for the owning caller");
            CloseHandle(query_only);
        }
#else
        lemon::utils::ProcessHandle unowned{nullptr, static_cast<int>(getpid())};
        check(!ProcessManager::has_exited(unowned) && !ProcessManager::stop_process(unowned),
              "a nonchild PID cannot be signaled or certified as stopped");
#endif
    } catch (const std::exception& error) {
        ++failures;
        std::fprintf(stderr, "Unexpected process observation failure: %s\n", error.what());
    }
    std::fprintf(stderr, "Process exit observation: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
