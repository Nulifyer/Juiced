#pragma once

#include <lemon/utils/aixlog.hpp>
#include <cerrno>
#include <chrono>
#include <csignal>
#include <initializer_list>
#include <sys/wait.h>
#include <thread>
#include <unistd.h>

namespace lemon::utils::detail {

inline bool owned_child_has_exited(int pid) {
#ifdef WNOWAIT
    if (pid <= 0) return false;
    siginfo_t status{};
    return waitid(P_PID, static_cast<id_t>(pid), &status, WEXITED | WNOHANG | WNOWAIT) == 0 &&
        status.si_pid == pid;
#else
    return false;
#endif
}

inline bool stop_owned_child(int pid) {
#ifdef WNOWAIT
    if (pid <= 0) return false;
    siginfo_t observed{};
    if (waitid(P_PID, static_cast<id_t>(pid), &observed, WEXITED | WNOHANG | WNOWAIT) != 0)
        return false;
    const auto confirmed = [pid](int status) {
        const int code = WIFEXITED(status) ? WEXITSTATUS(status) : WIFSIGNALED(status) ? 128 + WTERMSIG(status) : -1;
        LOG(INFO, "ProcessManager") << "Direct child PID " << pid << " exit observed, code " << code
                                   << "; retaining backend cooldown" << std::endl;
        std::this_thread::sleep_for(std::chrono::seconds(2));
        return true;
    };
    int status = 0;
    if (observed.si_pid == pid)
        return waitpid(pid, &status, WNOHANG) == pid && confirmed(status);
    for (const int signal : {SIGTERM, SIGKILL}) {
        if (::kill(pid, signal) != 0) return false;
        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(5);
        do {
            const auto result = waitpid(pid, &status, WNOHANG);
            if (result == pid) return confirmed(status);
            if (result < 0 && errno != EINTR) return false;
            std::this_thread::sleep_for(std::chrono::milliseconds(100));
        } while (std::chrono::steady_clock::now() < deadline);
    }
#endif
    return false;
}

} // namespace lemon::utils::detail
