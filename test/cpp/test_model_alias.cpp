#include "lemon/alias_manager.h"
#include "lemon/utils/path_utils.h"
#include "lemon/utils/json_utils.h"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <functional>
#include <future>
#include <mutex>
#include <fstream>
#include <iostream>
#include <iterator>
#include <memory>
#include <string>
#include <vector>

namespace fs = std::filesystem;
using lemon::AliasManager;

namespace lemon {
struct AliasPublicationTestHook {
    using Bindings = std::unordered_map<std::string, std::string>;
    static void writer(AliasManager& manager, std::function<void(const std::string&, const Bindings&)> action) {
        std::unique_lock lock(manager.write_mutex_);
        manager.persist_override_ = std::move(action);
    }
};
namespace utils {
struct JsonPersistenceTestHook {
    static void save(const json& value, const std::string& path,
                     const std::function<void(const fs::path&)>& before_replace) {
        JsonUtils::save_to_file(value, path, before_replace);
    }
};
}
}

static int g_failures = 0, g_checks = 0;

static void check(const char* name, bool ok) {
    ++g_checks;
    std::printf("[%s] %s\n", ok ? "PASS" : "FAIL", name);
    if (!ok) ++g_failures;
}

#ifdef _WIN32
static void set_test_env(const char* key, const char* val) {
    _putenv_s(key, val);
}
#else
static void set_test_env(const char* key, const char* val) {
    setenv(key, val, 1);
}
#endif

static fs::path make_temp_dir() {
    fs::path dir = fs::temp_directory_path();
    dir /= "lemonade_test_alias_" +
           std::to_string(std::chrono::steady_clock::now().time_since_epoch().count());
    fs::create_directories(dir);
    return dir;
}

static void test_alias_manager_basic() {
    fs::path temp_dir = make_temp_dir();
    std::string cache_dir = temp_dir.string();

    AliasManager mgr(cache_dir);
    std::string err;

    check("set_alias valid", mgr.set_alias("coding", "Qwen3-8B-GGUF", err));
    check("has_alias coding", mgr.has_alias("coding"));
    check("resolve_alias coding", mgr.resolve_alias("coding") == "Qwen3-8B-GGUF");
    check("resolve_alias unmapped returns nullopt", !mgr.resolve_alias("unknown").has_value());

    auto all = mgr.get_all_aliases();
    check("get_all_aliases count == 1", all.size() == 1);
    check("get_all_aliases coding key", all["coding"] == "Qwen3-8B-GGUF");

    AliasManager mgr2(cache_dir);
    check("reloaded has_alias coding", mgr2.has_alias("coding"));
    check("reloaded resolve_alias coding", mgr2.resolve_alias("coding") == "Qwen3-8B-GGUF");

    check("remove_alias returns true", mgr2.remove_alias("coding"));
    check("remove_alias removes key", !mgr2.has_alias("coding"));

    AliasManager mgr3(cache_dir);
    check("reloaded after remove is empty", mgr3.get_all_aliases().empty());

    std::error_code ec;
    fs::remove_all(temp_dir, ec);
}

static void test_alias_manager_validation_and_cycles() {
    fs::path temp_dir = make_temp_dir();
    std::string cache_dir = temp_dir.string();

    AliasManager mgr(cache_dir);
    std::string err;

    check("self-referential rejected", !mgr.set_alias("self", "self", err));
    check("empty alias rejected", !mgr.set_alias("", "target", err));
    check("empty target rejected", !mgr.set_alias("alias", "", err));

    check("set A -> B", mgr.set_alias("aliasA", "aliasB", err));
    check("set B -> A cycle rejected", !mgr.set_alias("aliasB", "aliasA", err));

    check("set B -> C", mgr.set_alias("aliasB", "targetC", err));
    check("chained resolve A -> C", mgr.resolve_alias("aliasA") == "targetC");

    std::error_code ec;
    fs::remove_all(temp_dir, ec);
}

static void test_alias_corrupted_json_recovery() {
    fs::path temp_dir = make_temp_dir();
    std::string cache_dir = temp_dir.string();

    fs::path alias_path = temp_dir / "aliases.json";
    {
        std::ofstream file(alias_path);
        file << "{ corrupted json object without closing brace: ";
    }

    AliasManager mgr(cache_dir);
    check("corrupted json recovers with empty map", mgr.get_all_aliases().empty());
    check("corrupted file renamed", fs::exists(temp_dir / "aliases.json.corrupted"));

    std::error_code ec;
    fs::remove_all(temp_dir, ec);
}

static void test_alias_manager_standalone_resolution() {
    fs::path temp_dir = make_temp_dir();
    std::string cache_dir = temp_dir.string();

    AliasManager alias_mgr(cache_dir);
    std::string err;
    alias_mgr.set_alias("coding-test", "Tiny-Test-Model-GGUF", err);

    auto res_alias = alias_mgr.resolve_alias("coding-test");
    check("resolve_alias standalone resolves target", res_alias.has_value() && res_alias.value() == "Tiny-Test-Model-GGUF");

    std::error_code ec;
    fs::remove_all(temp_dir, ec);
}

static void test_alias_disk_cycle_pruning() {
    fs::path temp_dir = make_temp_dir();
    std::string cache_dir = temp_dir.string();

    fs::path alias_path = temp_dir / "aliases.json";
    {
        std::ofstream file(alias_path);
        file << "{\n  \"cycleA\": \"cycleB\",\n  \"cycleB\": \"cycleA\"\n}\n";
    }

    AliasManager mgr(cache_dir);
    check("disk loaded cycle pruned from map", mgr.get_all_aliases().empty());
    check("disk loaded cyclic alias resolves to nullopt", !mgr.resolve_alias("cycleA").has_value());

    std::error_code ec;
    fs::remove_all(temp_dir, ec);
}

static std::string read_bytes(const fs::path& path) {
    std::ifstream stream(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(stream), std::istreambuf_iterator<char>()};
}

static void test_alias_recovery_ownership() {
    const auto root = make_temp_dir();
    { std::ofstream file(root / "aliases.json"); file << "invalid original"; }
    AliasManager manager(lemon::utils::path_to_utf8(root));
    { std::ofstream file(root / "aliases.json"); file << "invalid second"; }
    manager.load();
    check("alias recovery retains the first corrupted record", read_bytes(root / "aliases.json.corrupted") == "invalid original");
    check("alias recovery retains subsequent corrupted records separately", read_bytes(root / "aliases.json.corrupted.1") == "invalid second");
    std::string error;
    manager.set_alias("stable", "Target-A", error);
    const auto before = manager.get_all_aliases();
    { std::ofstream file(root / "not-directory"); file << "blocked"; }
    bool failed = false;
    try { manager.set_cache_dir(lemon::utils::path_to_utf8(root / "not-directory")); }
    catch (const std::exception&) { failed = true; }
    check("failed alias directory change retains committed directory and bindings", failed &&
          manager.get_cache_dir() == lemon::utils::path_to_utf8(root) && manager.get_all_aliases() == before);
    std::error_code ec; fs::remove_all(root, ec);
}

static void test_alias_publication_failures() {
    using Hook = lemon::AliasPublicationTestHook;
    for (const bool staging : {true, false}) for (const std::string action : {"create", "replace", "remove"}) {
        const auto root = make_temp_dir();
        AliasManager manager(lemon::utils::path_to_utf8(root));
        std::string error;
        manager.set_alias("stable", "Target-A", error);
        const auto before = manager.get_all_aliases();
        const auto committed = read_bytes(root / "aliases.json");
#ifndef _WIN32
        check("alias files retain owner-only permissions", (fs::status(root / "aliases.json").permissions() &
              (fs::perms::group_all | fs::perms::others_all)) == fs::perms::none);
#endif
        Hook::writer(manager, [&](const std::string& path, const Hook::Bindings& candidate) {
            if (staging) {
                auto retained = root; retained += ".retained";
                fs::rename(root, retained);
                { std::ofstream blocked(root); blocked << "not a directory"; }
                try { lemon::utils::JsonUtils::save_to_file(nlohmann::json(candidate), path); }
                catch (...) { fs::remove(root); fs::rename(retained, root); throw; }
                fs::remove(root); fs::rename(retained, root);
            } else {
                const auto target = lemon::utils::path_from_utf8(path);
                auto retained = target; retained += ".retained";
                try {
                    lemon::utils::JsonPersistenceTestHook::save(nlohmann::json(candidate), path, [&](const fs::path&) {
                        fs::rename(target, retained);
                        fs::create_directory(target);
                        std::ofstream blocked(target / "keep"); blocked << "preserve directory";
                    });
                } catch (...) {
                    if (fs::exists(retained)) { fs::remove_all(target); fs::rename(retained, target); }
                    throw;
                }
                fs::remove_all(target); fs::rename(retained, target);
            }
        });
        const auto mutate = [&]() {
            return action == "remove" ? manager.remove_alias("stable") :
                manager.set_alias(action == "create" ? "candidate" : "stable", "Target-B", error);
        };
        bool failed = false;
        try { mutate(); } catch (const std::exception&) { failed = true; }
        check("alias mutation reports failed persistence", failed);
        check("failed alias mutation preserves live bindings", manager.get_all_aliases() == before);
        check("failed alias mutation preserves committed bytes", read_bytes(root / "aliases.json") == committed);
        AliasManager restarted(lemon::utils::path_to_utf8(root));
        check("failed alias mutation preserves restart resolution", restarted.get_all_aliases() == before);
        Hook::writer(manager, {});
        check("failed alias mutation remains retryable", mutate());
        restarted.load();
        check("successful alias retry agrees after restart", restarted.get_all_aliases() == manager.get_all_aliases());
        std::error_code ec; fs::remove_all(root, ec);
    }
}

static void test_alias_publication_overlap() {
    using Hook = lemon::AliasPublicationTestHook;
    const auto root = make_temp_dir();
    AliasManager manager(lemon::utils::path_to_utf8(root));
    std::string error;
    manager.set_alias("stable", "Target-A", error);
    std::promise<void> arrived, release, reading;
    auto entered = arrived.get_future();
    auto resume = release.get_future().share();
    auto reader_entered = reading.get_future();
    Hook::writer(manager, [&](const std::string& path, const Hook::Bindings& candidate) {
        lemon::utils::JsonPersistenceTestHook::save(nlohmann::json(candidate), path, [&](const fs::path&) {
            if (!candidate.count("candidate")) return;
            arrived.set_value();
            resume.wait();
            throw std::runtime_error("injected final alias publication failure");
        });
    });
    auto failed_writer = std::async(std::launch::async, [&]() {
        std::string detail;
        try { return manager.set_alias("candidate", "Target-B", detail); }
        catch (const std::exception&) { return false; }
    });
    check("alias writer reaches staged publication", entered.wait_for(std::chrono::seconds(5)) == std::future_status::ready);
    auto reader = std::async(std::launch::async, [&]() { reading.set_value(); return manager.get_all_aliases(); });
    reader_entered.wait();
    check("alias readers retain committed lookup access during file publication", reader.wait_for(std::chrono::seconds(5)) == std::future_status::ready);
    auto next_writer = std::async(std::launch::async, [&]() { std::string detail; return manager.set_alias("later", "Target-C", detail); });
    release.set_value();
    check("failed staged alias write is reported", !failed_writer.get());
    check("serialized alias writer succeeds after failure", next_writer.get());
    check("overlapping alias read excludes rejected candidate", reader.get().count("candidate") == 0);
    AliasManager restarted(lemon::utils::path_to_utf8(root));
    check("overlapping alias publication agrees after restart", restarted.get_all_aliases() == manager.get_all_aliases() &&
          restarted.resolve_alias("later") == "Target-C" && !restarted.has_alias("candidate"));
    std::error_code ec; fs::remove_all(root, ec);
}

int main() {
    std::printf("Running Standalone AliasManager Unit Tests...\n");

    test_alias_manager_basic();
    test_alias_manager_validation_and_cycles();
    test_alias_corrupted_json_recovery();
    test_alias_manager_standalone_resolution();
    test_alias_disk_cycle_pruning();
    test_alias_recovery_ownership();
    test_alias_publication_failures();
    test_alias_publication_overlap();

    if (g_failures > 0) {
        std::printf("\n!!! %d TEST(S) FAILED !!!\n", g_failures);
        return 1;
    }

    std::printf("\n%d checks, %d failures\n", g_checks, g_failures);
    std::printf("\nALL TESTS PASSED!\n");
    return 0;
}
