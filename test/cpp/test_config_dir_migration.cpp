#include <stdexcept>
#include "lemon/error_types.h"
#include "lemon/utils/managed_directory_lease.h"
#include <chrono>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>

#ifdef _WIN32
#include <windows.h>
#endif

#include <lemon/utils/path_utils.h>
#include "lemon/utils/process_manager.h"
#include "lemon/jobs/job_manager.h"
#include "lemon/alias_manager.h"
#include <map>
#include "lemon/config_file.h"
#include "lemon/model_manager.h"
#include "lemon/runtime_config.h"
#include "lemon/utils/json_utils.h"
#include "support/test_state_isolation.h"

namespace fs = std::filesystem;
namespace lemon {
struct ConfigurationMigrationTestHook {
    static void barrier(std::function<void(const std::string&)> callback) {
        ConfigFile::directory_migration_barrier_ = std::move(callback);
    }
};
}

namespace {
int legacy_checks = 0;
void require(bool condition, int line) {
    ++legacy_checks;
    if (!condition) throw std::runtime_error("Configuration migration check failed at line " + std::to_string(line));
}
#define CHECK(condition) require((condition), __LINE__)
void migrate_json(const std::string& source, const std::string& destination) {
    lemon::utils::ManagedDirectoryLease owner({lemon::utils::path_from_utf8(source), lemon::utils::path_from_utf8(destination)});
    lemon::ConfigFile::migrate_directory(source, destination, owner);
}
void migrate_paths(const std::string& source, const std::string& destination) {
    lemon::utils::ManagedDirectoryLease owner({lemon::utils::path_from_utf8(source), lemon::utils::path_from_utf8(destination)});
    lemon::utils::migrate_legacy_paths(source, destination, owner);
}

class ScopedEnvVar {
public:
    ScopedEnvVar(std::string name, std::string value)
        : name_(std::move(name)) {
        const char* old = std::getenv(name_.c_str());
        if (old) {
            had_old_value_ = true;
            old_value_ = old;
        }
        set(value.c_str());
    }

    ~ScopedEnvVar() {
        if (had_old_value_) {
            set(old_value_.c_str());
        } else {
#ifdef _WIN32
            SetEnvironmentVariableA(name_.c_str(), nullptr);
#else
            unsetenv(name_.c_str());
#endif
        }
    }

private:
    void set(const char* value) {
#ifdef _WIN32
        _putenv_s(name_.c_str(), value);
#else
        setenv(name_.c_str(), value, 1);
#endif
    }

    std::string name_;
    std::string old_value_;
    bool had_old_value_ = false;
};

void write_text(const fs::path& path, const std::string& text) {
    fs::create_directories(path.parent_path());
    std::ofstream out(path);
    if (!out.good()) throw std::runtime_error("Cannot write migration fixture");
    out << text;
}

std::string read_text(const fs::path& path) {
    std::ifstream in(path);
    if (!in.good()) throw std::runtime_error("Cannot read migration fixture");
    return std::string(std::istreambuf_iterator<char>(in),
                       std::istreambuf_iterator<char>());
}

void test_standard_cache_layout_migrates_to_explicit_config_dir() {
    const auto unique = std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now().time_since_epoch())
                            .count();
    const fs::path root = fs::temp_directory_path() /
                          ("lemonade_config_dir_migration_" + std::to_string(unique));
    const fs::path cache_dir = root / ".cache" / "lemonade";
    const fs::path config_dir = root / ".config" / "lemonade";

    try {
        write_text(cache_dir / "config.json", "{\"port\":13305}\n");
        write_text(cache_dir / "user_models.json", "{\"demo\":{}}\n");
        write_text(cache_dir / "recipe_options.json", "{\"demo\":{\"ctx_size\":4096}}\n");
        write_text(cache_dir / "jobs.json", "{\"jobs\":[]}\n");

        migrate_json(cache_dir.string(),
                                                              config_dir.string());

        for (const char* filename : {"config.json",
                                     "jobs.json",
                                     "recipe_options.json",
                                     "user_models.json"}) {
            const fs::path old_path = cache_dir / filename;
            const fs::path new_path = config_dir / filename;
            CHECK(fs::exists(old_path));
            CHECK(fs::exists(new_path));
        }
        CHECK(read_text(config_dir / "config.json") == "{\"port\":13305}\n");
        CHECK(read_text(config_dir / "jobs.json") == "{\"jobs\":[]}\n");
    } catch (...) {
        fs::remove_all(root);
        throw;
    }

    fs::remove_all(root);
}

void test_identical_cache_and_config_dirs_stay_put() {
    const auto unique = std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now().time_since_epoch())
                            .count();
    const fs::path root = fs::temp_directory_path() /
                          ("lemonade_config_dir_custom_" + std::to_string(unique));
    const fs::path custom_dir = root / "portable-data";

    try {
        write_text(custom_dir / "config.json", "{\"host\":\"127.0.0.1\"}\n");

        migrate_json(custom_dir.string(),
                                                              custom_dir.string());

        CHECK(fs::exists(custom_dir / "config.json"));
        CHECK(read_text(custom_dir / "config.json") == "{\"host\":\"127.0.0.1\"}\n");
    } catch (...) {
        fs::remove_all(root);
        throw;
    }

    fs::remove_all(root);
}

void test_explicit_config_dir_is_honored() {
    const auto unique = std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now().time_since_epoch())
                            .count();
    const fs::path root = fs::temp_directory_path() /
                          ("lemonade_explicit_config_dir_" + std::to_string(unique));
    const fs::path cache_dir = root / "portable-cache";
    const fs::path config_dir = root / "portable-config";

    lemon::utils::set_cache_dir(cache_dir.string());
    lemon::utils::set_config_dir(config_dir.string());

    try {
        write_text(cache_dir / "config.json", "{\"host\":\"127.0.0.1\"}\n");

        CHECK(lemon::utils::get_cache_dir() == cache_dir.string());
        CHECK(lemon::utils::get_config_dir() == config_dir.string());

        migrate_json(cache_dir.string(),
                                                              config_dir.string());

        CHECK(fs::exists(cache_dir / "config.json"));
        CHECK(fs::exists(config_dir / "config.json"));
        CHECK(read_text(config_dir / "config.json") == "{\"host\":\"127.0.0.1\"}\n");
    } catch (...) {
        lemon::utils::set_cache_dir("");
        lemon::utils::set_config_dir("");
        fs::remove_all(root);
        throw;
    }

    lemon::utils::set_cache_dir("");
    lemon::utils::set_config_dir("");
    fs::remove_all(root);
}

#ifndef _WIN32
void test_systemd_state_directory_takes_precedence_over_home() {
    const auto unique = std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now().time_since_epoch())
                            .count();
    const fs::path root = fs::temp_directory_path() /
                          ("lemonade_state_dir_" + std::to_string(unique));
    const fs::path home = root / "home";
    const fs::path state_dir = root / "var-lib-lemonade";

    ScopedEnvVar home_var("HOME", home.string());
    ScopedEnvVar state_var("STATE_DIRECTORY", state_dir.string());

    lemon::utils::set_config_dir("");

    const std::string resolved_config_dir = lemon::utils::get_config_dir();
    CHECK(resolved_config_dir == state_dir.string());

    fs::remove_all(root);
}

void test_systemd_cache_directory_is_used_directly() {
    const auto unique = std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now().time_since_epoch())
                            .count();
    const fs::path root = fs::temp_directory_path() /
                          ("lemonade_cache_dir_" + std::to_string(unique));
    const fs::path home = root / "home";
    const fs::path cache_dir = root / "var-cache-lemonade";

    ScopedEnvVar home_var("HOME", home.string());
    ScopedEnvVar cache_var("CACHE_DIRECTORY", cache_dir.string());

    lemon::utils::set_cache_dir("");

    const std::string resolved_cache_dir = lemon::utils::get_cache_dir();
    CHECK(resolved_cache_dir == cache_dir.string());

    fs::remove_all(root);
}

void test_systemd_relocation_preserves_runtime_and_model_owners() {
    const auto unique = std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now().time_since_epoch())
                            .count();
    const fs::path root = fs::temp_directory_path() /
                          ("lemonade_relocate_" + std::to_string(unique));
    const fs::path home = root / "home";
    const fs::path legacy_cache = home / ".cache" / "lemonade";
    const fs::path legacy_hub = home / ".cache" / "huggingface" / "hub";
    const fs::path cache_dir = root / "var" / "cache" / "lemonade";
    const fs::path config_dir = root / "var" / "lib" / "lemonade";

    ScopedEnvVar home_var("HOME", home.string());
    // Systemd exports these when it relocates the service's directories; the
    // recovery leg only runs when it sees them.
    ScopedEnvVar state_var("STATE_DIRECTORY", config_dir.string());
    ScopedEnvVar cache_var("CACHE_DIRECTORY", cache_dir.string());

    try {
        write_text(legacy_cache / "config.json", "{\"port\":9000}\n");
        write_text(legacy_cache / "bin" / "llama-server", "ELF\n");
        write_text(legacy_cache / "aliases.json", "{\"service-chat\":\"Fixture-Model\"}\n");
        const fs::path legacy_model =
            legacy_hub / "models--demo--foo" / "snapshots" / "abc" / "model.gguf";
        write_text(legacy_model, "GGUF\n");

        migrate_paths(cache_dir.string(), config_dir.string());

        CHECK(fs::exists(config_dir / "config.json"));
        CHECK(read_text(config_dir / "config.json") == "{\"port\":9000}\n");
        CHECK(fs::exists(legacy_cache / "config.json"));

        CHECK(!fs::exists(cache_dir / "bin" / "llama-server"));
        CHECK(read_text(legacy_cache / "bin" / "llama-server") == "ELF\n");
        CHECK(fs::exists(legacy_cache));
        CHECK(fs::exists(cache_dir / "aliases.json"));
        CHECK(read_text(cache_dir / "aliases.json") == read_text(legacy_cache / "aliases.json"));
        lemon::utils::set_cache_dir(lemon::utils::path_to_utf8(cache_dir));
        lemon::utils::set_config_dir(lemon::utils::path_to_utf8(config_dir));
        {
            lemon::AliasManager aliases(lemon::utils::path_to_utf8(cache_dir));
            CHECK(aliases.resolve_alias("service-chat").value_or("") == "Fixture-Model");
            std::string error;
            CHECK(aliases.set_alias("new-alias", "Fixture-Model", error));
        }
        migrate_paths(cache_dir.string(), config_dir.string());
        {
            lemon::AliasManager restarted(lemon::utils::path_to_utf8(cache_dir));
            CHECK(restarted.has_alias("new-alias"));
        }
        CHECK(read_text(legacy_cache / "aliases.json") == "{\"service-chat\":\"Fixture-Model\"}\n");
        lemon::utils::set_cache_dir(""); lemon::utils::set_config_dir("");

        // Models are deliberately left in place — no cross-filesystem copy.
        CHECK(fs::exists(legacy_model));
        CHECK(read_text(legacy_model) == "GGUF\n");
    } catch (...) {
        fs::remove_all(root);
        throw;
    }

    fs::remove_all(root);
}

void test_relocation_preserves_existing_target_files() {
    const auto unique = std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now().time_since_epoch())
                            .count();
    const fs::path root = fs::temp_directory_path() /
                          ("lemonade_relocate_keep_" + std::to_string(unique));
    const fs::path home = root / "home";
    const fs::path legacy_cache = home / ".cache" / "lemonade";
    const fs::path cache_dir = root / "var" / "cache" / "lemonade";
    const fs::path config_dir = root / "var" / "lib" / "lemonade";

    ScopedEnvVar home_var("HOME", home.string());
    ScopedEnvVar state_var("STATE_DIRECTORY", config_dir.string());
    ScopedEnvVar cache_var("CACHE_DIRECTORY", cache_dir.string());

    try {
        write_text(legacy_cache / "config.json", "{\"stale\":true}\n");
        write_text(config_dir / "config.json", "{\"current\":true}\n");

        bool rejected = false;
        try { migrate_paths(cache_dir.string(), config_dir.string()); }
        catch (const lemon::AdmissionError& error) { rejected = error.code == "configuration_migration_recovery_required"; }
        CHECK(rejected);
        CHECK(read_text(legacy_cache / "config.json") == "{\"stale\":true}\n");
        CHECK(read_text(config_dir / "config.json") == "{\"current\":true}\n");
    } catch (...) {
        fs::remove_all(root);
        throw;
    }

    fs::remove_all(root);
}

// A user pointing lemond at an explicit cache dir (e.g. `lemond /tmp/scratch`)
// must NOT drain their real ~/.cache/lemonade — that only happens on a service
// upgrade, signalled by systemd's STATE_DIRECTORY / CACHE_DIRECTORY.
void test_explicit_cache_dir_does_not_drain_legacy() {
    const auto unique = std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now().time_since_epoch())
                            .count();
    const fs::path root = fs::temp_directory_path() /
                          ("lemonade_scratch_" + std::to_string(unique));
    const fs::path home = root / "home";
    const fs::path legacy_cache = home / ".cache" / "lemonade";
    const fs::path scratch = root / "scratch";

    ScopedEnvVar home_var("HOME", home.string());
    // No STATE_DIRECTORY / CACHE_DIRECTORY: not a service relocation.
    ScopedEnvVar state_var("STATE_DIRECTORY", "");
    ScopedEnvVar cache_var("CACHE_DIRECTORY", "");

    try {
        write_text(legacy_cache / "config.json", "{\"real\":true}\n");
        write_text(legacy_cache / "bin" / "llama-server", "ELF\n");

        // Portable/explicit invocation: cache_dir == config_dir == scratch.
        migrate_paths(scratch.string(), scratch.string());

        // The real install is untouched; nothing was dragged into scratch.
        CHECK(fs::exists(legacy_cache / "config.json"));
        CHECK(read_text(legacy_cache / "config.json") == "{\"real\":true}\n");
        CHECK(fs::exists(legacy_cache / "bin" / "llama-server"));
        CHECK(!fs::exists(scratch / "config.json"));
        CHECK(!fs::exists(scratch / "bin"));
    } catch (...) {
        fs::remove_all(root);
        throw;
    }

    fs::remove_all(root);
}

void test_xdg_cache_and_config_dirs_are_resolved_separately() {
    const auto unique = std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now().time_since_epoch())
                            .count();
    const fs::path root = fs::temp_directory_path() /
                          ("lemonade_config_dir_xdg_" + std::to_string(unique));
    const fs::path home = root / "home";
    const fs::path cache_home = root / "xdg-cache";
    const fs::path config_home = root / "xdg-config";
    const fs::path cache_dir = cache_home / "lemonade";
    const fs::path config_dir = config_home / "lemonade";

    ScopedEnvVar home_var("HOME", home.string());
    ScopedEnvVar cache_var("XDG_CACHE_HOME", cache_home.string());
    ScopedEnvVar config_var("XDG_CONFIG_HOME", config_home.string());

    try {
        write_text(cache_dir / "config.json", "{\"port\":13305}\n");

        CHECK(lemon::utils::get_cache_dir() == cache_dir.string());
        CHECK(lemon::utils::get_config_dir() == config_dir.string());
        migrate_json(cache_dir.string(),
                                                              config_dir.string());

        CHECK(fs::exists(cache_dir / "config.json"));
        CHECK(fs::exists(config_dir / "config.json"));
        CHECK(read_text(config_dir / "config.json") == "{\"port\":13305}\n");
    } catch (...) {
        fs::remove_all(root);
        throw;
    }

    fs::remove_all(root);
}
#endif

int cohort_checks = 0, cohort_failures = 0;
void cohort_check(bool value, const char* label) {
    ++cohort_checks;
    if (!value) ++cohort_failures;
    std::cerr << (value ? "PASS: " : "FAIL: ") << label << "\n";
}
void test_native_settings_cohort() {
    using namespace lemon;
    const auto root = test::make_scratch_dir("configuration-cohort");
    const auto cache = root / "cache", config = root / "config";
    fs::create_directories(cache); fs::create_directories(config);
    utils::set_cache_dir(utils::path_to_utf8(cache));
    utils::set_config_dir(utils::path_to_utf8(cache));
    utils::set_models_dir(utils::path_to_utf8(root / "models"));
    auto defaults = ConfigFile::base_defaults();
    defaults["offline"] = true; defaults["no_fetch_executables"] = true;
    defaults["disable_model_filtering"] = true;
    RuntimeConfig runtime(defaults); RuntimeConfig::set_global(&runtime);
    const std::string name = "user.Migration-Cohort";
    {
        ModelManager models;
        models.register_user_model(name, {{"recipe", "llamacpp"}, {"checkpoint", "fixture/repo:Q4_K_M"}});
        models.set_saved_model_options(name, {{"ctx_size", 2048}});
        models.select_execution_profile(name, {{"id", "alternate"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 4096}}}});
        cohort_check(models.get_model_info(name).recipe == "vllm" &&
            models.get_saved_model_options(name).at("ctx_size") == 4096,
            "the source metadata fixture has one coherent selected profile and saved options");
    }
    const auto helpers = nlohmann::json{{"version", 1}, {"flm", {{"model", "Migration-Cohort"},
        {"operation", "remove"}, {"fingerprint", std::string(64, '0')}, {"pending", true}, {"state", "dispatching"}}}};
    utils::JsonUtils::save_to_file(helpers, utils::path_to_utf8(cache / "native_helpers.json"));
    const auto source_profile = read_text(cache / "execution_profiles.json");
    const auto source_helpers = read_text(cache / "native_helpers.json");
    migrate_json(utils::path_to_utf8(cache), utils::path_to_utf8(config));
    utils::set_config_dir(utils::path_to_utf8(config));
    {
        ModelManager restarted;
        const auto snapshot = restarted.get_model_options_snapshot(name);
        cohort_check(snapshot.info.recipe == "vllm" && snapshot.profile == "alternate" &&
            snapshot.saved.at("ctx_size") == 4096 && snapshot.info.recipe_options.get_option("ctx_size") == 4096,
            "migration preserves selection, recipe, saved and effective options as one version");
        const auto status = restarted.native_helper_status();
        cohort_check(status.contains("flm") && status.at("flm") == helpers.at("flm"),
            "migration retains unresolved native helper intent instead of presenting an absent journal");
    }
    cohort_check(fs::is_regular_file(config / "execution_profiles.json") &&
        read_text(config / "execution_profiles.json") == source_profile &&
        fs::is_regular_file(config / "native_helpers.json") && read_text(config / "native_helpers.json") == source_helpers,
        "the complete native metadata cohort is available under the destination owner");
    RuntimeConfig::set_global(nullptr);
}

constexpr const char* manifest_name = ".juiced-configuration-migration.json";
constexpr const char* generation_name = ".juiced-configuration-generation";
const std::vector<std::string> cohort_names = {"config.json", "jobs.json", "recipe_options.json",
    "user_models.json", "execution_profiles.json", "native_helpers.json"};
void save_json(const fs::path& path, const lemon::json& value) {
    lemon::utils::JsonUtils::save_to_file(value, lemon::utils::path_to_utf8(path));
}
void roots(const fs::path& root, bool destination) {
    lemon::utils::set_cache_dir(lemon::utils::path_to_utf8(root / "cache"));
    lemon::utils::set_config_dir(lemon::utils::path_to_utf8(root / (destination ? "config" : "cache")));
    lemon::utils::set_models_dir(lemon::utils::path_to_utf8(root / "models"));
}
std::map<std::string, std::string> seed_generation(const fs::path& root) {
    fs::create_directories(root / "cache"); fs::create_directories(root / "config"); roots(root, false);
    save_json(root / "cache/config.json", {{"config_version", 2}, {"offline", true}, {"future", "preserved"}});
    {
        lemon::ModelManager models;
        models.register_user_model("user.Migration-Cohort", {{"recipe", "llamacpp"}, {"checkpoint", "fixture/repo:Q4_K_M"}});
        models.set_saved_model_options("user.Migration-Cohort", {{"ctx_size", 2048}, {"future", "preserved"}});
        models.select_execution_profile("user.Migration-Cohort", {{"id", "alternate"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 4096}}}});
    }
    save_json(root / "cache/native_helpers.json", {{"version", 1}, {"flm", {{"model", "Migration-Cohort"},
        {"operation", "remove"}, {"fingerprint", std::string(64, '0')}, {"pending", true}, {"state", "dispatching"}}}});
    save_json(root / "cache/jobs.json", {{"version", 1}, {"future", "preserved"},
        {"jobs", lemon::json::array({{{"id", "retained"}, {"status", "running"}, {"future", "preserved"}}})}});
    std::map<std::string, std::string> original;
    for (const auto& name : cohort_names) original[name] = read_text(root / "cache" / name);
    return original;
}
bool same_files(const fs::path& directory, const std::map<std::string, std::string>& expected) {
    for (const auto& [name, text] : expected)
        if (!fs::is_regular_file(directory / name) || read_text(directory / name) != text) return false;
    return true;
}
template<class Action> bool blocked(Action action) {
    try { action(); return false; }
    catch (const lemon::AdmissionError& error) { return error.code == "configuration_migration_recovery_required"; }
}
void readers_block(const fs::path& root, bool relocated = false) {
    roots(root, true);
    if (relocated) lemon::utils::set_cache_dir(lemon::utils::path_to_utf8(root / "new-cache"));
    const auto config = lemon::utils::path_to_utf8(root / "config");
    cohort_check(blocked([&] { lemon::ConfigFile::require_complete_directory(config); }), "unfinished generations cannot authorize a destination reader");
    cohort_check(blocked([&] { lemon::ConfigFile::load_raw(config); }), "configuration reads cannot see a partially published cohort");
    cohort_check(blocked([&] { lemon::ConfigFile::save(config, {{"port", 12345}}); }), "configuration writes cannot overwrite an unfinished cohort");
    cohort_check(blocked([&] { lemon::ModelManager models; }), "model publication owners reject an unfinished destination before loading");
    cohort_check(blocked([&] { lemon::jobs::JobManager jobs(config, lemon::ResourceAdmission{}); }), "retained-job loading rejects unfinished generations before interpreting history");
    cohort_check(blocked([&] { lemon::AliasManager aliases(lemon::utils::get_cache_dir()); }), "alias loading cannot publish an unfinished cross-root generation");
}
void accepted(const fs::path& root, const std::map<std::string, std::string>& original, bool relocated = false) {
    roots(root, true);
    if (relocated) lemon::utils::set_cache_dir(lemon::utils::path_to_utf8(root / "new-cache"));
    auto config_files = original; config_files.erase("aliases.json");
    const bool alias_retained = !relocated || (fs::is_regular_file(root / "new-cache/aliases.json") &&
        read_text(root / "new-cache/aliases.json") == original.at("aliases.json"));
    cohort_check(same_files(root / "config", config_files) && alias_retained && same_files(root / "cache", original),
        "activation preserves the full byte-exact destination and original rollback cohort");
    lemon::ModelManager models;
    const auto view = models.get_model_options_snapshot("user.Migration-Cohort");
    cohort_check(view.profile == "alternate" && view.info.recipe == "vllm" && view.saved.at("ctx_size") == 4096 &&
        view.info.recipe_options.get_option("ctx_size") == 4096, "restart reads one complete profile/effective/saved version");
    lemon::jobs::JobManager jobs(lemon::utils::path_to_utf8(root / "config"), lemon::ResourceAdmission{});
    const auto job = jobs.get("retained");
    cohort_check(job && job->at("status") == "interrupted" && job->at("future") == "preserved" && same_files(root / "config", config_files),
        "historical jobs retain unknown data without automatically resuming or rewriting it");
    lemon::ResourceAdmission admission; models.set_resource_admission(admission);
    bool rejected = false;
    try { models.run_flm_helper("Migration-Cohort", "remove", lemon::utils::path_to_utf8(root / "must-not-exist"), {}); }
    catch (const lemon::AdmissionError& error) { rejected = error.code == "helper_recovery_required"; }
    cohort_check(rejected && same_files(root / "config", config_files), "prior helper intent remains quarantined before any new helper can dispatch");
}
void interruption_cases(const fs::path& executable) {
    for (const auto* phase : {"intent", "staged:config.json", "prepared", "published:config.json", "published:user_models.json", "active"}) {
        const auto root = lemon::test::make_scratch_dir("migration-interruption");
        const auto original = seed_generation(root);
        const auto result = lemon::utils::ProcessManager::run_process_with_output(lemon::utils::path_to_utf8(executable),
            {"--interrupt", lemon::utils::path_to_utf8(root), phase}, [](const auto& line) {
                std::cerr << "CHILD: " << line << "\n"; return true;
            }, "", 10);
        cohort_check(result == 91, "controlled manager exit reaches an actual migration publication boundary");
        if (result != 91) continue;
        cohort_check(same_files(root / "cache", original), "interrupted publication never destroys its original complete generation");
        if (std::string(phase) != "active") readers_block(root);
        migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config"));
        accepted(root, original);
    }
}
void migrate_relocated(const fs::path& root) {
    lemon::utils::ManagedDirectoryLease owner({root / "new-cache", root / "config"});
    lemon::ConfigFile::migrate_directory(lemon::utils::path_to_utf8(root / "new-cache"),
        lemon::utils::path_to_utf8(root / "config"), owner, lemon::utils::path_to_utf8(root / "cache"));
}
void relocated_interruptions(const fs::path& executable) {
    for (const auto* phase : {"prepared", "published:aliases.json", "active"}) {
        const auto root = lemon::test::make_scratch_dir("migration-cross-root");
        auto original = seed_generation(root);
        original["aliases.json"] = "{\"service-chat\":\"Migration-Cohort\"}\n";
        write_text(root / "cache/aliases.json", original.at("aliases.json"));
        const auto result = lemon::utils::ProcessManager::run_process_with_output(lemon::utils::path_to_utf8(executable),
            {"--interrupt-relocated", lemon::utils::path_to_utf8(root), phase}, [](const auto& line) {
                std::cerr << "CHILD: " << line << "\n"; return true;
            }, "", 10);
        cohort_check(result == 91, "cross-root configuration and alias publication reaches its controlled exit");
        if (result != 91) continue;
        if (std::string(phase) != "active") readers_block(root, true);
        migrate_relocated(root);
        accepted(root, original, true);
        lemon::AliasManager aliases(lemon::utils::get_cache_dir());
        cohort_check(aliases.resolve_alias("service-chat").value_or("") == "Migration-Cohort",
            "a recovered cache relocation preserves public alias resolution");
    }
}
void synchronous_failures() {
    for (int fault = 0; fault != 4; ++fault) {
        const auto root = lemon::test::make_scratch_dir("migration-failure");
        const auto original = seed_generation(root);
        const std::string phase = fault == 0 ? "before-stage:jobs.json" : fault == 1 ? "before-publish:jobs.json" : fault == 2 ? "before-prepared" : "before-active";
        const auto target = fault == 0 ? root / "config" / generation_name / "jobs.json" : fault == 1 ? root / "config/jobs.json" : root / "config" / manifest_name;
        const auto held = root / "config/held-intent";
        bool reached = false;
        lemon::ConfigurationMigrationTestHook::barrier([&](const auto& seen) {
            if (seen != phase || reached) return;
            reached = true;
            if (fault >= 2) fs::rename(target, held);
            fs::create_directory(target); write_text(target / "block", "preserve");
        });
        bool rejected = false;
        try { migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config")); }
        catch (const std::exception&) { rejected = true; }
        lemon::ConfigurationMigrationTestHook::barrier({});
        cohort_check(reached && rejected && same_files(root / "cache", original), "synchronous staging/replacement failure retains source data and migration intent");
        fs::remove_all(target);
        if (fault >= 2) fs::rename(held, target);
        readers_block(root);
        migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config"));
        accepted(root, original);
    }
}
void generation_changes() {
    for (const bool source_changed : {false, true}) {
        const auto root = lemon::test::make_scratch_dir("migration-generation-change");
        const auto original = seed_generation(root);
        const auto changed = root / (source_changed ? "cache" : "config") / "execution_profiles.json";
        lemon::ConfigurationMigrationTestHook::barrier([&](const auto& phase) {
            if (phase == "before-active") write_text(changed, original.at("execution_profiles.json") + " ");
        });
        const bool rejected = blocked([&] { migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config")); });
        lemon::ConfigurationMigrationTestHook::barrier({});
        cohort_check(rejected && read_text(changed) == original.at("execution_profiles.json") + " ",
            "a changed source or destination is preserved and cannot activate through an obsolete verification");
        readers_block(root);
        write_text(changed, original.at("execution_profiles.json"));
        migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config"));
        accepted(root, original);
    }
}
void retained_state_and_invalid_intents() {
    for (const bool target_state : {false, true}) {
        const auto root = lemon::test::make_scratch_dir("migration-owned-state");
        const auto original = seed_generation(root);
        const auto state = root / (target_state ? "config" : "cache") / "juiced/state.json";
        write_text(state, "{\"snapshot_store\":\"retained binding\"}");
        cohort_check(blocked([&] { migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config")); }) &&
            same_files(root / "cache", original) && !fs::exists(root / "config" / manifest_name) && read_text(state).find("retained binding") != std::string::npos,
            "path-bound Juiced state rejects bootstrap relocation before any metadata mutation");
    }
    for (int variant = 0; variant != 6; ++variant) {
        const auto root = lemon::test::make_scratch_dir("migration-invalid-intent");
        const auto original = seed_generation(root);
        migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config"));
        auto manifest = lemon::json::parse(read_text(root / "config" / manifest_name));
        if (variant == 0) manifest["version"] = 2;
        if (variant == 1) manifest["state"] = "unsupported";
        if (variant == 2) manifest["files"]["../escape"] = manifest["files"].begin().value();
        if (variant == 3) manifest["files"].begin().value()["permissions"] = uint64_t(1) << 32;
        if (variant == 4) manifest["source"] = "relative-source";
        if (variant == 5) for (auto& entry : manifest["files"]) entry["bytes"] = uint64_t(16 * 1024 * 1024);
        save_json(root / "config" / manifest_name, manifest);
        const auto retained = read_text(root / "config" / manifest_name);
        cohort_check(blocked([&] { migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config")); }) &&
            read_text(root / "config" / manifest_name) == retained && same_files(root / "cache", original) && same_files(root / "config", original),
            "unsupported or malformed intents are quarantined without being reset or traversed");
        readers_block(root);
    }
}
void active_edits_and_bounds() {
    {
        const auto root = lemon::test::make_scratch_dir("migration-active-edits");
        const auto original = seed_generation(root);
        migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config"));
        roots(root, true);
        {
            lemon::ModelManager models;
            models.update_saved_model_options("user.Migration-Cohort", {{"ctx_size", 8192}});
            lemon::ConfigFile::save(lemon::utils::path_to_utf8(root / "config"), {{"config_version", 2}, {"future", "new active value"}});
        }
        migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config"));
        lemon::ModelManager restarted;
        const auto view = restarted.get_model_options_snapshot("user.Migration-Cohort");
        cohort_check(view.saved.at("ctx_size") == 8192 && view.info.recipe_options.get_option("ctx_size") == 8192 &&
            lemon::ConfigFile::load_raw(lemon::utils::path_to_utf8(root / "config")).at("future") == "new active value" &&
            same_files(root / "cache", original), "completed migration never replays retained backups over later committed native edits");
    }
    for (const bool oversized : {false, true}) {
        const auto root = lemon::test::make_scratch_dir("migration-bounds");
        seed_generation(root);
        const auto path = root / "cache/config.json";
        if (oversized) fs::resize_file(path, 16 * 1024 * 1024 + 1);
        else {
            std::string nested;
            for (int i = 0; i != 150; ++i) nested += "{\"nested\":";
            nested += "0"; nested.append(150, '}'); write_text(path, nested);
        }
        const auto size = fs::file_size(path);
        cohort_check(blocked([&] { migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config")); }) &&
            fs::file_size(path) == size && !fs::exists(root / "config" / manifest_name),
            "oversized or deeply nested metadata rejects before staging without consuming unbounded input");
    }
}
void generation_contracts(const fs::path& executable) {
    auto defaults = lemon::ConfigFile::base_defaults(); defaults["offline"] = true;
    defaults["no_fetch_executables"] = true; defaults["disable_model_filtering"] = true;
    lemon::RuntimeConfig runtime(defaults); lemon::RuntimeConfig::set_global(&runtime);
    interruption_cases(executable); relocated_interruptions(executable); synchronous_failures(); generation_changes(); retained_state_and_invalid_intents(); active_edits_and_bounds();
    lemon::RuntimeConfig::set_global(nullptr);
}

}

int main(int argc, char** argv) {
    if (argc == 4 && (std::string(argv[1]) == "--interrupt" || std::string(argv[1]) == "--interrupt-relocated")) {
        const auto isolated = lemon::test::state_dir(); fs::remove_all(isolated);
        const auto root = lemon::utils::path_from_utf8(argv[2]);
        const std::string phase = argv[3];
        lemon::ConfigurationMigrationTestHook::barrier([&](const auto& seen) { if (seen == phase) std::_Exit(91); });
        if (std::string(argv[1]) == "--interrupt-relocated") migrate_relocated(root);
        else migrate_json(lemon::utils::path_to_utf8(root / "cache"), lemon::utils::path_to_utf8(root / "config"));
        return 2;
    }
    if (argc == 2 && std::string(argv[1]) == "--cohort-reproduction") {
        test_native_settings_cohort();
        std::cerr << "Configuration cohort: " << cohort_checks << " checks, " << cohort_failures << " failures\n";
        return cohort_failures ? 1 : 0;
    }
    test_standard_cache_layout_migrates_to_explicit_config_dir();
    test_identical_cache_and_config_dirs_stay_put();
    test_explicit_config_dir_is_honored();
#ifndef _WIN32
    test_systemd_state_directory_takes_precedence_over_home();
    test_systemd_cache_directory_is_used_directly();
    test_systemd_relocation_preserves_runtime_and_model_owners();
    test_relocation_preserves_existing_target_files();
    test_explicit_cache_dir_does_not_drain_legacy();
    test_xdg_cache_and_config_dirs_are_resolved_separately();
#endif
    test_native_settings_cohort();
    generation_contracts(lemon::utils::path_from_utf8(lemon::utils::get_executable_dir()) / lemon::utils::path_from_utf8(argv[0]).filename());
    std::cout << "Configuration migration: " << legacy_checks + cohort_checks << " checks, " << cohort_failures << " failures\n";
    if (cohort_failures) return 1;
    return 0;
}
