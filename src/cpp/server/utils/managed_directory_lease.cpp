#include "lemon/utils/managed_directory_lease.h"
#include "lemon/utils/path_utils.h"

#include <cerrno>
#include <stdexcept>
#include <system_error>

#ifdef _WIN32
#include <windows.h>
#else
#include <fcntl.h>
#include <sys/file.h>
#include <unistd.h>
#endif

namespace lemon::utils {
namespace fs = std::filesystem;

struct ManagedDirectoryLease::File {
    fs::path root;
    bool locked = false;
#ifdef _WIN32
    HANDLE handle = INVALID_HANDLE_VALUE;
    ~File() {
        if (handle == INVALID_HANDLE_VALUE) return;
        OVERLAPPED offset{};
        if (locked) UnlockFileEx(handle, 0, 1, 0, &offset);
        CloseHandle(handle);
    }
#else
    int fd = -1;
    ~File() {
        if (fd < 0) return;
        if (locked) flock(fd, LOCK_UN);
        close(fd);
    }
#endif
};

ManagedDirectoryLease::ManagedDirectoryLease(const std::vector<fs::path>& roots) {
    for (const auto& root : roots) protect(root);
}

ManagedDirectoryLease::~ManagedDirectoryLease() = default;

void ManagedDirectoryLease::protect(const fs::path& root) {
    if (root.empty()) throw std::invalid_argument("Managed directories must be explicit");
    fs::create_directories(root);
    const auto canonical = fs::canonical(root);
    for (const auto& file : held_) if (fs::equivalent(canonical, file->root)) return;
    auto file = std::make_unique<File>();
    file->root = canonical;
    const auto path = canonical / file_name;
    const auto label = path_to_utf8(canonical);
#ifdef _WIN32
    file->handle = CreateFileW(path.c_str(), GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file->handle == INVALID_HANDLE_VALUE) {
        const auto error = GetLastError();
        throw std::system_error(error, std::system_category(), "Could not open managed directory lease: " + label);
    }
    OVERLAPPED offset{};
    if (!LockFileEx(file->handle, LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY, 0, 1, 0, &offset)) {
        const auto error = GetLastError();
        if (error == ERROR_LOCK_VIOLATION || error == ERROR_SHARING_VIOLATION)
            throw std::runtime_error("Managed directory is already in use: " + label);
        throw std::system_error(error, std::system_category(), "Could not lock managed directory: " + label);
    }
#else
    file->fd = open(path.c_str(), O_CREAT | O_RDWR | O_CLOEXEC, 0600);
    if (file->fd < 0) {
        const auto error = errno;
        throw std::system_error(error, std::generic_category(), "Could not open managed directory lease: " + label);
    }
    int result;
    do { result = flock(file->fd, LOCK_EX | LOCK_NB); } while (result != 0 && errno == EINTR);
    if (result != 0) {
        const auto error = errno;
        if (error == EWOULDBLOCK || error == EAGAIN)
            throw std::runtime_error("Managed directory is already in use: " + label);
        throw std::system_error(error, std::generic_category(), "Could not lock managed directory: " + label);
    }
#endif
    file->locked = true;
    held_.push_back(std::move(file));
}

} // namespace lemon::utils
