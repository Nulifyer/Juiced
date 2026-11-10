#include "lemon/server_process.h"

#include <cerrno>
#include <chrono>
#include <csignal>
#include <cstdio>
#include <exception>
#include <filesystem>
#include <stdexcept>
#include <string>
#include <thread>
#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

int main(int argc, char** argv) {
#ifndef _WIN32
    if (argc > 1 && std::string(argv[1]).find("--output-") == 0) {
        const std::string mode = argv[1];
        std::printf("%d\n", static_cast<int>(getpid()));
        if (mode.find("--output-tail") == 0) std::printf("tail");
        std::fflush(stdout);
        std::fclose(stdout);
        std::fclose(stderr);
        std::this_thread::sleep_for(std::chrono::milliseconds(
            mode == "--output-short" || mode == "--output-tail-short" ? 250 : 4000));
        return 7;
    }
#endif
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
        const auto output_case = [&](const std::string& mode, int timeout, bool cancel_tail, bool throw_tail) {
            int child_pid = 0;
            int tail_calls = 0;
            bool callback_failed = false;
            int result = 0;
            const auto started = std::chrono::steady_clock::now();
            try {
                result = ProcessManager::run_process_with_output(executable, {mode}, [&](const auto& line) {
                    if (line != "tail") { child_pid = std::stoi(line); return true; }
                    ++tail_calls;
                    if (throw_tail) throw std::runtime_error("controlled trailing callback failure");
                    return !cancel_tail;
                }, "", timeout);
            } catch (const std::runtime_error& error) {
                callback_failed = std::string(error.what()) == "controlled trailing callback failure";
            }
            const auto elapsed = std::chrono::steady_clock::now() - started;
            const bool collected = child_pid > 0 && ::kill(child_pid, 0) < 0 && errno == ESRCH;
            std::fprintf(stderr, "Output case %s: %lld ms, result %d, collected %s\n", mode.c_str(),
                         static_cast<long long>(std::chrono::duration_cast<std::chrono::milliseconds>(elapsed).count()),
                         result, collected ? "true" : "false");
            if (mode == "--output-short" || mode == "--output-tail-short") {
                check(result == 7 && elapsed >= std::chrono::milliseconds(200),
                      "closed output preserves the eventual direct-child status");
                if (mode == "--output-tail-short")
                    check(tail_calls == 1, "closed output delivers an accepted trailing line exactly once");
            } else if (throw_tail) {
                check(tail_calls == 1 && callback_failed, "trailing callback failure remains observable");
            } else {
                check(result == -1 && elapsed < std::chrono::seconds(3),
                      cancel_tail ? "trailing callback cancellation stops a live closed-output child" :
                                    "closed output cannot bypass the process deadline");
                if (cancel_tail) check(tail_calls == 1, "the trailing output callback runs before process exit");
            }
            check(collected, "output exchange collects its direct child before returning");
        };
        output_case("--output-short", 2, false, false);
        output_case("--output-tail-short", 2, false, false);
        output_case("--output-closed", 1, false, false);
        output_case("--output-tail", -1, true, false);
        output_case("--output-tail", 2, false, true);
#endif
    } catch (const std::exception& error) {
        ++failures;
        std::fprintf(stderr, "Unexpected process observation failure: %s\n", error.what());
    }
    std::fprintf(stderr, "Process exit observation: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
