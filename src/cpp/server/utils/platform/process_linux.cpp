#include "process_posix_lifecycle.h"
#include <lemon/utils/process_platform.h>
#include <lemon/utils/content_hash.h>
#include <lemon/utils/aixlog.hpp>

#include <stdexcept>
#include <exception>
#include <iostream>
#include <fstream>
#include <filesystem>
#include <set>
#include <sstream>

#include <unistd.h>
#include <sys/types.h>
#include <sys/syscall.h>
#include <poll.h>
#include <sys/wait.h>
#include <signal.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <fcntl.h>
#include <errno.h>
#include <cstring>
#include <thread>
#include <chrono>
#include <algorithm>
#include <cctype>

#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/sysmacros.h>
#include <linux/magic.h>

#ifdef HAVE_LIBCAP
#include <sys/capability.h>
#endif

namespace lemon::utils {

// Helper functions
static bool should_filter_line(const std::string& line) {
    return (line.find("GET /health") != std::string::npos ||
            line.find("GET /v1/health") != std::string::npos ||
            line.find("srv  update_slots: all slots are idle") != std::string::npos ||
            line.find("Enter 'exit' to stop the server") != std::string::npos);
}

static bool is_error_line(const std::string& line) {
    std::string lowered = line;
    std::transform(lowered.begin(), lowered.end(), lowered.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return lowered.find("error") != std::string::npos;
}

static void log_process_line(const std::string& line) {
    if (should_filter_line(line)) {
        return;
    }

    if (is_error_line(line)) {
        LOG(ERROR, "Process") << line << std::endl;
    } else {
        LOG(INFO, "Process") << line << std::endl;
    }
}

#ifdef HAVE_LIBCAP
static void preserve_capabilities_for_exec() {
    cap_t caps = cap_get_proc();
    if (!caps) {
        return;
    }

    cap_flag_value_t has_sys_resource = CAP_CLEAR;
    cap_get_flag(caps, CAP_SYS_RESOURCE, CAP_EFFECTIVE, &has_sys_resource);

    if (has_sys_resource == CAP_SET) {
        cap_value_t cap_list[] = {CAP_SYS_RESOURCE};

        if (cap_set_flag(caps, CAP_INHERITABLE, 1, cap_list, CAP_SET) == 0) {
            if (cap_set_proc(caps) == 0) {
                prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, CAP_SYS_RESOURCE, 0, 0);
            }
        }
    }

    cap_free(caps);
}
#endif

static bool is_zombie_by_proc(pid_t pid) {
    std::ifstream stat_file("/proc/" + std::to_string(pid) + "/stat");
    std::string stat_line;
    if (!std::getline(stat_file, stat_line)) {
        return false;
    }

    // Process state is the first field after the final ')' of comm.
    const auto close_paren = stat_line.rfind(')');
    return close_paren != std::string::npos &&
           close_paren + 2 < stat_line.size() &&
           stat_line[close_paren + 2] == 'Z';
}

class LinuxProcessPlatform : public ProcessPlatform {
public:
    ProcessHandle spawn(
        const std::string& executable,
        const std::vector<std::string>& args,
        const std::string& working_dir,
        bool inherit_output,
        bool filter_health_logs,
        const std::vector<std::pair<std::string, std::string>>& env_vars) override;

    bool terminate(ProcessHandle handle) override;
    bool has_exited(ProcessHandle handle) override { return detail::owned_child_has_exited(handle.pid); }
    bool is_running(ProcessHandle handle) override;
    bool owns_loopback_listener(ProcessHandle handle, int port) override;
    std::optional<ProcessImageFiles> image_files(ProcessHandle handle, const std::function<bool()>& cancelled) override;
    std::optional<ProcessIdentity> process_identity(ProcessHandle handle, const std::function<bool()>& cancelled) override;
    ProcessIdentityState observe_process_identity(const ProcessIdentity& identity, const std::function<bool()>& cancelled) override;
    int get_exit_code(ProcessHandle handle) override;
    int wait_for_exit(ProcessHandle handle, int timeout_seconds) override;
    int reap(ProcessHandle handle) override;
    void kill(ProcessHandle handle) override;
    void terminate_without_cleanup(ProcessHandle handle) override;

    int run_with_output(
        const std::string& executable,
        const std::vector<std::string>& args,
        OutputLineCallback on_line,
        const std::string& working_dir,
        int timeout_seconds,
        bool capture_stderr = true,
        const std::vector<std::pair<std::string, std::string>>& env_vars = {},
        const OutputProcessLifecycle* lifecycle = nullptr) override;

    int find_free_port(int start_port) override;
    int run_command(const std::string& command, std::string& output, int timeout_seconds) override;

protected:
    // To be overridden by platform-specific subclasses (macOS uses posix_spawn)
    virtual pid_t spawn_process(
        const std::string& executable,
        const std::vector<std::string>& args,
        const std::string& working_dir,
        bool inherit_output,
        bool filter_health_logs,
        const std::vector<std::pair<std::string, std::string>>& env_vars,
        int stdout_pipe[2],
        int stderr_pipe[2]);
};

// Linux implementation using fork/exec
pid_t LinuxProcessPlatform::spawn_process(
    const std::string& executable,
    const std::vector<std::string>& args,
    const std::string& working_dir,
    bool inherit_output,
    bool filter_health_logs,
    const std::vector<std::pair<std::string, std::string>>& env_vars,
    int stdout_pipe[2],
    int stderr_pipe[2]) {

    pid_t pid = fork();

    if (pid < 0) {
        throw std::runtime_error("Failed to fork process");
    }

    if (pid == 0) {
        // Child process
        prctl(PR_SET_PDEATHSIG, SIGTERM);

        if (!working_dir.empty()) {
            chdir(working_dir.c_str());
        }

        // Set environment variables
        for (const auto& env_pair : env_vars) {
            setenv(env_pair.first.c_str(), env_pair.second.c_str(), 1);
        }

        // Redirect stdout/stderr to pipes if filtering
        if (inherit_output && filter_health_logs) {
            close(stdout_pipe[0]);
            close(stderr_pipe[0]);
            dup2(stdout_pipe[1], STDOUT_FILENO);
            dup2(stderr_pipe[1], STDERR_FILENO);
            close(stdout_pipe[1]);
            close(stderr_pipe[1]);
        } else if (!inherit_output) {
            int dev_null = open("/dev/null", O_WRONLY);
            if (dev_null >= 0) {
                dup2(dev_null, STDOUT_FILENO);
                dup2(dev_null, STDERR_FILENO);
                close(dev_null);
            }
        }

#ifdef HAVE_LIBCAP
        preserve_capabilities_for_exec();
#endif

        // Prepare argv
        std::vector<char*> argv_ptrs;
        argv_ptrs.push_back(const_cast<char*>(executable.c_str()));
        for (const auto& arg : args) {
            argv_ptrs.push_back(const_cast<char*>(arg.c_str()));
        }
        argv_ptrs.push_back(nullptr);

        execvp(executable.c_str(), argv_ptrs.data());

        // If execvp returns, it failed
        std::cerr << "Failed to execute: " << executable << std::endl;
        _exit(1);
    }

    return pid;
}

ProcessHandle LinuxProcessPlatform::spawn(
    const std::string& executable,
    const std::vector<std::string>& args,
    const std::string& working_dir,
    bool inherit_output,
    bool filter_health_logs,
    const std::vector<std::pair<std::string, std::string>>& env_vars) {

    ProcessHandle handle;
    handle.handle = nullptr;
    handle.pid = 0;

    int stdout_pipe[2] = {-1, -1};
    int stderr_pipe[2] = {-1, -1};

    // Create pipes for filtering if requested
    if (inherit_output && filter_health_logs) {
        if (pipe(stdout_pipe) < 0 || pipe(stderr_pipe) < 0) {
            throw std::runtime_error("Failed to create pipes for output filtering");
        }
    }

    if (inherit_output) {
        std::string cmdline = executable;
        for (const auto& arg : args) {
            cmdline += " " + arg;
        }
        if (filter_health_logs) {
            LOG(DEBUG, "ProcessManager") << "Starting process with filtered output: " << cmdline << std::endl;
        } else {
            LOG(DEBUG, "ProcessManager") << "Starting process with inherited output: " << cmdline << std::endl;
        }
    }

    pid_t pid = spawn_process(executable, args, working_dir, inherit_output,
                              filter_health_logs, env_vars, stdout_pipe, stderr_pipe);

    handle.pid = pid;

    if (inherit_output) {
        LOG(INFO, "ProcessManager") << "Process started successfully, PID: " << pid << std::endl;
    }

    // Start filter threads if needed
    if (inherit_output && filter_health_logs) {
        close(stdout_pipe[1]);
        close(stderr_pipe[1]);

        std::thread([fd = stdout_pipe[0]]() {
            char buffer[4096];
            std::string line_buffer;
            ssize_t bytes_read;

            while ((bytes_read = read(fd, buffer, sizeof(buffer) - 1)) > 0) {
                buffer[bytes_read] = '\0';
                line_buffer += buffer;

                size_t pos;
                while ((pos = line_buffer.find('\n')) != std::string::npos) {
                    std::string line = line_buffer.substr(0, pos);
                    line_buffer = line_buffer.substr(pos + 1);
                    log_process_line(line);
                }
            }

            if (!line_buffer.empty()) {
                log_process_line(line_buffer);
            }

            close(fd);
        }).detach();

        std::thread([fd = stderr_pipe[0]]() {
            char buffer[4096];
            std::string line_buffer;
            ssize_t bytes_read;

            while ((bytes_read = read(fd, buffer, sizeof(buffer) - 1)) > 0) {
                buffer[bytes_read] = '\0';
                line_buffer += buffer;

                size_t pos;
                while ((pos = line_buffer.find('\n')) != std::string::npos) {
                    std::string line = line_buffer.substr(0, pos);
                    line_buffer = line_buffer.substr(pos + 1);
                    log_process_line(line);
                }
            }

            if (!line_buffer.empty()) {
                log_process_line(line_buffer);
            }

            close(fd);
        }).detach();
    }

    return handle;
}

bool LinuxProcessPlatform::terminate(ProcessHandle handle) {
    return detail::stop_owned_child(handle.pid);
}

bool LinuxProcessPlatform::is_running(ProcessHandle handle) {
    if (handle.pid <= 0) {
        return false;
    }

#ifdef WNOWAIT
    siginfo_t info;
    std::memset(&info, 0, sizeof(info));
    if (waitid(P_PID, static_cast<id_t>(handle.pid), &info, WEXITED | WNOHANG | WNOWAIT) == 0) {
        return info.si_pid == 0;
    }

    if (errno == ECHILD) {
        return false;
    }
#endif

    if (is_zombie_by_proc(handle.pid)) {
        return false;
    }

    errno = 0;
    return ::kill(handle.pid, 0) == 0 || errno == EPERM;
}

int LinuxProcessPlatform::get_exit_code(ProcessHandle handle) {
    if (handle.pid <= 0) {
        return -1;
    }

    int status = 0;
    pid_t result = waitpid(handle.pid, &status, WNOHANG);

    if (result == 0) {
        return -1;
    }

    if (result < 0) {
        return -1;
    }

    if (WIFEXITED(status)) {
        return WEXITSTATUS(status);
    }

    if (WIFSIGNALED(status)) {
        return 128 + WTERMSIG(status);
    }

    return -1;
}

int LinuxProcessPlatform::wait_for_exit(ProcessHandle handle, int timeout_seconds) {
    if (handle.pid <= 0) {
        return -1;
    }

    int status = 0;
    if (timeout_seconds < 0) {
        if (waitpid(handle.pid, &status, 0) <= 0) {
            return -1;
        }
        if (WIFEXITED(status)) {
            return WEXITSTATUS(status);
        }
        if (WIFSIGNALED(status)) {
            return 128 + WTERMSIG(status);
        }
        return -1;
    }

    for (int i = 0; i < timeout_seconds * 10; ++i) {
        pid_t result = waitpid(handle.pid, &status, WNOHANG);
        if (result > 0) {
            if (WIFEXITED(status)) {
                return WEXITSTATUS(status);
            }
            if (WIFSIGNALED(status)) {
                return 128 + WTERMSIG(status);
            }
            return -1;
        }
        if (result < 0) {
            return -1;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(100));
    }

    return -1;
}

int LinuxProcessPlatform::reap(ProcessHandle handle) {
    if (handle.pid <= 0) {
        return -1;
    }

    int status = 0;
    pid_t result = waitpid(handle.pid, &status, WNOHANG);
    if (result <= 0) {
        return -1;
    }

    if (WIFEXITED(status)) {
        return WEXITSTATUS(status);
    }

    if (WIFSIGNALED(status)) {
        return 128 + WTERMSIG(status);
    }

    return -1;
}

void LinuxProcessPlatform::kill(ProcessHandle handle) {
    if (handle.pid > 0) {
        errno = 0;
        if (::kill(handle.pid, SIGKILL) == 0 || errno != ESRCH) {
            int status = 0;
            waitpid(handle.pid, &status, 0);
        }
    }
}

void LinuxProcessPlatform::terminate_without_cleanup(ProcessHandle handle) {
    if (handle.pid > 0) {
        ::kill(handle.pid, SIGKILL);
    }
}

int LinuxProcessPlatform::run_with_output(
    const std::string& executable,
    const std::vector<std::string>& args,
    OutputLineCallback on_line,
    const std::string& working_dir,
    int timeout_seconds,
    bool capture_stderr,
    const std::vector<std::pair<std::string, std::string>>& env_vars,
    const OutputProcessLifecycle* lifecycle) {

    int stdout_pipe[2];

    if (pipe(stdout_pipe) < 0) {
        throw std::runtime_error("Failed to create pipe");
    }

    pid_t pid = fork();

    if (pid < 0) {
        close(stdout_pipe[0]);
        close(stdout_pipe[1]);
        throw std::runtime_error("Failed to fork process");
    }

    if (pid == 0) {
        // Child process
        prctl(PR_SET_PDEATHSIG, SIGTERM);
        close(stdout_pipe[0]);

        dup2(stdout_pipe[1], STDOUT_FILENO);
        if (capture_stderr) {
            dup2(stdout_pipe[1], STDERR_FILENO);
        }
        close(stdout_pipe[1]);

        if (!working_dir.empty()) {
            chdir(working_dir.c_str());
        }

#ifdef HAVE_LIBCAP
        preserve_capabilities_for_exec();
#endif

        for (const auto& env : env_vars) {
            if (setenv(env.first.c_str(), env.second.c_str(), 1) != 0)
                _exit(127);
        }

        std::vector<char*> argv_ptrs;
        argv_ptrs.push_back(const_cast<char*>(executable.c_str()));
        for (const auto& arg : args) {
            argv_ptrs.push_back(const_cast<char*>(arg.c_str()));
        }
        argv_ptrs.push_back(nullptr);

        execvp(executable.c_str(), argv_ptrs.data());
        _exit(127);
    }

    // Parent process
    close(stdout_pipe[1]);

    std::string line_buffer;
    char buffer[4096];
    ssize_t bytes_read;
    bool killed_by_callback = false;

    auto start_time = std::chrono::steady_clock::now();

    std::exception_ptr failure;
    try {
        if (lifecycle) lifecycle->attach({nullptr, static_cast<int>(pid)});
        const int flags = fcntl(stdout_pipe[0], F_GETFL, 0);
        if (flags < 0 || fcntl(stdout_pipe[0], F_SETFL, flags | O_NONBLOCK) < 0)
            throw std::runtime_error("Failed to make helper output nonblocking");
        while (true) {
            if (lifecycle && lifecycle->cancelled && lifecycle->cancelled()) { killed_by_callback = true; break; }
            if (timeout_seconds > 0 && std::chrono::steady_clock::now() - start_time >=
                                           std::chrono::seconds(timeout_seconds)) {
                killed_by_callback = true;
                break;
            }

            bytes_read = read(stdout_pipe[0], buffer, sizeof(buffer) - 1);

            if (bytes_read > 0) {
                buffer[bytes_read] = '\0';
                line_buffer += buffer;
                if (lifecycle && line_buffer.size() > OutputProcessLifecycle::maximum_line_bytes) throw std::runtime_error("Helper output line exceeds 64 KiB");

                size_t pos;
                while (true) {
                    size_t newline_pos = line_buffer.find('\n');
                    size_t cr_pos = line_buffer.find('\r');

                    if (newline_pos == std::string::npos && cr_pos == std::string::npos) {
                        break;
                    }

                    if (newline_pos == std::string::npos) {
                        pos = cr_pos;
                    } else if (cr_pos == std::string::npos) {
                        pos = newline_pos;
                    } else {
                        pos = std::min(newline_pos, cr_pos);
                    }

                    std::string line = line_buffer.substr(0, pos);

                    size_t skip = 1;
                    if (pos + 1 < line_buffer.size() &&
                        line_buffer[pos] == '\r' && line_buffer[pos + 1] == '\n') {
                        skip = 2;
                    }
                    line_buffer = line_buffer.substr(pos + skip);

                    if (line.empty()) {
                        continue;
                    }

                    if (on_line && !on_line(line)) {
                        killed_by_callback = true;
                        break;
                    }
                }

                if (killed_by_callback) break;
            } else if (bytes_read == 0) {
                if (!line_buffer.empty()) {
                    const std::string trailing = std::move(line_buffer);
                    line_buffer.clear();
                    if (on_line && !on_line(trailing)) { killed_by_callback = true; break; }
                }
                if (has_exited({nullptr, static_cast<int>(pid)})) break;
                std::this_thread::sleep_for(std::chrono::milliseconds(10));
            } else {
                if (errno == EAGAIN || errno == EWOULDBLOCK) {
                    if (has_exited({nullptr, static_cast<int>(pid)})) {
                        while ((bytes_read = read(stdout_pipe[0], buffer, sizeof(buffer) - 1)) > 0) {
                            buffer[bytes_read] = '\0';
                            line_buffer += buffer;
                            if (lifecycle && line_buffer.size() > OutputProcessLifecycle::maximum_line_bytes) throw std::runtime_error("Helper output line exceeds 64 KiB");
                        }
                        break;
                    }

                    std::this_thread::sleep_for(std::chrono::milliseconds(10));
                } else if (errno != EINTR) {
                    throw std::runtime_error("Failed to read helper output");
                }
            }
        }

        if (!line_buffer.empty() && on_line && !killed_by_callback) {
            if (!on_line(line_buffer)) killed_by_callback = true;
        }

    } catch (...) { failure = std::current_exception(); killed_by_callback = true; }
    close(stdout_pipe[0]);
    if (lifecycle) {
        const int result = lifecycle->finish(killed_by_callback);
        if (failure) std::rethrow_exception(failure);
        return result;
    }
    if (failure) {
        if (!terminate({nullptr, static_cast<int>(pid)}))
            throw std::runtime_error("Helper output failed and direct-child cleanup remains unconfirmed");
        std::rethrow_exception(failure);
    }
    if (killed_by_callback) {
        terminate_without_cleanup({nullptr, static_cast<int>(pid)});
        if (wait_for_exit({nullptr, static_cast<int>(pid)}, 5) < 0)
            throw std::runtime_error("Cancelled helper direct-child cleanup remains unconfirmed");
        return -1;
    }
    int status = 0;
    pid_t collected;
    do { collected = waitpid(pid, &status, WNOHANG); } while (collected < 0 && errno == EINTR);
    if (collected != pid) return -1;

    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

bool LinuxProcessPlatform::owns_loopback_listener(ProcessHandle handle, int port) {
    if (!is_running(handle)) return false;
    try {
        const auto root = std::filesystem::path("/proc") / std::to_string(handle.pid);
        if (std::filesystem::read_symlink(root / "ns/net") !=
            std::filesystem::read_symlink("/proc/self/ns/net")) return false;
        std::set<std::string> sockets;
        size_t descriptors = 0;
        for (const auto& entry : std::filesystem::directory_iterator(root / "fd")) {
            if (++descriptors > 65536) return false;
            std::error_code error;
            const auto link = std::filesystem::read_symlink(entry.path(), error).string();
            if (!error && link.rfind("socket:[", 0) == 0 && link.back() == ']')
                sockets.insert(link.substr(8, link.size() - 9));
        }
        std::ifstream table(root / "net/tcp");
        if (!table) return false;
        std::string line;
        size_t bytes = 0;
        bool owned = false;
        std::getline(table, line);
        while (std::getline(table, line)) {
            bytes += line.size();
            if (bytes > 16 * 1024 * 1024) return false;
            std::istringstream row(line);
            std::string index, local, remote, state, queues, timer, retransmits, uid, timeout, inode;
            if (!(row >> index >> local >> remote >> state >> queues >> timer >> retransmits >> uid >> timeout >> inode)) return false;
            const auto colon = local.find(':');
            if (state != "0A" || colon == std::string::npos || std::stoul(local.substr(colon + 1), nullptr, 16) != unsigned(port)) continue;
            const auto address = std::stoul(local.substr(0, colon), nullptr, 16);
            if (address != INADDR_ANY && address != htonl(INADDR_LOOPBACK)) continue;
            if (!sockets.count(inode)) return false;
            owned = true;
        }
        return table.eof() && owned && is_running(handle);
    } catch (const std::exception&) { return false; }
}

namespace {
struct IdentityDescriptor {
    int value;
    explicit IdentityDescriptor(int descriptor) : value(descriptor) {}
    IdentityDescriptor(const IdentityDescriptor&) = delete;
    IdentityDescriptor& operator=(const IdentityDescriptor&) = delete;
    ~IdentityDescriptor() { if (value >= 0) ::close(value); }
};

std::optional<std::string> process_identity_domain() {
    struct statfs filesystem{};
    if (::statfs("/proc", &filesystem) || filesystem.f_type != PROC_SUPER_MAGIC) return std::nullopt;
    const auto self = std::filesystem::read_symlink("/proc/self").string();
    if (self != std::to_string(::getpid())) return std::nullopt;
    IdentityDescriptor boot{::open("/proc/sys/kernel/random/boot_id", O_RDONLY | O_CLOEXEC | O_NONBLOCK | O_NOFOLLOW)};
    if (boot.value < 0 || ::fstatfs(boot.value, &filesystem) || filesystem.f_type != PROC_SUPER_MAGIC) return std::nullopt;
    char bytes[128];
    const auto count = ::read(boot.value, bytes, sizeof(bytes));
    if (count != 37 || bytes[36] != '\n') return std::nullopt;
    const std::string boot_id(bytes, 36);
    for (size_t i = 0; i < boot_id.size(); ++i)
        if ((i == 8 || i == 13 || i == 18 || i == 23) ? boot_id[i] != '-' : !std::isxdigit(static_cast<unsigned char>(boot_id[i])))
            return std::nullopt;
    IdentityDescriptor space{::open("/proc/self/ns/pid", O_RDONLY | O_CLOEXEC)};
    struct stat identity{};
    if (space.value < 0 || ::fstatfs(space.value, &filesystem) || filesystem.f_type != NSFS_MAGIC ||
        ::fstat(space.value, &identity) || !identity.st_ino) return std::nullopt;
    return string_sha256("linux-pidfs-64-v1\n" + boot_id + "\n" +
        std::to_string(identity.st_dev) + ":" + std::to_string(identity.st_ino));
}

std::optional<std::string> process_identity_token(int descriptor) {
    // pidfs gives each process a never-repeated inode within this kernel boot.
    // Anonymous-inode pidfds and truncated 32-bit inode values cannot prove that.
    constexpr long pidfs_magic = 0x50494446;
    struct statfs filesystem{};
    struct stat identity{};
    if (sizeof(ino_t) < 8 || ::fstatfs(descriptor, &filesystem) || filesystem.f_type != pidfs_magic ||
        ::fstat(descriptor, &identity) || !identity.st_ino) return std::nullopt;
    return string_sha256(std::to_string(identity.st_dev) + ":" + std::to_string(identity.st_ino));
}

bool identity_hash(const std::string& value) {
    return value.size() == 64 && std::all_of(value.begin(), value.end(), [](unsigned char c) {
        return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f');
    });
}
}

std::optional<ProcessIdentity> LinuxProcessPlatform::process_identity(
    ProcessHandle handle, const std::function<bool()>& cancelled) {
#ifdef SYS_pidfd_open
    try {
        if ((cancelled && cancelled()) || !is_running(handle)) return std::nullopt;
        const auto domain = process_identity_domain();
        if (!domain) return std::nullopt;
        IdentityDescriptor descriptor{static_cast<int>(::syscall(SYS_pidfd_open, handle.pid, 0))};
        if (descriptor.value < 0) return std::nullopt;
        const auto token = process_identity_token(descriptor.value);
        if (!token || (cancelled && cancelled()) || !is_running(handle) || process_identity_domain() != domain) return std::nullopt;
        return ProcessIdentity{"linux-pidfs-64-v1", handle.pid, *domain, *token};
    } catch (const std::exception&) { return std::nullopt; }
#endif
    return std::nullopt;
}

ProcessIdentityState LinuxProcessPlatform::observe_process_identity(
    const ProcessIdentity& identity, const std::function<bool()>& cancelled) {
#ifdef SYS_pidfd_open
    try {
        if (identity.method != "linux-pidfs-64-v1" || identity.pid <= 0 || !identity_hash(identity.domain) ||
            !identity_hash(identity.token) || (cancelled && cancelled())) return ProcessIdentityState::unknown;
        const auto domain = process_identity_domain();
        if (!domain || *domain != identity.domain) return ProcessIdentityState::unknown;
        IdentityDescriptor descriptor{static_cast<int>(::syscall(SYS_pidfd_open, identity.pid, 0))};
        if (descriptor.value < 0) {
            const bool absent = errno == ESRCH;
            return absent && !(cancelled && cancelled()) && process_identity_domain() == domain ?
                ProcessIdentityState::exited : ProcessIdentityState::unknown;
        }
        const auto token = process_identity_token(descriptor.value);
        if (!token) return ProcessIdentityState::unknown;
        pollfd event{descriptor.value, POLLIN, 0};
        if (::poll(&event, 1, 0) < 0 || (event.revents & (POLLERR | POLLNVAL))) return ProcessIdentityState::unknown;
        const bool exited = *token != identity.token || (event.revents & (POLLIN | POLLHUP));
        if ((cancelled && cancelled()) || process_identity_domain() != domain) return ProcessIdentityState::unknown;
        return exited ? ProcessIdentityState::exited : ProcessIdentityState::running;
    } catch (const std::exception&) { return ProcessIdentityState::unknown; }
#endif
    return ProcessIdentityState::unknown;
}

std::optional<ProcessImageFiles> LinuxProcessPlatform::image_files(
    ProcessHandle handle, const std::function<bool()>& cancelled) {
    if (!is_running(handle)) return std::nullopt;
    try {
        const auto check_cancelled = [&] {
            if (cancelled && cancelled()) throw std::runtime_error("Runtime image inspection cancelled");
        };
        check_cancelled();
        const auto root = std::filesystem::path("/proc") / std::to_string(handle.pid);
        const auto mount_namespace = std::filesystem::read_symlink(root / "ns/mnt");
        if (mount_namespace != std::filesystem::read_symlink("/proc/self/ns/mnt")) return std::nullopt;
        struct stat child_root{}, manager_root{};
        if (::stat((root / "root").c_str(), &child_root) || ::stat("/", &manager_root) ||
            child_root.st_dev != manager_root.st_dev || child_root.st_ino != manager_root.st_ino) return std::nullopt;
        const auto signature = [](const struct stat& file) {
            std::ostringstream value;
            value << file.st_dev << ':' << file.st_ino << ':' << file.st_size << ':'
                  << file.st_mtim.tv_sec << ':' << file.st_mtim.tv_nsec << ':'
                  << file.st_ctim.tv_sec << ':' << file.st_ctim.tv_nsec;
            return value.str();
        };
        const auto executable = std::filesystem::read_symlink(root / "exe");
        const auto valid_path = [](const std::filesystem::path& path) {
            const auto text = path.string();
            return path.is_absolute() && text.size() <= 16384 &&
                (text.size() < 10 || text.compare(text.size() - 10, 10, " (deleted)"));
        };
        if (!valid_path(executable)) return std::nullopt;
        struct stat executable_file{}, executable_link{};
        if (::stat(executable.c_str(), &executable_file) || ::stat((root / "exe").c_str(), &executable_link) ||
            !S_ISREG(executable_file.st_mode) || signature(executable_file) != signature(executable_link)) return std::nullopt;

        const auto read_proc = [&](const char* name, size_t maximum) -> std::optional<std::string> {
            struct FileDescriptor {
                int value;
                ~FileDescriptor() { if (value >= 0) ::close(value); }
            } descriptor{::open((root / name).c_str(), O_RDONLY | O_CLOEXEC)};
            if (descriptor.value < 0) return std::nullopt;
            std::string contents;
            char block[65536];
            for (;;) {
                check_cancelled();
                const auto count = ::read(descriptor.value, block, sizeof(block));
                if (count < 0 && errno == EINTR) continue;
                if (count < 0) return std::nullopt;
                if (!count) break;
                if (static_cast<size_t>(count) > maximum - contents.size()) return std::nullopt;
                contents.append(block, static_cast<size_t>(count));
            }
            return contents;
        };
        // procfs reads can be short. Inventory comparisons still cannot make
        // independently read mappings and initial exec environment atomic.
        const auto contents = read_proc("maps", 4 * 1024 * 1024);
        const auto environment = read_proc("environ", 2 * 1024 * 1024);
        if (!contents || contents->empty() || !environment) return std::nullopt;

        struct Mount { std::string path, type; unsigned long device_major, device_minor; };
        std::vector<Mount> mounts;
        bool mounts_loaded = false, used_btrfs = false;
        const auto mount_path = [](const std::string& encoded) {
            std::string decoded;
            for (size_t i = 0; i < encoded.size(); ++i) {
                if (encoded[i] == '\\' && i + 3 < encoded.size() && encoded[i + 1] >= '0' && encoded[i + 1] <= '7' &&
                    encoded[i + 2] >= '0' && encoded[i + 2] <= '7' && encoded[i + 3] >= '0' && encoded[i + 3] <= '7') {
                    decoded += char((encoded[i + 1] - '0') * 64 + (encoded[i + 2] - '0') * 8 + encoded[i + 3] - '0');
                    i += 3;
                } else decoded += encoded[i];
            }
            return decoded;
        };
        const auto btrfs_device_matches = [&](const std::string& path, unsigned long dev_major, unsigned long dev_minor) {
            struct statfs filesystem{};
            if (::statfs(path.c_str(), &filesystem) || filesystem.f_type != BTRFS_SUPER_MAGIC) return false;
            if (!mounts_loaded) {
                mounts_loaded = true;
                std::ifstream input("/proc/self/mountinfo");
                std::string line;
                size_t bytes = 0;
                while (std::getline(input, line)) {
                    check_cancelled();
                    bytes += line.size();
                    if (bytes > 1024 * 1024 || mounts.size() >= 4096) return false;
                    std::istringstream row(line);
                    std::string id, parent, device, origin, point, options, field;
                    if (!(row >> id >> parent >> device >> origin >> point >> options)) return false;
                    while (row >> field && field != "-") {}
                    std::string type;
                    if (field != "-" || !(row >> type)) return false;
                    const auto colon = device.find(':');
                    if (colon == std::string::npos) return false;
                    mounts.push_back({mount_path(point), type, std::stoul(device.substr(0, colon)), std::stoul(device.substr(colon + 1))});
                }
                if (!input.eof()) return false;
            }
            const Mount* governing = nullptr;
            for (const auto& mount : mounts) {
                if ((mount.path == "/" || path == mount.path || path.rfind(mount.path + "/", 0) == 0) &&
                    (!governing || mount.path.size() > governing->path.size())) governing = &mount;
            }
            if (!governing || governing->type != "btrfs" || governing->device_major != dev_major || governing->device_minor != dev_minor) return false;
            used_btrfs = true;
            return true;
        };

        ProcessImageFiles result;
        result.executable = std::filesystem::canonical(executable).string();
        result.namespace_identity = mount_namespace.string() + ':' + std::to_string(child_root.st_dev) + ':' + std::to_string(child_root.st_ino);
        std::map<std::string, std::string> file_signatures;
        const auto directory = std::filesystem::read_symlink(root / "cwd");
        if (!valid_path(directory)) return std::nullopt;
        result.working_directory = std::filesystem::canonical(directory).string();
        size_t begin = 0;
        while (begin < environment->size()) {
            check_cancelled();
            const auto end = environment->find('\0', begin);
            const auto entry = environment->substr(begin, end == std::string::npos ? end : end - begin);
            const auto equals = entry.find('=');
            if (equals == std::string::npos || equals == 0 || equals > 256 || entry.size() > 65536 || result.environment.size() >= 4096 ||
                !result.environment.emplace(entry.substr(0, equals), entry.substr(equals + 1)).second) return std::nullopt;
            if (end == std::string::npos) break;
            begin = end + 1;
        }
        std::istringstream input(*contents);
        std::string line;
        size_t rows = 0;
        while (std::getline(input, line)) {
            check_cancelled();
            if (++rows > 65536) return std::nullopt;
            std::istringstream row(line);
            std::string range, permissions, offset, device, inode, pathname;
            if (!(row >> range >> permissions >> offset >> device >> inode)) return std::nullopt;
            if (permissions.size() != 4 || permissions[2] != 'x') continue;
            std::getline(row >> std::ws, pathname);
            if (inode == "0") {
                if (pathname == "[vdso]" || pathname == "[vsyscall]") continue;
                return std::nullopt;
            }
            if (permissions[1] == 'w' || result.files.size() >= 4096) return std::nullopt;
            const auto separator = range.find('-');
            if (separator == std::string::npos || separator == 0 || separator + 1 == range.size() ||
                range.substr(0, separator).find_first_not_of("0123456789abcdefABCDEF") != std::string::npos ||
                range.substr(separator + 1).find_first_not_of("0123456789abcdefABCDEF") != std::string::npos) return std::nullopt;
            const auto first = std::stoull(range.substr(0, separator), nullptr, 16);
            const auto last = std::stoull(range.substr(separator + 1), nullptr, 16);
            if (first >= last) return std::nullopt;
            // maps pads low addresses; map_files requires unpadded hex names.
            std::ostringstream mapping_name;
            mapping_name << std::hex << first << '-' << last;
            const auto mapping_file = root / "map_files" / mapping_name.str();
            const auto link = std::filesystem::read_symlink(mapping_file);
            if (!valid_path(link)) return std::nullopt;
            const auto path = std::filesystem::canonical(link);
            struct stat file{};
            if (::stat(path.c_str(), &file) || !S_ISREG(file.st_mode) || file.st_ino != std::stoull(inode)) return std::nullopt;
            const auto colon = device.find(':');
            if (colon == std::string::npos) return std::nullopt;
            const auto dev_major = std::stoul(device.substr(0, colon), nullptr, 16);
            const auto dev_minor = std::stoul(device.substr(colon + 1), nullptr, 16);
            if ((major(file.st_dev) != dev_major || minor(file.st_dev) != dev_minor) &&
                !btrfs_device_matches(path.string(), dev_major, dev_minor)) return std::nullopt;
            const auto observed = signature(file);
            const auto name = path.string();
            if (file_signatures.count(name) && file_signatures.at(name) != observed) return std::nullopt;
            file_signatures[name] = observed;
            result.files[name] += observed + '|' + range + '|' + permissions + '|' + offset + '|' + device + '|' + inode + '|' + link.string() + '\n';
            // The pathname came from the owned mapping, not a caller's file list.
            if (std::filesystem::read_symlink(mapping_file) != link) return std::nullopt;
        }
        if (result.files.empty() || !result.files.count(result.executable) || !is_running(handle) ||
            std::filesystem::read_symlink(root / "ns/mnt") != mount_namespace ||
            std::filesystem::read_symlink(root / "exe") != executable) return std::nullopt;
        result.method = used_btrfs ? "linux-procfs-map-files+btrfs-mount-v1" : "linux-procfs-map-files-v1";
        return result;
    } catch (const std::exception&) { return std::nullopt; }
}

int LinuxProcessPlatform::find_free_port(int start_port) {
    for (int port = start_port; port < start_port + 1000; ++port) {
        int sock = socket(AF_INET, SOCK_STREAM, 0);
        if (sock < 0) {
            continue;
        }

        sockaddr_in addr;
        addr.sin_family = AF_INET;
        addr.sin_port = htons(port);
        addr.sin_addr.s_addr = inet_addr("127.0.0.1");

        int result = bind(sock, reinterpret_cast<sockaddr*>(&addr), sizeof(addr));
        close(sock);

        if (result == 0) {
            return port;
        }
    }

    return -1;
}

int LinuxProcessPlatform::run_command(const std::string& command, std::string& output, int timeout_seconds) {
    output.clear();

    FILE* pipe = popen(command.c_str(), "r");
    if (!pipe) {
        return -1;
    }

    char buf[4096];
    while (fgets(buf, sizeof(buf), pipe)) {
        output += buf;
    }

    return pclose(pipe);
}

std::unique_ptr<ProcessPlatform> create_process_platform() {
    return std::make_unique<LinuxProcessPlatform>();
}

} // namespace lemon::utils
