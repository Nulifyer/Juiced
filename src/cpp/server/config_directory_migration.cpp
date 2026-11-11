#include "lemon/config_file.h"
#include "lemon/resource_admission.h"
#include "lemon/utils/content_hash.h"
#include "lemon/utils/json_utils.h"
#include "lemon/utils/managed_directory_lease.h"
#include "lemon/utils/path_utils.h"
#include "lemon/utils/path_platform.h"

#include <array>
#include <algorithm>
#include <chrono>
#include <fstream>
#include <map>

namespace lemon {
namespace {
namespace fs = std::filesystem;
constexpr const char* manifest_name = ".juiced-configuration-migration.json";
constexpr const char* generation_name = ".juiced-configuration-generation";
constexpr uint64_t maximum_file = 16 * 1024 * 1024, maximum_cohort = 64 * 1024 * 1024;
constexpr const char* alias_name = "aliases.json";
constexpr std::array<const char*, 7> metadata_names = {
    "config.json", "jobs.json", "recipe_options.json", "user_models.json",
    "execution_profiles.json", "native_helpers.json", alias_name};
[[noreturn]] void recovery(const std::string& reason) {
    throw AdmissionError("configuration_migration_recovery_required",
        "Configuration migration requires recovery (" + reason + "); preserve both directories and use the original complete configuration until resolved", 503);
}
bool present(const fs::path& path) { return fs::exists(fs::symlink_status(path)); }
std::string bounded_read(const fs::path& path, uint64_t maximum) {
    if (!fs::is_regular_file(fs::symlink_status(path))) recovery("expected a regular metadata file");
    const auto size = fs::file_size(path);
    if (size > maximum) recovery("metadata exceeds its migration bound");
    std::string text(static_cast<size_t>(size), '\0');
    std::ifstream input(path, std::ios::binary);
    if (!input || !input.read(text.data(), static_cast<std::streamsize>(size)) || input.peek() != EOF)
        recovery("metadata changed or could not be read");
    return text;
}
json object_from(const std::string& text) {
    size_t events = 0;
    try {
        auto value = json::parse(text, [&](int depth, json::parse_event_t, json&) {
            if (depth > 128 || ++events > 100000) recovery("metadata structure exceeds its migration bound");
            return true;
        });
        if (!value.is_object()) recovery("metadata is not a JSON object");
        return value;
    } catch (const json::exception&) {
        // Parser diagnostics can contain private configuration values.
        recovery("metadata is not valid JSON");
    }
}
bool digest(const json& value) {
    if (!value.is_string()) return false;
    const auto& text = value.get_ref<const std::string&>();
    return text.size() == 64 && std::all_of(text.begin(), text.end(), [](char c) {
        return (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f');
    });
}
json read_manifest(const fs::path& directory) {
    const auto manifest = object_from(bounded_read(directory / manifest_name, 65536));
    if (manifest.size() != 6 || !manifest.contains("version") || !manifest.at("version").is_number_integer() ||
        manifest.at("version") != 1 || !manifest.contains("source") || !manifest.at("source").is_string() ||
        !manifest.contains("cache_directory") || !manifest.at("cache_directory").is_string() ||
        !manifest.contains("destination") || !manifest.at("destination").is_string() ||
        manifest.at("destination") != utils::path_to_utf8(utils::ManagedDirectoryLease::resolve_root(directory)) ||
        !manifest.contains("state") || !manifest.at("state").is_string() ||
        (manifest.at("state") != "staging" && manifest.at("state") != "prepared" && manifest.at("state") != "active") ||
        !manifest.contains("files") || !manifest.at("files").is_object() || manifest.at("files").empty())
        recovery("unsupported or incomplete manifest");
    const auto source = utils::path_from_utf8(manifest.at("source").get<std::string>());
    if (!source.is_absolute() || source != utils::ManagedDirectoryLease::resolve_root(source) ||
        manifest.at("source") == manifest.at("destination")) recovery("invalid source identity");
    const auto cache = utils::path_from_utf8(manifest.at("cache_directory").get<std::string>());
    if (!cache.is_absolute() || cache != utils::ManagedDirectoryLease::resolve_root(cache)) recovery("invalid cache identity");
    if (manifest.at("files").contains(alias_name) && manifest.at("source") == manifest.at("cache_directory"))
        recovery("aliases already belong to the retained cache owner");
    uint64_t total = 0;
    for (const auto& [name, entry] : manifest.at("files").items()) {
        if (std::find(metadata_names.begin(), metadata_names.end(), name) == metadata_names.end() ||
            !entry.is_object() || entry.size() != 3 || !entry.contains("sha256") || !digest(entry.at("sha256")) ||
            !entry.contains("bytes") || !entry.at("bytes").is_number_unsigned() || entry.at("bytes").get<uint64_t>() > maximum_file ||
            !entry.contains("permissions") || !entry.at("permissions").is_number_unsigned() || entry.at("permissions").get<uint64_t>() > 07777)
            recovery("unsupported metadata entry");
        total += entry.at("bytes").get<uint64_t>();
        if (total > maximum_cohort) recovery("metadata cohort exceeds its migration bound");
    }
    return manifest;
}
bool has_metadata(const fs::path& root) {
    if (present(root / "juiced") || present(root / manifest_name) || present(root / generation_name)) return true;
    return std::any_of(metadata_names.begin(), metadata_names.end(), [&](const auto* name) { return present(root / name); });
}
fs::path target_for(const json& manifest, const std::string& role) {
    return utils::path_from_utf8(manifest.at(role == alias_name ? "cache_directory" : "destination").get<std::string>()) / role;
}
bool matches(const fs::path& path, const json& entry) {
    if (!present(path)) return false;
    const auto text = bounded_read(path, maximum_file);
    return text.size() == entry.at("bytes").get<uint64_t>() && utils::string_sha256(text) == entry.at("sha256");
}
void publish(const fs::path& target, const std::string& text, fs::perms permissions,
             utils::ManagedDirectoryLease& storage_owner) {
    storage_owner.validate();
    // Only used after a persisted migration intent establishes pathname ownership.
    auto temporary = target; temporary += ".migration-part";
    if (present(temporary) && !fs::is_regular_file(fs::symlink_status(temporary))) recovery("unexpected staging pathname");
    {
        std::ofstream output(temporary, std::ios::binary | std::ios::trunc);
        if (!output) recovery("could not stage metadata");
        fs::permissions(temporary, permissions);
        output.write(text.data(), static_cast<std::streamsize>(text.size()));
        output.flush(); output.close();
        if (!output) recovery("could not flush staged metadata");
    }
    storage_owner.validate();
    std::error_code error;
    if (!utils::atomic_replace_file(temporary, target, error)) recovery("could not publish metadata");
}
} // namespace

std::function<void(const std::string&)> ConfigFile::directory_migration_barrier_;

void ConfigFile::require_complete_directory(const std::string& directory) {
    const auto root = utils::path_from_utf8(directory);
    if (!present(root / manifest_name)) {
        if (present(root / generation_name)) recovery("generation has no manifest");
        return;
    }
    const auto manifest = read_manifest(root);
    if (manifest.at("state") != "active") recovery("destination generation is not active");
    for (const auto& [name, entry] : manifest.at("files").items())
        if (!fs::is_regular_file(fs::symlink_status(target_for(manifest, name)))) recovery("an active metadata file is missing or replaced");
}

void ConfigFile::migrate_directory(const std::string& cache_name, const std::string& destination_name,
                                  utils::ManagedDirectoryLease& storage_owner, const std::string& legacy_cache) {
    const auto cache = utils::ManagedDirectoryLease::resolve_root(utils::path_from_utf8(cache_name));
    const auto destination = utils::ManagedDirectoryLease::resolve_root(utils::path_from_utf8(destination_name));
    storage_owner.protect(cache); storage_owner.protect(destination); storage_owner.validate();
    const bool recorded = present(destination / manifest_name);
    json manifest = recorded ? read_manifest(destination) : json();
    auto source = cache;
    if (recorded) {
        if (manifest.at("cache_directory") != utils::path_to_utf8(cache)) recovery("cache owner changed since migration");
        source = utils::path_from_utf8(manifest.at("source").get<std::string>());
        if (!fs::is_directory(source)) {
            if (manifest.at("state") != "active") recovery("retained source root is missing");
            require_complete_directory(destination_name);
            return;
        }
        storage_owner.protect(source);
    } else {
        if (source == destination && has_metadata(source)) { require_complete_directory(destination_name); return; }
        if (!legacy_cache.empty()) {
            const auto old = utils::ManagedDirectoryLease::resolve_root(utils::path_from_utf8(legacy_cache));
            if (old != cache && fs::is_directory(old)) {
                storage_owner.protect(old);
                if (has_metadata(old)) {
                    if (has_metadata(cache)) recovery("two source generations need ownership resolution");
                    source = old;
                }
            }
        }
    }
    if (source == destination) { require_complete_directory(destination_name); return; }
    require_complete_directory(utils::path_to_utf8(source));
    const auto in_scope = [&](const std::string& role) { return role != alias_name || source != cache; };
    const auto destination_file = [&](const std::string& role) { return (role == alias_name ? cache : destination) / role; };
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(30);
    const auto observe = [&](const std::string& phase) {
        if (std::chrono::steady_clock::now() >= deadline) recovery("migration deadline exceeded");
        if (directory_migration_barrier_) directory_migration_barrier_(phase);
        if (std::chrono::steady_clock::now() >= deadline) recovery("migration deadline exceeded");
        storage_owner.validate();
    };
    const auto stage = destination / generation_name;
    // State and snapshots carry canonical bindings, imports and writer ownership.
    // A bootstrap metadata transport must not silently create a replacement state.
    if (present(source / "juiced")) recovery("path-bound Juiced state needs validated relocation");
    if (!recorded && present(destination / "juiced")) {
        for (const auto* name : metadata_names)
            if (in_scope(name) && present(source / name)) recovery("destination already owns Juiced state");
    }
    std::map<std::string, std::string> source_files;
    json entries = json::object();
    uint64_t total = 0;
    for (const auto* name : metadata_names) {
        if (!in_scope(name) || !present(source / name)) continue;
        observe("read-source");
        auto text = bounded_read(source / name, maximum_file);
        object_from(text);
        total += text.size();
        if (total > maximum_cohort) recovery("metadata cohort exceeds its migration bound");
        entries[name] = {{"bytes", uint64_t(text.size())}, {"sha256", utils::string_sha256(text)},
            {"permissions", uint32_t(fs::status(source / name).permissions()) & uint32_t(fs::perms::mask)}};
        source_files.emplace(name, std::move(text));
    }
    if (recorded) {
        for (const auto& [name, entry] : entries.items())
            if (!manifest.at("files").contains(name) || manifest.at("files").at(name) != entry)
                recovery("retained source generation changed");
        if (manifest.at("state") == "active") { require_complete_directory(destination_name); return; }
        if (entries != manifest.at("files")) recovery("incomplete retained source generation");
    } else {
        if (entries.empty()) return;
        if (present(stage) && (!fs::is_directory(fs::symlink_status(stage)) || !fs::is_empty(stage)))
            recovery("unowned generation directory");
        for (const auto* name : metadata_names) {
            if (!in_scope(name)) continue;
            if (present(fs::path(destination_file(name)).concat(".migration-part"))) recovery("unowned publication pathname");
            if (!present(destination_file(name))) continue;
            if (!entries.contains(name) || !matches(destination_file(name), entries.at(name)))
                recovery("destination contains a different generation");
        }
        manifest = {{"version", 1}, {"source", utils::path_to_utf8(source)}, {"destination", utils::path_to_utf8(destination)}, {"cache_directory", utils::path_to_utf8(cache)},
            {"state", "staging"}, {"files", entries}};
        observe("before-intent");
        utils::JsonUtils::save_to_file(manifest, utils::path_to_utf8(destination / manifest_name), fs::perms::owner_read | fs::perms::owner_write);
        observe("intent");
    }
    if (present(stage) && !fs::is_directory(fs::symlink_status(stage))) recovery("invalid generation directory");
    fs::create_directories(stage);
    fs::permissions(stage, fs::perms::owner_all);
    for (const auto& item : fs::directory_iterator(stage)) {
        const auto name = utils::path_to_utf8(item.path().filename());
        bool owned = false;
        for (const auto& [role, entry] : manifest.at("files").items())
            if (name == role || name == role + ".migration-part") owned = true;
        if (!owned || !fs::is_regular_file(item.symlink_status())) recovery("unowned generation contents");
    }
    for (const auto* name : metadata_names)
        if (in_scope(name) && present(destination_file(name)) && !manifest.at("files").contains(name)) recovery("destination gained foreign metadata");
    total = 0;
    for (const auto& [name, entry] : manifest.at("files").items()) {
        total += entry.at("bytes").get<uint64_t>();
        if (present(destination_file(name)) && !matches(destination_file(name), entry)) recovery("destination metadata changed before activation");
    }
    if (fs::space(destination).available < total * 2 + 1024 * 1024) recovery("insufficient migration headroom");
    if (manifest.at("files").contains(alias_name) &&
        fs::space(cache).available < manifest.at("files").at(alias_name).at("bytes").get<uint64_t>() + 1024 * 1024)
        recovery("insufficient alias migration headroom");
    for (const auto& [name, entry] : manifest.at("files").items()) {
        if (!matches(stage / name, entry)) {
            const auto original = source_files.find(name);
            if (original == source_files.end()) recovery("a missing staged file has no retained source");
            observe("before-stage:" + name);
            publish(stage / name, original->second, static_cast<fs::perms>(entry.at("permissions").get<uint32_t>()), storage_owner);
        }
        if (!matches(stage / name, entry)) recovery("staged metadata failed verification");
        observe("staged:" + name);
    }
    manifest["state"] = "prepared";
    observe("before-prepared");
    utils::JsonUtils::save_to_file(manifest, utils::path_to_utf8(destination / manifest_name));
    observe("prepared");
    for (const auto& [name, entry] : manifest.at("files").items()) {
        if (present(destination_file(name))) {
            if (!matches(destination_file(name), entry)) recovery("destination metadata changed before publication");
        } else {
            observe("before-publish:" + name);
            publish(destination_file(name), bounded_read(stage / name, maximum_file),
                static_cast<fs::perms>(entry.at("permissions").get<uint32_t>()), storage_owner);
        }
        observe("published:" + name);
    }
    observe("before-active");
    // Recheck the complete candidate at the commit point. The source remains
    // intact for recovery; later native writes belong only to the active target.
    for (const auto* name : metadata_names) {
        if (!in_scope(name)) continue;
        const bool expected = manifest.at("files").contains(name);
        if (present(source / name) != expected || present(destination_file(name)) != expected)
            recovery("generation membership changed before activation");
        if (expected && (!matches(destination_file(name), manifest.at("files").at(name)) ||
                         !matches(source / name, manifest.at("files").at(name)))) recovery("generation changed before activation");
    }
    manifest["state"] = "active";
    utils::JsonUtils::save_to_file(manifest, utils::path_to_utf8(destination / manifest_name));
    observe("active");
}
namespace utils {
// Startup entrypoints link this server-owned operation. Standalone path utility
// consumers do not gain server persistence or management dependencies.
void migrate_legacy_paths(const std::string& cache_dir,
                          const std::string& config_dir,
                          ManagedDirectoryLease& storage_owner) {
    const bool relocated_by_service =
        !get_environment_variable_utf8("CACHE_DIRECTORY").empty() ||
        !get_environment_variable_utf8("STATE_DIRECTORY").empty();
    ConfigFile::migrate_directory(cache_dir, config_dir, storage_owner,
        relocated_by_service ? create_path_platform()->get_legacy_cache_dir() : std::string());
}

} // namespace utils
} // namespace lemon
