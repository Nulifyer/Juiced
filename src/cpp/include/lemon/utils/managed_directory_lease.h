#pragma once

#include <filesystem>
#include <memory>
#include <mutex>
#include <utility>
#include <vector>

namespace lemon::utils {

class ManagedDirectoryLease {
    struct File;
public:
    class Claim {
    public:
        Claim() = default;
        explicit operator bool() const { return static_cast<bool>(file_); }
        std::filesystem::path root() const;
    private:
        friend class ManagedDirectoryLease;
        explicit Claim(std::shared_ptr<File> file) : file_(std::move(file)) {}
        std::shared_ptr<File> file_;
    };
    static constexpr const char* file_name = ".juiced-store.lock";
    static std::filesystem::path resolve_root(const std::filesystem::path& root);
    explicit ManagedDirectoryLease(const std::vector<std::filesystem::path>& roots);
    ~ManagedDirectoryLease();
    ManagedDirectoryLease(const ManagedDirectoryLease&) = delete;
    ManagedDirectoryLease& operator=(const ManagedDirectoryLease&) = delete;
    void protect(const std::filesystem::path& root);
    Claim claim(const std::filesystem::path& root);
    void validate();
private:
    std::mutex mutex_;
    std::vector<std::weak_ptr<File>> files_;
    std::vector<Claim> held_;
};

} // namespace lemon::utils
