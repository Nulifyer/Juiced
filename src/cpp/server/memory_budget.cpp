#include "lemon/memory_budget.h"

#include <algorithm>
#include <charconv>
#include <fstream>
#include <limits>
#include <sstream>

namespace lemon {
namespace {

std::optional<uint64_t> number(const std::string& text) {
    uint64_t value = 0;
    const auto result = std::from_chars(text.data(), text.data() + text.size(), value);
    if (text.empty() || result.ec != std::errc{} || result.ptr != text.data() + text.size())
        return std::nullopt;
    return value;
}

struct TokenRead {
    std::optional<std::string> value;
    bool missing = false;
};

TokenRead read_token(const std::filesystem::path& path) {
    std::ifstream file(path);
    if (!file.is_open()) {
        std::error_code error;
        const auto status = std::filesystem::status(path, error);
        return {std::nullopt, status.type() == std::filesystem::file_type::not_found &&
            (!error || error == std::errc::no_such_file_or_directory)};
    }
    std::string result, extra;
    if (!(file >> result) || file >> extra || file.bad()) return {};
    return {result, false};
}

std::optional<std::string> token(const std::filesystem::path& path) {
    return read_token(path).value;
}

bool contains(const std::string& list, const std::string& item) {
    std::istringstream stream(list);
    std::string value;
    while (std::getline(stream, value, ',')) if (value == item) return true;
    return false;
}

std::string unescape_mount(const std::string& text) {
    std::string result;
    for (size_t i = 0; i < text.size(); ++i) {
        if (text[i] == '\\' && i + 3 < text.size() &&
            text[i+1] >= '0' && text[i+1] <= '7' &&
            text[i+2] >= '0' && text[i+2] <= '7' &&
            text[i+3] >= '0' && text[i+3] <= '7') {
            result += static_cast<char>((text[i+1]-'0')*64 + (text[i+2]-'0')*8 + text[i+3]-'0');
            i += 3;
        } else result += text[i];
    }
    return result;
}

bool safe_absolute(const std::filesystem::path& path) {
    // These are Linux procfs paths even when fixtures run on Windows.
    if (path.generic_string().empty() || path.generic_string().front() != '/') return false;
    for (const auto& component : path) if (component == "..") return false;
    return true;
}

// Mount roots use host cgroup paths; membership may use a cgroup namespace.
std::optional<std::filesystem::path> relative_group(
    const std::filesystem::path& group, const std::filesystem::path& root) {
    if (!safe_absolute(group) || !safe_absolute(root)) return std::nullopt;
    if (group == "/") return std::filesystem::path{};
    const auto relative = group.lexically_relative(root);
    if (relative.empty()) return std::nullopt;
    for (const auto& component : relative) if (component == "..") return std::nullopt;
    return relative;
}

void read_constraints(MemoryBudget& budget, const std::filesystem::path& mount,
                      std::filesystem::path leaf, bool v2, bool hierarchy_root_visible) {
    budget.containment = v2 ? "cgroup-v2" : "cgroup-v1";
    budget.constraints_complete = true;
    for (;;) {
        const auto limit_read = read_token(leaf / (v2 ? "memory.max" : "memory.limit_in_bytes"));
        const auto& limit = limit_read.value;
        if (!limit) {
            const auto used = read_token(leaf / "memory.current");
            std::ifstream controllers(leaf / "cgroup.controllers");
            std::string controller;
            bool memory_controller = false;
            while (controllers >> controller)
                memory_controller = memory_controller || controller == "memory";
            // Only a visible hierarchy root genuinely lacks both memory files.
            // A subtree or an unreadable/malformed interface is not unlimited.
            if (!(v2 && leaf == mount && hierarchy_root_visible &&
                  limit_read.missing && used.missing && memory_controller && !controllers.bad()))
                budget.constraints_complete = false;
        } else {
            MemoryConstraint constraint;
            constraint.source = v2 ? "cgroup-v2-memory" : "cgroup-v1-memory";
            if (v2 && *limit == "max") {
                // This scope imposes no finite ceiling. Ancestors still may.
            } else if (const auto parsed = number(*limit)) {
                // v1 uses PAGE_COUNTER_MAX rounded to a page as unlimited.
                if (!v2 && *parsed >= static_cast<uint64_t>(INT64_MAX) - 65535) {
                    // Unlimited is not a measured capacity.
                } else constraint.limit_bytes = *parsed;
            } else budget.constraints_complete = false;
            const auto used = token(leaf / (v2 ? "memory.current" : "memory.usage_in_bytes"));
            if (used) constraint.used_bytes = number(*used);
            if (constraint.limit_bytes && !constraint.used_bytes)
                budget.constraints_complete = false;
            budget.constraints.push_back(std::move(constraint));
        }
        if (leaf == mount) break;
        const auto parent = leaf.parent_path();
        if (parent == leaf) { budget.constraints_complete = false; break; }
        // v1 ancestors account descendants only with hierarchical accounting.
        if (!v2) {
            const auto hierarchical = token(parent / "memory.use_hierarchy");
            if (!hierarchical || (*hierarchical != "0" && *hierarchical != "1")) {
                budget.constraints_complete = false;
                break;
            }
            if (*hierarchical == "0") break;
        }
        leaf = parent;
    }
}

} // namespace

std::optional<uint64_t> MemoryBudget::available_bytes() const {
    if (!constraints_complete || !host_total_bytes || !host_available_bytes ||
        *host_total_bytes == 0 || *host_available_bytes > *host_total_bytes)
        return std::nullopt;
    auto available = *host_available_bytes;
    for (const auto& constraint : constraints) {
        if (!constraint.limit_bytes) continue;
        if (!constraint.used_bytes) return std::nullopt;
        const auto free = *constraint.used_bytes >= *constraint.limit_bytes
            ? 0 : *constraint.limit_bytes - *constraint.used_bytes;
        available = (std::min)(available, free);
    }
    return available;
}

MemoryBudget read_linux_memory_budget(const std::filesystem::path& proc_root,
                                     const std::filesystem::path& filesystem_root) {
    MemoryBudget budget;
    budget.source = "proc-meminfo";
    std::ifstream meminfo(proc_root / "meminfo");
    std::string line;
    while (std::getline(meminfo, line)) {
        std::istringstream fields(line);
        std::string key, value, unit, extra;
        if (!(fields >> key >> value >> unit) || fields >> extra || unit != "kB") continue;
        const auto parsed = number(value);
        if (!parsed || *parsed > UINT64_MAX / 1024) continue;
        if (key == "MemTotal:") budget.host_total_bytes = *parsed * 1024;
        if (key == "MemAvailable:") budget.host_available_bytes = *parsed * 1024;
    }

    std::ifstream membership(proc_root / "self/cgroup");
    std::optional<std::string> v2_group, v1_group;
    bool any_group = false;
    while (std::getline(membership, line)) {
        const auto first = line.find(':'), second = line.find(':', first + 1);
        if (first == std::string::npos || second == std::string::npos) continue;
        any_group = true;
        const auto controllers = line.substr(first + 1, second - first - 1);
        if (line.substr(0, first) == "0" && controllers.empty()) v2_group = line.substr(second + 1);
        if (contains(controllers, "memory")) v1_group = line.substr(second + 1);
    }
    std::ifstream mounts(proc_root / "self/mountinfo");
    while (std::getline(mounts, line)) {
        const auto separator = line.find(" - ");
        if (separator == std::string::npos) continue;
        std::istringstream left(line.substr(0, separator)), right(line.substr(separator + 3));
        std::string id, parent, device, root, mount, options, type, source, super_options;
        if (!(left >> id >> parent >> device >> root >> mount >> options) ||
            !(right >> type >> source >> super_options)) continue;
        const bool v2 = type == "cgroup2";
        if (!v2 && (type != "cgroup" || !contains(super_options, "memory"))) continue;
        const auto& group = v2 ? v2_group : v1_group;
        if (!group) continue;
        const auto relative = relative_group(*group, unescape_mount(root));
        const std::filesystem::path mount_path = unescape_mount(mount);
        if (!relative || !safe_absolute(mount_path)) continue;
        const auto physical_mount = filesystem_root / mount_path.relative_path();
        const auto leaf = relative->empty() || *relative == "."
            ? physical_mount : physical_mount / *relative;
        read_constraints(budget, physical_mount, leaf, v2, unescape_mount(root) == "/");
        return budget;
    }
    // An absent/unreadable mount or membership is not an unlimited container.
    budget.containment = any_group ? "unresolved" : "unknown";
    return budget;
}

} // namespace lemon
