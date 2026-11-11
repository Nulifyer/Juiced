#include "lemon/server_process.h"

#include <cerrno>
#include <chrono>
#include <csignal>
#include <cstdio>
#include <exception>
#include <filesystem>
#include <fstream>
#include <nlohmann/json.hpp>
#include <stdexcept>
#include <string>
#include <thread>
#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#include <sys/utsname.h>
#endif

int main(int argc, char** argv) {
    if (argc == 7 && std::string(argv[1]) == "--identity-observer") {
        try {
            const lemon::utils::ProcessIdentity identity{argv[2], std::stoi(argv[3]), argv[4], argv[5]};
            const auto state = lemon::utils::ProcessManager::observe_process_identity(identity);
            const auto expected = std::string(argv[6]) == "running" ? lemon::utils::ProcessIdentityState::running :
                std::string(argv[6]) == "exited" ? lemon::utils::ProcessIdentityState::exited : lemon::utils::ProcessIdentityState::unknown;
            return state == expected ? 0 : 7;
        } catch (const std::exception&) { return 8; }
    }
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
    std::string reference_os = "unknown";
#ifdef _WIN32
    using VersionQuery = LONG(WINAPI*)(OSVERSIONINFOW*);
    const auto ntdll = GetModuleHandleW(L"ntdll.dll");
    const auto version_query = ntdll ? reinterpret_cast<VersionQuery>(GetProcAddress(ntdll, "RtlGetVersion")) : nullptr;
    OSVERSIONINFOW version{}; version.dwOSVersionInfoSize = sizeof(version);
    if (version_query && version_query(&version) >= 0)
        reference_os = "Windows " + std::to_string(version.dwMajorVersion) + "." +
            std::to_string(version.dwMinorVersion) + "." + std::to_string(version.dwBuildNumber);
#else
    utsname version{};
    if (::uname(&version) == 0) reference_os = std::string(version.sysname) + " " + version.release + " " + version.machine;
#endif
    int checks = 0, failures = 0;
    bool supported = false, foreign_running = false, foreign_exited = false;
    std::string method;

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
        const auto identity = process.process_identity();
        supported = identity.has_value();
        if (identity) {
            method = identity->method;
            std::fprintf(stderr, "Native identity capability: %s\n", method.c_str());
            check(ProcessManager::observe_process_identity(*identity) == lemon::utils::ProcessIdentityState::running,
                  "captured kernel identity observes the same live child");
            const auto repeated = process.process_identity();
            check(repeated && repeated->domain == identity->domain && repeated->token == identity->token,
                  "reopened native process identity is stable");
            auto changed = *identity; changed.domain[0] = changed.domain[0] == 'a' ? 'b' : 'a';
            check(ProcessManager::observe_process_identity(changed) == lemon::utils::ProcessIdentityState::unknown,
                  "changed execution domain cannot certify process exit");
            check(!process.process_identity([] { return true; }) &&
                  ProcessManager::observe_process_identity(*identity, [] { return true; }) == lemon::utils::ProcessIdentityState::unknown,
                  "cancelled native identity operations remain inconclusive");
        } else {
            std::fprintf(stderr, "Native identity capability: unavailable; prior-writer recovery remains unqualified\n");
            check(ProcessManager::observe_process_identity({}) == lemon::utils::ProcessIdentityState::unknown,
                  "unsupported native identity remains explicitly unknown");
        }
        const auto observe_foreign = [&](const char* expected) {
            return ProcessManager::run_process_with_output(executable,
                {"--identity-observer", identity->method, std::to_string(identity->pid), identity->domain, identity->token, expected},
                [](const std::string&) { return true; }, "", 5) == 0;
        };
        if (identity) {
            foreign_running = observe_foreign("running");
            check(foreign_running && process.running(), "another native process observes the live writer without signaling it");
            auto malformed = *identity;
            malformed.pid = 0;
            check(ProcessManager::observe_process_identity(malformed) == lemon::utils::ProcessIdentityState::unknown,
                  "invalid process identifiers never establish termination");
            malformed = *identity; malformed.token = "invalid";
            check(ProcessManager::observe_process_identity(malformed) == lemon::utils::ProcessIdentityState::unknown,
                  "malformed persisted process tags never establish termination");
        }
        process.stop();
        if (identity) {
            foreign_exited = observe_foreign("exited");
            check(foreign_exited, "another native process observes exit after the original owner releases its handle");
        }
        if (identity) check(ProcessManager::observe_process_identity(*identity) == lemon::utils::ProcessIdentityState::exited,
                            "persisted identity observes exit after its original handle is retired");
        check(process.exited() && !process.running() && process.handle().pid == 0,
              "confirmed native stop retires the direct child handle");
        process.stop();
        check(process.exited(), "confirmed stop is idempotent");
        if (identity) {
            lemon::NativeProcess observed({false, false});
            observed.start({executable, {"--child"}});
            const auto recorded = observed.process_identity();
            check(recorded && recorded->token != identity->token, "a replacement child has a different kernel object identity");
            if (recorded) {
                ProcessManager::terminate_process(observed.handle());
                auto result = lemon::utils::ProcessIdentityState::running;
                const auto until = std::chrono::steady_clock::now() + std::chrono::seconds(5);
                while (result != lemon::utils::ProcessIdentityState::exited && std::chrono::steady_clock::now() < until) {
                    result = ProcessManager::observe_process_identity(*recorded);
                    std::this_thread::sleep_for(std::chrono::milliseconds(10));
                }
                check(result == lemon::utils::ProcessIdentityState::exited && observed.exited(),
                      "identity observation leaves the original owner's child status unconsumed");
                check(observed.collect_exit().has_value(), "original native owner still collects the observed exit");
            }
            observed.stop();
        }
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
    try {
        const nlohmann::json proof = {{"schema", 1}, {"reference_os", reference_os}, {"native_identity_supported", supported}, {"method", method},
            {"foreign_running", foreign_running}, {"foreign_exited", foreign_exited}, {"checks", checks}, {"failures", failures},
            {"scope", "Native direct-process identity only. Unsupported capability does not qualify prior-manager recovery; no descendant/device/remote termination proof."}};
        std::ofstream report("process-identity-proof.json"); report << proof.dump(2) << '\n';
        report.close();
        if (!report) throw std::runtime_error("Cannot retain process identity proof");
    } catch (const std::exception& error) { ++failures; std::fprintf(stderr, "Proof publication failed: %s\n", error.what()); }
    std::fprintf(stderr, "Process exit observation: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
