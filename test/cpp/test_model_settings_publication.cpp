#include "lemon/config_file.h"
#include "lemon/error_types.h"
#include "lemon/model_manager.h"
#include "lemon/runtime_config.h"
#include "lemon/utils/json_utils.h"
#include "lemon/utils/path_utils.h"
#include "lemon/utils/process_manager.h"
#include "support/test_state_isolation.h"

#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>

using namespace lemon;
namespace fs = std::filesystem;
static constexpr const char* model = "user.Atomic-Settings";
static int checks = 0, failures = 0;
static void check(bool condition, const char* label) {
    ++checks;
    std::fprintf(stderr, "%s: %s\n", condition ? "PASS" : "FAIL", label);
    if (!condition) ++failures;
}
static std::string bytes(const fs::path& path) {
    std::ifstream input(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(input), {}};
}
static void save(const fs::path& path, const json& data) {
    utils::JsonUtils::save_to_file(data, utils::path_to_utf8(path));
}
static void use_root(const fs::path& root) {
    utils::set_cache_dir(utils::path_to_utf8(root));
    utils::set_config_dir(utils::path_to_utf8(root));
    utils::set_models_dir(utils::path_to_utf8(root / "hf"));
}
static void seed(ModelManager& models) {
    models.register_user_model(model, {{"recipe", "llamacpp"}, {"checkpoint", "fixture/repo:Q4_K_M"},
        {"recipe_options", {{"ctx_size", 1536}}}});
    models.set_saved_model_options(model, {{"ctx_size", 2048}, {"future_option", "preserved"}});
}
static void alternate(ModelManager& models) {
    models.select_execution_profile(model, {{"id", "alternate"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 4096}}}});
}
static bool view(ModelManager& models, const char* recipe, int context) {
    const auto snapshot = models.get_model_options_snapshot(model);
    return snapshot.info.recipe == recipe && snapshot.info.recipe_options.get_option("ctx_size") == context &&
        snapshot.saved.at("ctx_size") == context;
}
class BlockPublication {
public:
    explicit BlockPublication(fs::path path) : path_(std::move(path)), backup_(path_.string() + ".held") {
        fs::rename(path_, backup_);
        fs::create_directory(path_);
        std::ofstream(path_ / "block") << "replacement must fail";
    }
    ~BlockPublication() { fs::remove_all(path_); fs::rename(backup_, path_); }
private:
    fs::path path_, backup_;
};
namespace lemon {
struct ModelPublicationTestHook {
    static void interrupt(ModelManager& models, bool after, bool options) {
        const auto stopped = [] { std::_Exit(91); };
        if (after) models.settings_publication_barrier_ = stopped;
        else if (options) models.options_persistence_barrier_ = stopped;
        else models.profile_persistence_barrier_ = stopped;
    }
};
}
static void interruptions(const fs::path& executable) {
    for (const std::string action : {"alternate", "registered", "options"}) {
        for (const bool after : {false, true}) {
            const auto root = test::make_scratch_dir("settings-" + action);
            use_root(root);
            {
                ModelManager models; seed(models);
                if (action == "registered") alternate(models);
                if (action == "options") models.select_execution_profile(model, {{"id", "registered"}});
            }
            const auto legacy = bytes(root / "recipe_options.json");
            const auto status = utils::ProcessManager::run_process_with_output(utils::path_to_utf8(executable),
                {after ? "--after" : "--before", utils::path_to_utf8(root), action}, [](const auto& line) {
                    std::fprintf(stderr, "CHILD: %s\n", line.c_str()); return true;
                }, "", 20);
            check(status == 91, "controlled interruption reaches the requested settings publication boundary");
            if (status != 91) continue;
            ModelManager restarted;
            const bool alternate_selected = action == "alternate" ? after : action == "registered" && !after;
            const int context = alternate_selected ? 4096 : action == "options" && after ? 8192 : 2048;
            check(view(restarted, alternate_selected ? "vllm" : "llamacpp", context),
                  "restart observes one complete recipe/effective/saved version after selection or option edit");
            check(bytes(root / "recipe_options.json") == legacy,
                  "profile-owned settings publication preserves the legacy file byte for byte");
            const auto state = restarted.get_execution_profiles(model);
            check(!state.contains("settings"), "public profile projection preserves its established fields");
        }
    }
}
static void failed_publication() {
    for (const bool options : {false, true}) {
        const auto root = test::make_scratch_dir("settings-failure"); use_root(root);
        ModelManager models; seed(models); models.select_execution_profile(model, {{"id", "registered"}});
        const auto profiles = root / "execution_profiles.json";
        const auto committed = bytes(profiles);
        const auto legacy = bytes(root / "recipe_options.json");
        bool failed = false;
        {
            BlockPublication blocked(profiles);
            try {
                if (options) models.update_saved_model_options(model, {{"ctx_size", 8192}});
                else alternate(models);
            } catch (const std::exception&) { failed = true; }
            check(failed && view(models, "llamacpp", 2048), "failed file replacement leaves the current cache and saved options committed");
        }
        ModelManager restarted;
        check(bytes(profiles) == committed && view(restarted, "llamacpp", 2048),
              "failed replacement preserves complete restart state");
        if (options) restarted.update_saved_model_options(model, {{"ctx_size", 8192}});
        else alternate(restarted);
        ModelManager accepted;
        check(view(accepted, options ? "llamacpp" : "vllm", options ? 8192 : 4096) &&
              bytes(root / "recipe_options.json") == legacy, "successful retry publishes once through the settings authority");
    }
}
static void legacy_and_clears() {
    const auto root = test::make_scratch_dir("settings-legacy"); use_root(root);
    { ModelManager models; seed(models); }
    // A pre-upgrade selected profile still reads its active overrides from the legacy file.
    const json registered = {{"id", "registered"}, {"recipe", "llamacpp"}, {"options", {{"ctx_size", 2048}}},
        {"saved_options", {{"ctx_size", 2048}, {"future_option", "preserved"}}}};
    const json other = {{"id", "alternate"}, {"recipe", "vllm"}, {"options", {{"ctx_size", 4096}}}};
    const json state = {{"selected", "alternate"}, {"profiles", {{"registered", registered}, {"alternate", other}}}};
    save(root / "execution_profiles.json", {{model, state}});
    save(root / "recipe_options.json", {{model, {{"ctx_size", 4096}, {"legacy_unknown", "untouched"}}}});
    const auto legacy = bytes(root / "recipe_options.json");
    ModelManager models;
    check(view(models, "vllm", 4096), "legacy profiles without settings ownership retain their previous active view");
    models.select_execution_profile(model, {{"id", "registered"}});
    check(view(models, "llamacpp", 2048) && models.get_saved_model_options(model).at("future_option") == "preserved",
          "legacy selection atomically adopts registered saved values including unknown fields");
    models.update_saved_model_options(model, {{"ctx_size", nullptr}});
    check(models.get_model_info(model).recipe_options.get_option("ctx_size") == 1536 &&
          !models.get_saved_model_options(model).contains("ctx_size"), "clearing owned overrides restores the registered nondefault value");
    models.update_saved_model_options(model, {{"ctx_size", -1}});
    ModelManager restarted;
    check(view(restarted, "llamacpp", -1) && bytes(root / "recipe_options.json") == legacy,
          "explicit negative context and unknown legacy fields survive restart without rewriting legacy storage");
    const auto stored = json::parse(bytes(root / "execution_profiles.json"));
    check(stored.at(model).at("settings").at("version") == 1 &&
          stored.at(model).at("settings").at("saved_options").at("ctx_size") == -1,
          "persisted profile row binds active saved options to the selected definition");
}
static void invalid_versions() {
    for (int variant = 0; variant != 8; ++variant) {
        const auto root = test::make_scratch_dir("settings-unsupported"); use_root(root);
        { ModelManager models; seed(models); models.select_execution_profile(model, {{"id", "registered"}}); }
        const auto profiles = root / "execution_profiles.json";
        auto stored = json::parse(bytes(profiles));
        if (variant == 0) stored[model]["settings"]["version"] = 2;
        if (variant == 1) stored[model]["settings"]["saved_options"] = json::array();
        if (variant == 2) stored[model]["profiles"]["registered"].erase("recipe");
        if (variant == 3) stored[model]["selected"] = "missing";
        if (variant == 4) stored = json::array();
        if (variant == 6) stored[model] = json::array();
        if (variant == 7) stored[model]["settings"]["version"] = "1";
        save(profiles, stored);
        if (variant == 5) { std::ofstream out(profiles); out << "{incomplete"; }
        const auto committed = bytes(profiles), legacy = bytes(root / "recipe_options.json");
        bool failed = false;
        try { ModelManager rejected; } catch (const std::exception&) { failed = true; }
        check(failed && bytes(profiles) == committed && bytes(root / "recipe_options.json") == legacy,
              "unsupported or incomplete stored settings reject activation without overwriting retained files");
    }
}
int main(int argc, char** argv) {
    auto defaults = ConfigFile::base_defaults();
    defaults["offline"] = true;
    defaults["no_fetch_executables"] = true;
    defaults["disable_model_filtering"] = true;
    RuntimeConfig runtime(defaults); RuntimeConfig::set_global(&runtime);
    if (argc == 4) {
        const auto isolated = test::state_dir(); use_root(utils::path_from_utf8(argv[2])); fs::remove_all(isolated);
        ModelManager models;
        const std::string action = argv[3];
        ModelPublicationTestHook::interrupt(models, std::string(argv[1]) == "--after", action == "options");
        if (action == "options") models.update_saved_model_options(model, {{"ctx_size", 8192}});
        else if (action == "registered") models.select_execution_profile(model, {{"id", "registered"}});
        else alternate(models);
        return 2;
    }
    const auto executable = utils::path_from_utf8(utils::get_executable_dir()) / utils::path_from_utf8(argv[0]).filename();
    interruptions(executable); failed_publication(); legacy_and_clears(); invalid_versions();
    RuntimeConfig::set_global(nullptr);
    std::fprintf(stderr, "Model settings publication: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
