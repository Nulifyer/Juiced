#include "lemon/cloud_provider_registry.h"
#include "lemon/config_file.h"
#include "lemon/jobs/job_manager.h"
#include "lemon/model_manager.h"
#include "lemon/runtime_config.h"
#include "lemon/utils/json_utils.h"
#include "lemon/utils/path_utils.h"
#include "support/test_state_isolation.h"

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <functional>
#include <thread>

#ifdef _WIN32
#include <windows.h>
#endif

using json = nlohmann::json;
namespace fs = std::filesystem;
static int checks = 0, failures = 0;
static void check(bool ok, const char* message) {
    ++checks;
    if (!ok) ++failures;
    std::fprintf(stderr, "%s: %s\n", ok ? "PASS" : "FAIL", message);
}
static json read(const fs::path& path) { std::ifstream f(path); return json::parse(f); }
static void write(const fs::path& path, const json& body) { std::ofstream f(path); f << body.dump(); }

class BlockReplacement {
public:
    explicit BlockReplacement(const fs::path& target) : parent_(target.parent_path()) {
#ifdef _WIN32
        handle_ = CreateFileW(target.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE,
            nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (handle_ == INVALID_HANDLE_VALUE) throw std::runtime_error("Could not hold replacement fixture");
#else
        permissions_ = fs::status(parent_).permissions();
        fs::permissions(parent_, fs::perms::owner_read | fs::perms::owner_exec);
#endif
    }
    ~BlockReplacement() {
#ifdef _WIN32
        CloseHandle(handle_);
#else
        fs::permissions(parent_, permissions_);
#endif
    }
private:
    fs::path parent_;
#ifdef _WIN32
    HANDLE handle_;
#else
    fs::perms permissions_;
#endif
};

static void cloud_failures() {
    for (const auto& failure : {"staging", "replacement"}) for (int operation = 0; operation < 4; ++operation) {
        const auto root = lemon::test::make_scratch_dir("cloud");
        lemon::CloudProviderRegistry registry;
        registry.install("fixture", "http://localhost.invalid/v1");
        registry.set_runtime_key("fixture", "process-only-fixture");
        lemon::RuntimeConfig runtime(lemon::ConfigFile::base_defaults());
        auto persist = [&](const json& metadata) {
            runtime.set_persisted({{"cloud_providers", metadata}}, [&](const json& change) {
                auto disk = lemon::ConfigFile::load_raw(root.string());
                lemon::ConfigFile::save(root.string(), lemon::utils::JsonUtils::merge(disk, change));
            });
        };
        persist(registry.to_config_array());
        const auto before = registry.to_config_array(), disk = read(root / "config.json"), config = runtime.snapshot();
        auto mutate = [&] {
            if (operation == 0) registry.install("new", "https://localhost.invalid/v2", {}, persist);
            if (operation == 1) registry.install("fixture", "https://localhost.invalid/v2", {}, persist);
            if (operation == 2) registry.set_allow_insecure_http("fixture", true, persist);
            if (operation == 3) registry.uninstall("fixture", persist);
        };
        bool rejected = false;
        if (std::string(failure) == "staging") {
            fs::create_directory(root / "config.json.tmp");
            write(root / "config.json.tmp" / "block", json::object());
            try { mutate(); } catch (const std::exception&) { rejected = true; }
            fs::remove_all(root / "config.json.tmp");
        } else {
            write(root / "config.json.tmp", json::object());
            BlockReplacement block(root / "config.json");
            try { mutate(); } catch (const std::exception&) { rejected = true; }
        }
        check(rejected && registry.to_config_array() == before && runtime.snapshot() == config && read(root / "config.json") == disk,
              "failed cloud publication preserves registry, runtime and disk for every mutation");
        check(registry.resolve_key("fixture") == "process-only-fixture", "failed cloud mutation preserves process credentials");
        mutate();
        const auto durable = read(root / "config.json").at("cloud_providers");
        lemon::CloudProviderRegistry restarted; restarted.load_from_config(durable);
        check(registry.to_config_array() == durable && runtime.snapshot().at("cloud_providers") == durable && restarted.to_config_array() == durable,
              "cloud retry and restart agree on complete metadata");
        check(read(root / "config.json").dump().find("process-only-fixture") == std::string::npos &&
              (operation != 3 || registry.resolve_key("fixture").empty()), "credentials never persist and uninstall clears them after commit");
    }
}

static void retained_jobs() {
    for (const auto& status : {"completed", "interrupted"}) for (bool staging : {false, true}) {
        auto root = lemon::test::make_scratch_dir("jobs");
        auto target = root / "jobs.json";
        write(target, {{"version", 1}, {"jobs", json::array({{{"id", "retained-1"}, {"status", status}}})}});
        {
            lemon::jobs::JobManager jobs(root.string(), root.string(), {});
            auto before = jobs.get("retained-1"); const auto disk = read(target);
            bool active = false, rejected = false;
            if (staging) {
                fs::create_directory(root / "jobs.json.tmp");
                try { jobs.remove("retained-1", active); } catch (const lemon::jobs::JobError& e) { rejected = e.status == 500; }
                fs::remove(root / "jobs.json.tmp");
            } else {
                write(root / "jobs.json.tmp", json::object());
                BlockReplacement block(target);
                try { jobs.remove("retained-1", active); } catch (const lemon::jobs::JobError& e) { rejected = e.status == 500; }
            }
            check(rejected && jobs.get("retained-1") == before && read(target) == disk,
                  "failed retained deletion or tombstone preserves memory and committed record");
            check(jobs.remove("retained-1", active), "failed retained deletion remains retryable");
        }
        lemon::jobs::JobManager restarted(root.string(), root.string(), {});
        check(!restarted.get("retained-1"), "durable deletion does not reappear after restart");
    }
}

static void model_files() {
    auto defaults = lemon::ConfigFile::base_defaults();
    defaults["offline"] = true; defaults["auto_check_model_updates"] = false;
    lemon::RuntimeConfig runtime(defaults); lemon::RuntimeConfig::set_global(&runtime);
    {
        lemon::ModelManager models;
        const json definition = {{"checkpoint", "fixture/repo:Q4_K_M"}, {"recipe", "llamacpp"}};
        models.register_user_model("user.Persistence", definition);
        auto info = models.get_model_info("user.Persistence");
        models.save_model_options(info);
        models.select_execution_profile("user.Persistence", {{"id", "registered"}});
        const auto root = lemon::utils::get_config_dir();
        for (const auto& name : {"user_models.json", "recipe_options.json", "execution_profiles.json"}) {
            auto target = fs::path(root) / name;
            const auto before = read(target);
            const auto before_options = models.get_saved_model_options("user.Persistence");
            bool rejected = false;
            {
                BlockReplacement block(target);
                try {
                    if (std::string(name) == "user_models.json") models.register_user_model("user.Persistence2", definition);
                    if (std::string(name) == "recipe_options.json") {
                        auto changed = info;
                        changed.recipe_options = lemon::RecipeOptions("llamacpp", {{"ctx_size", 4096}});
                        models.save_model_options(changed);
                    }
                    if (std::string(name) == "execution_profiles.json") models.select_execution_profile("user.Persistence", {{"id", "registered"}});
                } catch (const std::exception&) { rejected = true; }
            }
            check(rejected && read(target) == before, "model registration/options/profile write failure preserves complete committed file");
            check(models.get_saved_model_options("user.Persistence") == before_options, "failed model-file publication preserves visible saved options");
        }
        lemon::ModelManager restarted;
        check(restarted.model_exists("user.Persistence") && !restarted.model_exists("user.Persistence2"), "model registration restart preserves only committed registrations");
        check(restarted.get_saved_model_options("user.Persistence") == models.get_saved_model_options("user.Persistence") &&
              restarted.get_execution_profiles("user.Persistence") == models.get_execution_profiles("user.Persistence"), "saved options and profiles retain restart readback");
    }
    lemon::RuntimeConfig::set_global(nullptr);
}

int main() {
    cloud_failures(); retained_jobs(); model_files();
    std::fprintf(stderr, "Persistence boundaries: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
