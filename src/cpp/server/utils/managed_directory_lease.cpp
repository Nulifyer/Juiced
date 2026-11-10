#include "lemon/utils/managed_directory_lease.h"
#include "lemon/utils/path_utils.h"

#include <algorithm>
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
    int directory_fd = -1;
    ~File() {
        if (fd >= 0) {
            if (locked) flock(fd, LOCK_UN);
            close(fd);
        }
        if (directory_fd >= 0) close(directory_fd);
    }
#endif
};

ManagedDirectoryLease::ManagedDirectoryLease(const std::vector<fs::path>& roots) {
    for (const auto& root : roots) protect(root);
}

ManagedDirectoryLease::~ManagedDirectoryLease() = default;

fs::path ManagedDirectoryLease::Claim::root() const {
    return file_ ? file_->root : fs::path{};
}

void ManagedDirectoryLease::protect(const fs::path& root) {
    auto acquired = claim(root);
    std::lock_guard lock(mutex_);
    if (std::none_of(held_.begin(), held_.end(), [&](const auto& held) { return held.file_ == acquired.file_; }))
        held_.push_back(std::move(acquired));
}

ManagedDirectoryLease::Claim ManagedDirectoryLease::claim(const fs::path& root) {
    if (root.empty()) throw std::invalid_argument("Managed directories must be explicit");
    fs::create_directories(root);
    const auto canonical = fs::canonical(root);
    std::lock_guard lock(mutex_);
    files_.erase(std::remove_if(files_.begin(), files_.end(), [](const auto& file) { return file.expired(); }), files_.end());
    for (const auto& weak : files_)
        if (auto file = weak.lock(); file && fs::equivalent(canonical, file->root)) return Claim(std::move(file));
    auto file = std::make_shared<File>();
    file->root = canonical;
    const auto path = canonical / file_name;
    const auto label = path_to_utf8(canonical);
#ifdef _WIN32
    file->handle = CreateFileW(path.c_str(), GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file->handle == INVALID_HANDLE_VALUE &&
        (GetLastError() == ERROR_ACCESS_DENIED || GetLastError() == ERROR_WRITE_PROTECT))
        file->handle = CreateFileW(path.c_str(), GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
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
    const auto lock_fd = [&](int fd) {
        int result;
        do { result = flock(fd, LOCK_EX | LOCK_NB); } while (result != 0 && errno == EINTR);
        if (result != 0) {
            const auto error = errno;
            if (error == EWOULDBLOCK || error == EAGAIN)
                throw std::runtime_error("Managed directory is already in use: " + label);
            throw std::system_error(error, std::generic_category(), "Could not lock managed directory: " + label);
        }
    };
    // A lock on the directory survives replacement of its control-file inode.
    file->directory_fd = open(canonical.c_str(), O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (file->directory_fd < 0) {
        const auto error = errno;
        throw std::system_error(error, std::generic_category(), "Could not open managed directory: " + label);
    }
    lock_fd(file->directory_fd);
    file->fd = open(path.c_str(), O_CREAT | O_RDWR | O_CLOEXEC, 0600);
    if (file->fd < 0 && (errno == EACCES || errno == EROFS))
        file->fd = open(path.c_str(), O_RDONLY | O_CLOEXEC);
    if (file->fd < 0) {
        const auto error = errno;
        throw std::system_error(error, std::generic_category(), "Could not open managed directory lease: " + label);
    }
    lock_fd(file->fd);
#endif
    file->locked = true;
    files_.push_back(file);
    return Claim(std::move(file));
}

} // namespace lemon::utils
