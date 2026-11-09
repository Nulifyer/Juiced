#pragma once

#include <filesystem>
#include <memory>
#include <vector>

namespace lemon::utils {

class ManagedDirectoryLease {
public:
    static constexpr const char* file_name = ".juiced-store.lock";
    explicit ManagedDirectoryLease(const std::vector<std::filesystem::path>& roots);
    ~ManagedDirectoryLease();
    ManagedDirectoryLease(const ManagedDirectoryLease&) = delete;
    ManagedDirectoryLease& operator=(const ManagedDirectoryLease&) = delete;
    void protect(const std::filesystem::path& root);
private:
    struct File;
    std::vector<std::unique_ptr<File>> held_;
};

} // namespace lemon::utils
