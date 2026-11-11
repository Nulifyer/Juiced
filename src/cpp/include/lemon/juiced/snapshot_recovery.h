#pragma once

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <functional>
#include <nlohmann/json.hpp>
#include <string>

namespace lemon {

// Binding checks are shared with startup. Other journal predicates are
// diagnostic and cannot establish writer termination or recovery authority.
namespace snapshot_metadata {
bool valid_id(const std::string& id);
bool valid_state_header(const nlohmann::json& state);
bool valid_binding(const nlohmann::json& binding);
bool valid_saved(const std::string& id, const nlohmann::json& entry);
bool valid_cleanup_key(const std::string& key);
// Diagnostic shape validation cannot certify any producer or writer termination.
bool valid_write(const std::string& pending, const nlohmann::json& entry);
std::string write_actor_binding(const std::string& pending, const nlohmann::json& entry, const std::string& store);
bool valid_actor_binding(const std::string& pending, const nlohmann::json& entry, const std::string& store);
}

struct SnapshotInspectionOptions {
    std::chrono::milliseconds timeout{std::chrono::seconds(5)};
    std::function<bool()> cancelled;
    // Sum of initially observed file sizes. Each native input additionally
    // permits one byte to detect growth. This is not a filesystem I/O quota.
    uint64_t maximum_hash_bytes = 64ULL * 1024 * 1024 * 1024;
    size_t maximum_records = 16384;
    size_t maximum_files = 16384;
    size_t maximum_issues = 256;
};

class SnapshotRecovery {
public:
    // This is a non-atomic observation of paths, never a writer fence or recovery
    // authorization. The deadline is cooperative and cannot interrupt blocked OS I/O.
    // Windows inspection supports canonical drive roots and refuses other root shapes.
    // Retained handles prevent path substitution from redirecting file reads.
    // No source path is created, written, or removed. Reads may update atime.
    // Windows ancestry handles can restrict directory changes during inspection;
    // leaf handles permit replacement. File-content writers are not fenced.
    static nlohmann::json inspect_file(const std::filesystem::path& state_file,
        const std::filesystem::path& root, const SnapshotInspectionOptions& options = {});
};

}
