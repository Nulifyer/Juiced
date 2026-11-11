#pragma once

#include <cstddef>
#include <string>
#include <vector>
#include <functional>
#include <optional>
#include <map>

namespace lemon {
namespace utils {

struct ProcessHandle {
    void* handle;
    int pid;
};

struct ProcessIdentity {
    std::string method;
    int pid = 0;
    std::string domain;
    std::string token;
};

enum class ProcessIdentityState { unknown, running, exited };

struct ProcessImageFiles {
    std::string method;
    std::string executable;
    std::map<std::string, std::string> files;
    std::string namespace_identity;
    // Private verification facts; never serialize the raw exec environment.
    std::map<std::string, std::string> environment;
    std::string working_directory;
    bool operator==(const ProcessImageFiles& other) const {
        return method == other.method && executable == other.executable && files == other.files && namespace_identity == other.namespace_identity && environment == other.environment && working_directory == other.working_directory;
    }
};

// Returns true to continue, false to kill the process
using OutputLineCallback = std::function<bool(const std::string& line)>;

// An operation owner retains the handle before output callbacks run. finish()
// must confirm cleanup or retain ownership and throw. The observer outlives this
// synchronous exchange; its retained process may outlive the initiating request.
struct OutputProcessLifecycle {
    static constexpr std::size_t maximum_line_bytes = 64 * 1024;
    std::function<void(ProcessHandle)> attach;
    std::function<int(bool)> finish;
    std::function<bool()> cancelled;
};

class ProcessManager {
public:
    static ProcessHandle start_process(
        const std::string& executable,
        const std::vector<std::string>& args,
        const std::string& working_dir = "",
        bool inherit_output = false,
        bool filter_health_logs = false,
        const std::vector<std::pair<std::string, std::string>>& env_vars = {});

    // Blocks until process exits or callback returns false (which kills the process)
    // Returns exit code, or -1 if killed by callback
    static int run_process_with_output(
        const std::string& executable,
        const std::vector<std::string>& args,
        OutputLineCallback on_line,
        const std::string& working_dir = "",
        int timeout_seconds = -1,
        bool capture_stderr = true,
        const std::vector<std::pair<std::string, std::string>>& env_vars = {},
        const OutputProcessLifecycle* lifecycle = nullptr);

    // True only after the owned direct child's exit is observed and cleanup succeeds.
    static bool stop_process(ProcessHandle handle);
    static bool has_exited(ProcessHandle handle);

    // Must be non-mutating: on POSIX it must not reap the child, because
    // status/health checks may call this frequently.
    static bool is_running(ProcessHandle handle);
    static bool owns_loopback_listener(ProcessHandle handle, int port);
    static std::optional<ProcessImageFiles> image_files(ProcessHandle handle, const std::function<bool()>& cancelled = {});

    // Capture only through a current native owner. Observation neither signals
    // nor collects a process and does not establish descendant/device exit.
    static std::optional<ProcessIdentity> process_identity(ProcessHandle handle,
        const std::function<bool()>& cancelled = {});
    static ProcessIdentityState observe_process_identity(const ProcessIdentity& identity,
        const std::function<bool()>& cancelled = {});

    // POSIX callers should prefer reap_process() when they intentionally own
    // cleanup of an exited child.
    static int get_exit_code(ProcessHandle handle);
    static int wait_for_exit(ProcessHandle handle, int timeout_seconds = -1);

    // Reap/close an already-exited child/handle and return its exit code.
    // Returns -1 if the process is still running, is not owned, or the status
    // cannot be obtained. This is intentionally explicit so liveness checks stay
    // read-only while lifecycle cleanup can reliably remove zombies.
    static int reap_process(ProcessHandle handle);
    static std::optional<int> collect_exit(ProcessHandle handle);

    static std::string read_output(ProcessHandle handle, int max_bytes = 4096);

    // Kill process forcefully and wait/close owned handles.
    static void kill_process(ProcessHandle handle);

    // Ask the OS to terminate the process without reaping or closing handles.
    // Use this only when another owner will perform the normal lifecycle cleanup.
    static void terminate_process(ProcessHandle handle);

    // Replacement for system()/popen() that avoids console flashes in GUI apps.
    static int run_command(const std::string& command, std::string& output, int timeout_seconds = 30);

    static int find_free_port(int start_port = 8001);
};

} // namespace utils
} // namespace lemon
