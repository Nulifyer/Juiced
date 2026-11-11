#include "lemon/alias_manager.h"
#include "lemon/config_file.h"
#include "lemon/utils/aixlog.hpp"
#include "lemon/utils/content_hash.h"
#include "lemon/utils/json_utils.h"
#include "lemon/utils/model_name_utils.h"
#include "lemon/utils/path_utils.h"

#include <filesystem>
#include <fstream>
#include <mutex>
#include <nlohmann/json.hpp>
#include <stdexcept>
#include <system_error>

namespace fs = std::filesystem;
using json = nlohmann::json;

namespace lemon {

AliasManager::AliasManager(std::string cache_dir)
    : cache_dir_(std::move(cache_dir)) {
    if (!cache_dir_.empty()) {
        load();
    }
}

void AliasManager::set_cache_dir(const std::string& cache_dir) {
    auto operation = resource_admission_ ? resource_admission_->write_scope("aliases") : ResourceAdmission::WriteScope{};
    std::unique_lock writer(write_mutex_);
    auto candidate = read_aliases(cache_dir);
    std::unique_lock lock(mutex_);
    cache_dir_ = cache_dir;
    aliases_.swap(candidate);
}

std::string AliasManager::get_cache_dir() const {
    std::shared_lock lock(mutex_);
    return cache_dir_;
}

bool AliasManager::set_alias(const std::string& alias, const std::string& target, std::string& err_msg) {
    auto operation = resource_admission_ ? resource_admission_->write_scope("aliases") : ResourceAdmission::WriteScope{};
    std::string clean_alias = utils::normalize_model_name(alias);
    std::string clean_target = utils::normalize_model_name(target);

    if (clean_alias.empty()) {
        err_msg = "Alias name cannot be empty";
        return false;
    }
    if (clean_target.empty()) {
        err_msg = "Alias target cannot be empty";
        return false;
    }
    if (clean_alias == clean_target) {
        err_msg = "Alias cannot point to itself";
        return false;
    }

    std::unique_lock writer(write_mutex_);
    {
        auto temp_aliases = get_all_aliases();
        temp_aliases[clean_alias] = clean_target;

        // Cycle detection check
        std::string current = clean_target;
        std::unordered_set<std::string> visited;
        visited.insert(clean_alias);

        size_t depth = 0;
        while (temp_aliases.count(current)) {
            if (visited.count(current)) {
                err_msg = "Circular alias dependency detected: " + clean_alias + " -> " + clean_target;
                LOG(WARNING, "AliasManager") << err_msg << std::endl;
                return false;
            }
            visited.insert(current);
            current = temp_aliases[current];
            depth++;
            if (depth > MAX_ALIAS_CHAIN_DEPTH) {
                err_msg = "Alias resolution chain exceeded max depth of " + std::to_string(MAX_ALIAS_CHAIN_DEPTH);
                return false;
            }
        }

        save_locked(temp_aliases);
        std::unique_lock lock(mutex_);
        aliases_.swap(temp_aliases);
    }

    LOG(INFO, "AliasManager") << "Set alias: " << clean_alias << " -> " << clean_target << std::endl;
    return true;
}

bool AliasManager::remove_alias(const std::string& alias) {
    auto operation = resource_admission_ ? resource_admission_->write_scope("aliases") : ResourceAdmission::WriteScope{};
    const auto clean_alias = utils::normalize_model_name(alias);
    std::unique_lock writer(write_mutex_);
    auto candidate = get_all_aliases();
    if (!candidate.erase(clean_alias)) return false;
    save_locked(candidate);
    { std::unique_lock lock(mutex_); aliases_.swap(candidate); }
    LOG(INFO, "AliasManager") << "Removed alias: " << clean_alias << std::endl;
    return true;
}

std::optional<std::string> AliasManager::resolve_alias(const std::string& alias_or_name) const {
    std::string current = utils::normalize_model_name(alias_or_name);
    std::shared_lock lock(mutex_);

    auto it = aliases_.find(current);
    if (it == aliases_.end()) {
        return std::nullopt;
    }

    std::unordered_set<std::string> visited;
    visited.insert(current);
    size_t depth = 0;

    while (it != aliases_.end() && depth < MAX_ALIAS_CHAIN_DEPTH) {
        std::string next_target = it->second;
        if (visited.count(next_target)) {
            LOG(WARNING, "AliasManager") << "Circular alias chain detected starting from '" << alias_or_name << "'" << std::endl;
            return std::nullopt;
        }
        current = next_target;
        visited.insert(current);
        depth++;
        it = aliases_.find(current);
    }

    if (depth >= MAX_ALIAS_CHAIN_DEPTH && it != aliases_.end()) {
        LOG(WARNING, "AliasManager") << "Alias chain depth exceeded " << MAX_ALIAS_CHAIN_DEPTH << " for '" << alias_or_name << "'" << std::endl;
        return std::nullopt;
    }

    return current;
}

bool AliasManager::has_alias(const std::string& alias) const {
    std::string clean_alias = utils::normalize_model_name(alias);
    std::shared_lock lock(mutex_);
    return aliases_.find(clean_alias) != aliases_.end();
}

std::unordered_map<std::string, std::string> AliasManager::get_all_aliases() const {
    std::shared_lock lock(mutex_);
    return aliases_;
}

void AliasManager::load() {
    auto operation = resource_admission_ ? resource_admission_->write_scope("aliases") : ResourceAdmission::WriteScope{};
    std::unique_lock writer(write_mutex_);
    auto candidate = read_aliases(get_cache_dir());
    std::unique_lock lock(mutex_);
    aliases_.swap(candidate);
}

std::unordered_map<std::string, std::string> AliasManager::read_aliases(const std::string& current_cache_dir) {
    if (current_cache_dir.empty()) return {};
    ConfigFile::require_complete_directory(utils::get_config_dir());

    fs::path cache_path = utils::path_from_utf8(current_cache_dir);
    if (fs::exists(cache_path) && !fs::is_directory(cache_path))
        throw std::runtime_error("Alias cache must be a directory: " + current_cache_dir);
    fs::path alias_path = cache_path / "aliases.json";

    const auto status = fs::symlink_status(alias_path);
    if (!fs::exists(status)) return {};
    if (!fs::is_regular_file(status))
        throw std::runtime_error("Alias configuration must be a regular file");
    constexpr uint64_t maximum_bytes = 16 * 1024 * 1024;
    const auto size = fs::file_size(alias_path);
    if (size > maximum_bytes)
        throw AdmissionError("alias_configuration_recovery_required", "Alias configuration exceeds its 16 MiB read bound; preserve it for recovery", 503);
    std::string original(static_cast<size_t>(size), '\0');
    {
        std::ifstream file(alias_path, std::ios::binary);
        if (!file || !file.read(original.data(), static_cast<std::streamsize>(size)) || file.peek() != std::char_traits<char>::eof())
            throw std::runtime_error("Alias configuration changed or could not be read");
    }
    json document;
    std::unordered_map<std::string, std::string> loaded_aliases;
    size_t events = 0;
    try {
        // The parser treats a literal NUL as EOF, even with trailing bytes.
        // JSON permits NUL only as an escaped value within a string.
        if (original.find('\0') == std::string::npos) {
            document = json::parse(original, [&](int depth, json::parse_event_t, json&) {
                if (depth > 128 || ++events > 100000)
                    throw AdmissionError("alias_configuration_recovery_required", "Alias configuration exceeds its parse bound; preserve it for recovery", 503);
                return true;
            });
        }
        if (document.is_object()) for (auto& [key, value] : document.items()) {
            if (!value.is_string()) continue;
            const auto alias = utils::normalize_model_name(key);
            const auto target = utils::normalize_model_name(value.get<std::string>());
            if (!alias.empty() && !target.empty() && alias != target) loaded_aliases[alias] = target;
        }
    } catch (const json::exception&) {
        // Parser diagnostics can expose values from the damaged document.
        document = json();
    }

    // Prune cyclic dependencies loaded from disk
    std::unordered_map<std::string, std::string> sanitized_aliases;
    for (const auto& [alias_k, target_v] : loaded_aliases) {
        std::unordered_set<std::string> visited;
        visited.insert(alias_k);
        std::string curr = target_v;
        bool is_cyclic = false;
        size_t depth = 0;

        while (loaded_aliases.count(curr) && depth < MAX_ALIAS_CHAIN_DEPTH) {
            if (visited.count(curr)) {
                is_cyclic = true;
                break;
            }
            visited.insert(curr);
            curr = loaded_aliases.at(curr);
            depth++;
        }

        if (visited.count(curr) || depth >= MAX_ALIAS_CHAIN_DEPTH) {
            is_cyclic = true;
        }

        if (is_cyclic) {
            LOG(WARNING, "AliasManager") << "Pruning cyclic alias entry from disk: " << alias_k << " -> " << target_v << std::endl;
        } else {
            sanitized_aliases[alias_k] = target_v;
        }
    }

    if (document != json(sanitized_aliases)) {
        const auto digest = utils::string_sha256(original);
        const auto matches = [&](const fs::path& candidate) {
            return fs::is_regular_file(fs::symlink_status(candidate)) &&
                fs::file_size(candidate) == size && utils::content_sha256(candidate) == digest;
        };
        const auto validate_source = [&] {
            if (resource_admission_) resource_admission_->validate_storage();
            ConfigFile::require_complete_directory(utils::get_config_dir());
            if (!matches(alias_path))
                throw AdmissionError("alias_configuration_recovery_required", "Alias source changed during recovery; preserve it and retry", 503);
        };
        validate_source();
        auto backup = cache_path / "aliases.json.corrupted";
        if (fs::exists(fs::symlink_status(backup)) && !matches(backup))
            backup = cache_path / ("aliases.json.corrupted." + digest);
        if (fs::exists(fs::symlink_status(backup))) {
            if (!matches(backup))
                throw AdmissionError("alias_configuration_recovery_required", "Alias recovery backup is occupied or damaged; preserve it for recovery", 503);
        } else {
            if (fs::space(cache_path).available < 3 * size + 1024 * 1024)
                throw AdmissionError("alias_configuration_recovery_required", "Alias recovery needs space for its original backup and replacement", 503);
            utils::JsonUtils::save_text_to_file(original, utils::path_to_utf8(backup), [&](const auto&) {
                validate_source();
                if (fs::exists(fs::symlink_status(backup)))
                    throw std::runtime_error("Alias recovery backup appeared before publication");
            }, fs::perms::owner_read | fs::perms::owner_write);
            if (!matches(backup)) throw std::runtime_error("Alias recovery backup failed verification");
        }
        // The original pathname remains present until this single-file commit.
        // A failed commit retains both the original and its verified backup.
        persist_aliases_at(utils::path_to_utf8(alias_path), sanitized_aliases, validate_source);
        LOG(WARNING, "AliasManager") << "Recovered alias configuration; original bytes retained at "
                                     << utils::path_to_utf8(backup) << std::endl;
    }

    LOG(INFO, "AliasManager") << "Loaded " << sanitized_aliases.size() << " alias binding(s) from disk." << std::endl;
    return sanitized_aliases;
}

void AliasManager::save() {
    auto operation = resource_admission_ ? resource_admission_->write_scope("aliases") : ResourceAdmission::WriteScope{};
    std::unique_lock writer(write_mutex_);
    save_locked(get_all_aliases());
}

void AliasManager::save_locked(const std::unordered_map<std::string, std::string>& aliases) {
    const auto cache_dir = get_cache_dir();
    if (cache_dir.empty()) return;
    ConfigFile::require_complete_directory(utils::get_config_dir());
    const auto target = utils::path_to_utf8(utils::path_from_utf8(cache_dir) / "aliases.json");
    persist_aliases_at(target, aliases);
}

void AliasManager::persist_aliases_at(const std::string& target,
    const std::unordered_map<std::string, std::string>& aliases,
    const std::function<void()>& validate_source) {
    const auto validate = [&] {
        if (resource_admission_) resource_admission_->validate_storage();
        ConfigFile::require_complete_directory(utils::get_config_dir());
        if (validate_source) validate_source();
    };
    validate();
    if (persist_override_) persist_override_(target, aliases);
    else utils::JsonUtils::save_to_file(json(aliases), target, [&](const auto&) { validate(); },
        fs::perms::owner_read | fs::perms::owner_write);
}

} // namespace lemon
