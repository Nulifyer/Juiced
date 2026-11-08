#pragma once

#include <cstdint>
#include <filesystem>
#include <optional>
#include <string>
#include <vector>

namespace lemon {

// A constraint on a physical pool, not another pool to add to host RAM.
struct MemoryConstraint {
    std::optional<uint64_t> limit_bytes;
    std::optional<uint64_t> used_bytes;
    std::string source;
};

struct MemoryBudget {
    std::optional<uint64_t> host_total_bytes;
    std::optional<uint64_t> host_available_bytes;
    std::string source = "unavailable";
    std::string containment = "unknown";
    bool constraints_complete = false;
    std::vector<MemoryConstraint> constraints;

    // A measured upper bound, before model/workspace reservations. Unknown
    // constraints make this unknown. A measured zero stays zero.
    std::optional<uint64_t> available_bytes() const;
};

// Paths are injectable for fixture files. No shell commands or driver changes.
MemoryBudget read_linux_memory_budget(
    const std::filesystem::path& proc_root = "/proc",
    const std::filesystem::path& filesystem_root = "/");

} // namespace lemon
