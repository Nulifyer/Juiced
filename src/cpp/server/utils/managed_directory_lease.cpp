#include "lemon/utils/managed_directory_lease.h"
#include "lemon/utils/path_utils.h"

#include <algorithm>
#include <cerrno>
#include <iterator>
#include <stdexcept>
#include <system_error>

#ifdef _WIN32
#include <windows.h>
#else
#include <fcntl.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace lemon::utils {
namespace fs = std::filesystem;

#ifndef _WIN32
namespace {
bool contains(const fs::path& parent, const fs::path& child) {
    return std::mismatch(parent.begin(), parent.end(), child.begin(), child.end()).first == parent.end();
}
}
#endif

struct ManagedDirectoryLease::File {
    fs::path root;
    bool locked = false;
#ifdef _WIN32
    HANDLE handle = INVALID_HANDLE_VALUE;
    HANDLE directory_handle = INVALID_HANDLE_VALUE;
    ~File() {
        if (handle != INVALID_HANDLE_VALUE) {
            OVERLAPPED offset{};
            if (locked) UnlockFileEx(handle, 0, 1, 0, &offset);
            CloseHandle(handle);
        }
        if (directory_handle != INVALID_HANDLE_VALUE) CloseHandle(directory_handle);
    }
#else
    int fd = -1;
    int directory_fd = -1;
    std::vector<int> ancestor_fds;
    std::shared_ptr<File> parent;
    ~File() {
        if (fd >= 0) {
            if (locked) flock(fd, LOCK_UN);
            close(fd);
        }
        if (directory_fd >= 0) close(directory_fd);
        for (const auto ancestor : ancestor_fds) if (ancestor >= 0) close(ancestor);
    }
#endif
    void validate_identity() const {
        bool matches = false;
#ifdef _WIN32
        const auto current = CreateFileW(root.c_str(), FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
            OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, nullptr);
        if (current != INVALID_HANDLE_VALUE) {
            FILE_ID_INFO retained_info{}, current_info{};
            matches = GetFileInformationByHandleEx(directory_handle, FileIdInfo, &retained_info, sizeof(retained_info)) &&
                GetFileInformationByHandleEx(current, FileIdInfo, &current_info, sizeof(current_info)) &&
                retained_info.VolumeSerialNumber == current_info.VolumeSerialNumber &&
                std::equal(std::begin(retained_info.FileId.Identifier), std::end(retained_info.FileId.Identifier),
                           std::begin(current_info.FileId.Identifier));
            CloseHandle(current);
        }
#else
        const auto current = open(root.c_str(), O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        if (current >= 0) {
            struct stat retained_info{}, current_info{};
            matches = fstat(directory_fd, &retained_info) == 0 && fstat(current, &current_info) == 0 &&
                retained_info.st_dev == current_info.st_dev && retained_info.st_ino == current_info.st_ino;
            close(current);
        }
#endif
        if (!matches) throw std::runtime_error("Managed directory identity changed: " + path_to_utf8(root));
    }
};

fs::path ManagedDirectoryLease::resolve_root(const fs::path& root) {
    if (root.empty()) throw std::invalid_argument("Managed directories must be explicit");
    auto resolved = fs::weakly_canonical(fs::absolute(root));
    while (resolved.has_relative_path() && resolved.filename().empty()) resolved = resolved.parent_path();
    return resolved;
}

ManagedDirectoryLease::ManagedDirectoryLease(const std::vector<fs::path>& roots) {
#ifdef _WIN32
    for (const auto& root : roots) protect(root);
#else
    std::vector<fs::path> ordered;
    for (const auto& root : roots) ordered.push_back(resolve_root(root));
    std::stable_sort(ordered.begin(), ordered.end(), [](const auto& left, const auto& right) {
        return std::distance(left.begin(), left.end()) < std::distance(right.begin(), right.end());
    });
    for (const auto& root : ordered) protect(root);
#endif
}

ManagedDirectoryLease::~ManagedDirectoryLease() = default;

void ManagedDirectoryLease::validate() {
    std::lock_guard lock(mutex_);
    for (const auto& weak : files_)
        if (const auto file = weak.lock()) file->validate_identity();
}

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
    const auto canonical = resolve_root(root);
    std::lock_guard lock(mutex_);
    files_.erase(std::remove_if(files_.begin(), files_.end(), [](const auto& file) { return file.expired(); }), files_.end());
    for (const auto& weak : files_) {
        if (auto file = weak.lock(); file && (canonical == file->root ||
            (fs::exists(canonical) && fs::equivalent(canonical, file->root)))) {
            file->validate_identity();
            return Claim(std::move(file));
        }
    }
    auto file = std::make_shared<File>();
    file->root = canonical;
    const auto path = canonical / file_name;
    const auto label = path_to_utf8(canonical);
#ifdef _WIN32
    fs::create_directories(canonical);
    file->directory_handle = CreateFileW(canonical.c_str(), FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
        OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, nullptr);
    if (file->directory_handle == INVALID_HANDLE_VALUE)
        throw std::system_error(GetLastError(), std::system_category(), "Could not open managed directory: " + label);
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
    for (const auto& weak : files_) {
        const auto owned = weak.lock();
        if (owned && owned->root != canonical && contains(owned->root, canonical)) {
            owned->validate_identity();
            if (!file->parent || contains(file->parent->root, owned->root)) file->parent = owned;
        }
    }
    if (!file->parent) for (const auto& weak : files_) {
        if (const auto owned = weak.lock(); owned && contains(canonical, owned->root))
            throw std::runtime_error("Managed directory is already in use by a narrower claim: " + label);
    }
    const auto lock_fd = [&](int fd, int mode) {
        int result;
        do { result = flock(fd, mode | LOCK_NB); } while (result != 0 && errno == EINTR);
        if (result != 0) {
            const auto error = errno;
            if (error == EWOULDBLOCK || error == EAGAIN)
                throw std::runtime_error("Managed directory is already in use: " + label);
            throw std::system_error(error, std::generic_category(), "Could not lock managed directory: " + label);
        }
    };
    if (file->parent) {
        fs::create_directories(canonical);
    } else {
        // A missing descendant must not be created before its parent intent is held.
        fs::path ancestor;
        for (const auto& component : canonical) {
            ancestor /= component;
            if (ancestor == canonical) break;
            if (!fs::is_directory(ancestor)) fs::create_directory(ancestor);
            file->ancestor_fds.push_back(-1);
            auto& fd = file->ancestor_fds.back();
            fd = open(ancestor.c_str(), O_RDONLY | O_DIRECTORY | O_CLOEXEC);
            if (fd < 0) throw std::system_error(errno, std::generic_category(), "Could not open managed directory ancestor: " + label);
            lock_fd(fd, LOCK_SH);
        }
        if (!fs::is_directory(canonical)) fs::create_directory(canonical);
    }
    if (fs::canonical(canonical) != canonical)
        throw std::runtime_error("Managed directory changed during acquisition: " + label);
    // A lock on the directory survives replacement of its control-file inode.
    file->directory_fd = open(canonical.c_str(), O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (file->directory_fd < 0) {
        const auto error = errno;
        throw std::system_error(error, std::generic_category(), "Could not open managed directory: " + label);
    }
    lock_fd(file->directory_fd, LOCK_EX);
    file->fd = open(path.c_str(), O_CREAT | O_RDWR | O_CLOEXEC, 0600);
    if (file->fd < 0 && (errno == EACCES || errno == EROFS))
        file->fd = open(path.c_str(), O_RDONLY | O_CLOEXEC);
    if (file->fd < 0) {
        const auto error = errno;
        throw std::system_error(error, std::generic_category(), "Could not open managed directory lease: " + label);
    }
    lock_fd(file->fd, LOCK_EX);
#endif
    file->locked = true;
    files_.push_back(file);
    return Claim(std::move(file));
}

} // namespace lemon::utils
