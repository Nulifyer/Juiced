#include "lemon/alias_manager.h"
#include "lemon/utils/path_utils.h"
#include "lemon/utils/content_hash.h"
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
    check("corrupted bytes backed up while primary remains present", fs::is_regular_file(alias_path) &&
          fs::exists(temp_dir / "aliases.json.corrupted") &&
          lemon::utils::JsonUtils::load_from_file(alias_path.string()).is_object());

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
    check("alias recovery retains subsequent corrupted records separately", read_bytes(root / ("aliases.json.corrupted." + lemon::utils::string_sha256("invalid second"))) == "invalid second");
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

static void write_bytes(const fs::path& path, const std::string& contents) {
    std::ofstream stream(path, std::ios::binary | std::ios::trunc);
    stream << contents;
    stream.close();
    if (!stream) throw std::runtime_error("fixture could not write alias bytes");
}

static void test_alias_recovery_documents() {
    const std::vector<std::string> documents = {
        "[]", "null", "42", "true", R"("private original")", "", "{ incomplete\r\n",
        std::string("{\r\n\"original\":\"retained\"\n}") + std::string(1, '\0') + "trailing",
        R"({"valid":"Target-A","invalid":42,"self":"self","cycleA":"cycleB","cycleB":"cycleA"})"
    };
    for (const auto& original : documents) {
        const auto root = make_temp_dir();
        const auto path = root / "aliases.json";
        write_bytes(path, original);
        AliasManager manager(lemon::utils::path_to_utf8(root));
        const auto expected = original.find("\"valid\"") != std::string::npos ?
            lemon::AliasPublicationTestHook::Bindings{{"valid", "Target-A"}} : lemon::AliasPublicationTestHook::Bindings{};
        check("recovery preserves exact original bytes before canonical publication",
              read_bytes(root / "aliases.json.corrupted") == original);
        check("recovery publishes only supported acyclic bindings", manager.get_all_aliases() == expected &&
              lemon::utils::JsonUtils::load_from_file(path.string()) == nlohmann::json(expected));
        const auto canonical = read_bytes(path);
        AliasManager restarted(lemon::utils::path_to_utf8(root));
        check("recovered primary and backup survive unchanged restart", restarted.get_all_aliases() == expected &&
              read_bytes(path) == canonical && read_bytes(root / "aliases.json.corrupted") == original &&
              std::distance(fs::directory_iterator(root), fs::directory_iterator{}) == 2);
#ifndef _WIN32
        check("recovery backup is owner-only", (fs::status(root / "aliases.json.corrupted").permissions() &
              (fs::perms::group_all | fs::perms::others_all)) == fs::perms::none);
#endif
        fs::remove_all(root);
    }
    // Existing numbered recovery records remain operator data.
    const auto root = make_temp_dir();
    write_bytes(root / "aliases.json", "[]");
    write_bytes(root / "aliases.json.corrupted", "first retained");
    write_bytes(root / "aliases.json.corrupted.1", "historical retained");
    AliasManager manager(root.string());
    const auto digest = lemon::utils::string_sha256("[]");
    check("occupied backup selects a digest without changing historical records",
          read_bytes(root / "aliases.json.corrupted") == "first retained" &&
          read_bytes(root / "aliases.json.corrupted.1") == "historical retained" &&
          read_bytes(root / ("aliases.json.corrupted." + digest)) == "[]");
    write_bytes(root / "aliases.json", "[]");
    manager.load();
    check("identical damaged input reuses its verified backup", std::distance(fs::directory_iterator(root), fs::directory_iterator{}) == 4);
    write_bytes(root / "aliases.json", "[]");
    write_bytes(root / ("aliases.json.corrupted." + digest), "foreign occupied bytes");
    bool rejected = false;
    try { manager.load(); } catch (const lemon::AdmissionError& error) { rejected = error.code == "alias_configuration_recovery_required"; }
    check("occupied digest backup rejects without overwriting either record", rejected &&
          read_bytes(root / "aliases.json") == "[]" &&
          read_bytes(root / ("aliases.json.corrupted." + digest)) == "foreign occupied bytes");
    fs::remove_all(root);
}

static void test_alias_recovery_failures() {
    using Hook = lemon::AliasPublicationTestHook;
    for (const bool staging : {true, false}) {
        const auto root = make_temp_dir();
        AliasManager manager(root.string());
        std::string error;
        manager.set_alias("stable", "Target-A", error);
        const auto before = manager.get_all_aliases();
        const std::string original = "[\r\n\"unrecognized original\"\n]";
        const auto target = root / "aliases.json";
        write_bytes(target, original);
        Hook::writer(manager, [&](const std::string& path, const Hook::Bindings& candidate) {
            if (staging) {
                auto retained = root; retained += ".retained";
                fs::rename(root, retained); write_bytes(root, "not a directory");
                try { lemon::utils::JsonUtils::save_to_file(nlohmann::json(candidate), path); }
                catch (...) { fs::remove(root); fs::rename(retained, root); throw; }
                fs::remove(root); fs::rename(retained, root);
            } else {
                auto retained = target; retained += ".retained";
                try {
                    lemon::utils::JsonPersistenceTestHook::save(nlohmann::json(candidate), path, [&](const fs::path&) {
                        fs::rename(target, retained); fs::create_directory(target); write_bytes(target / "keep", "blocked");
                    });
                } catch (...) { fs::remove_all(target); fs::rename(retained, target); throw; }
            }
        });
        bool rejected = false;
        try { manager.load(); } catch (const std::exception&) { rejected = true; }
        check("failed recovery reports storage failure and retains live bindings", rejected && manager.get_all_aliases() == before);
        check("failed recovery retains original role and byte-exact backup", read_bytes(target) == original &&
              read_bytes(root / "aliases.json.corrupted") == original);
        check("failed recovery reclaims uncommitted staging", std::distance(fs::directory_iterator(root), fs::directory_iterator{}) == 2);
        Hook::writer(manager, {});
        AliasManager restarted(root.string());
        check("restart completes failed recovery without another backup", restarted.get_all_aliases().empty() &&
              read_bytes(root / "aliases.json.corrupted") == original &&
              std::distance(fs::directory_iterator(root), fs::directory_iterator{}) == 2);
        manager.load();
        check("original owner retries to the restarted committed view", manager.get_all_aliases() == restarted.get_all_aliases());
        fs::remove_all(root);
    }
}

static void test_alias_recovery_bounds_and_owner() {
    const auto root = make_temp_dir();
    AliasManager manager(root.string());
    std::string error;
    manager.set_alias("stable", "Target-A", error);
    const auto before = manager.get_all_aliases();
    const auto path = root / "aliases.json";
    std::string many = "[";
    for (int i = 0; i < 100001; ++i) many += "0,";
    many.back() = ']';
    for (const auto& original : {std::string(16 * 1024 * 1024 + 1, ' '),
         std::string(140, '[') + "0" + std::string(140, ']'), many}) {
        write_bytes(path, original);
        bool rejected = false;
        try { manager.load(); } catch (const lemon::AdmissionError& exception) {
            rejected = exception.code == "alias_configuration_recovery_required";
        }
        check("bounded alias reads reject without fallback, mutation or backup", rejected &&
              manager.get_all_aliases() == before && read_bytes(path) == original &&
              !fs::exists(root / "aliases.json.corrupted"));
    }
    write_bytes(path, "[]");
    lemon::ResourceAdmission admission;
    manager.set_resource_admission(admission);
    int validation_mode = 0;
    admission.set_storage_validator([&] {
        if (validation_mode == 0) throw std::runtime_error("root ownership changed");
        if (validation_mode == 1 && fs::exists(root / "aliases.json.corrupted"))
            write_bytes(path, "{\"external\":\"Target-B\"}");
    });
    bool rejected = false;
    try { manager.load(); } catch (const std::exception&) { rejected = true; }
    check("recovery validates native root ownership before backup", rejected && read_bytes(path) == "[]" &&
          !fs::exists(root / "aliases.json.corrupted") && manager.get_all_aliases() == before);
    validation_mode = 1;
    rejected = false;
    try { manager.load(); } catch (const lemon::AdmissionError& exception) { rejected = exception.code == "alias_configuration_recovery_required"; }
    check("recovery revalidates source before final publication and preserves a competing edit", rejected &&
          read_bytes(path) == "{\"external\":\"Target-B\"}" && read_bytes(root / "aliases.json.corrupted") == "[]" &&
          manager.get_all_aliases() == before);
    write_bytes(path, "[]");
    validation_mode = 2;
    std::promise<void> arrived, release;
    const auto resume = release.get_future().share();
    lemon::AliasPublicationTestHook::writer(manager, [&](const std::string& destination, const auto& candidate) {
        arrived.set_value(); resume.wait();
        lemon::utils::JsonUtils::save_to_file(nlohmann::json(candidate), destination);
    });
    auto loading = std::async(std::launch::async, [&] { manager.load(); });
    const bool reached = arrived.get_future().wait_for(std::chrono::seconds(5)) == std::future_status::ready;
    admission.maintenance(true);
    check("accepted alias recovery retains writer ownership through final publication", reached &&
          !admission.wait_for_writes(std::chrono::milliseconds(5)) && fs::is_regular_file(path) &&
          read_bytes(root / "aliases.json.corrupted") == "[]");
    rejected = false;
    try { manager.load(); } catch (const lemon::AdmissionError& exception) { rejected = exception.code == "maintenance_active"; }
    check("maintenance rejects fresh recovery without disturbing accepted publication", rejected && read_bytes(path) == "[]");
    release.set_value(); loading.get();
    check("accepted recovery publishes and drains during maintenance", manager.get_all_aliases().empty() &&
          lemon::utils::JsonUtils::load_from_file(path.string()).is_object() && admission.wait_for_writes(std::chrono::milliseconds(100)));
    admission.maintenance(false);
    lemon::AliasPublicationTestHook::writer(manager, {});
    fs::remove_all(root);
}

static void test_byte_preserving_atomic_publication() {
    const auto root = make_temp_dir();
    const auto path = root / "original.json";
    const std::string original = "kept original";
    const std::string candidate("\r\nline\nwith\0bytes", 17);
    write_bytes(path, original);
    bool rejected = false;
    try {
        lemon::utils::JsonUtils::save_text_to_file(candidate, path.string(), [&](const fs::path& staged) {
            check("raw atomic staging preserves CRLF, LF and null bytes", read_bytes(staged) == candidate);
            throw std::runtime_error("injected publication rejection");
        });
    } catch (const std::exception&) { rejected = true; }
    check("raw publication rejection retains destination and cleans staging", rejected && read_bytes(path) == original &&
          std::distance(fs::directory_iterator(root), fs::directory_iterator{}) == 1);
    lemon::utils::JsonUtils::save_text_to_file(candidate, path.string());
    check("raw publication round-trips exact original bytes", read_bytes(path) == candidate);
    fs::remove_all(root);
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
    test_alias_recovery_documents();
    test_alias_recovery_failures();
    test_alias_recovery_bounds_and_owner();
    test_byte_preserving_atomic_publication();
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
