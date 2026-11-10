#include "lemon/utils/managed_directory_lease.h"

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <future>
#include <string>

#ifdef _WIN32
#include <windows.h>
#endif

namespace fs = std::filesystem;
using Lease = lemon::utils::ManagedDirectoryLease;

int main() {
    const auto root = fs::temp_directory_path() /
        ("juiced-directory-lease-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    struct Cleanup {
        fs::path root;
        ~Cleanup() { std::error_code ec; fs::remove_all(root, ec); }
    } cleanup{root};
    int checks = 0, failures = 0;
    const auto check = [&](bool condition, const char* name) {
        ++checks;
        std::fprintf(stderr, "CHECK %d: %s\n", checks, name);
        if (!condition) { ++failures; std::fprintf(stderr, "FAIL: %s\n", name); }
    };
    const auto busy = [&](const fs::path& path) {
        try { Lease other({path}); return false; }
        catch (const std::exception& error) { return std::string(error.what()).find("already in use") != std::string::npos; }
    };
    try {
        const auto a = root / "a", b = root / "b", c = root / "c";
        {
            Lease first({a, a / "."});
            first.protect(a);
            check(busy(a), "duplicate root arguments do not defeat exclusive ownership");
            Lease independent({b});
            check(busy(b), "independent managed roots can be owned simultaneously");
            bool failed = false;
            try { Lease partial({c, b}); } catch (const std::exception&) { failed = true; }
            check(failed, "partial acquisition reports a conflicting root");
            Lease reclaimed({c});
            check(busy(c), "failed construction releases already acquired roots");
            failed = false;
            try { first.protect(b); } catch (const std::exception&) { failed = true; }
            check(failed && busy(a), "failed extension preserves existing ownership");
#ifndef _WIN32
            const auto alias = root / "alias";
            fs::create_directory_symlink(a, alias);
            first.protect(alias);
            check(busy(alias), "symlinked directory identity shares the same lease");
#endif
        }
        check(fs::is_regular_file(a / Lease::file_name) && fs::file_size(a / Lease::file_name) == 0,
              "release retains the empty control file rather than unlinking its inode");
        { Lease reopened({a, b, c}); check(busy(a), "released roots can be acquired again"); }
        const auto unicode = root / fs::u8path(u8"cache-é-模型");
        { Lease owned({unicode}); check(busy(unicode), "Unicode managed directory paths preserve exclusivity"); }
        const auto invalid = root / "invalid";
        fs::create_directories(invalid / Lease::file_name);
        bool failed = false;
        try { Lease unusable({invalid}); } catch (const std::exception&) { failed = true; }
        check(failed && fs::is_directory(invalid / Lease::file_name), "unusable lease files fail closed without deleting them");
        fs::remove(invalid / Lease::file_name);
        { Lease recovered({invalid}); check(busy(invalid), "failed control-file acquisition releases partial ownership for retry"); }
        failed = false;
        try { Lease empty({fs::path{}}); } catch (const std::exception&) { failed = true; }
        check(failed, "empty managed roots never silently select a default store");
        const auto preview = root / "uncreated-root-preview";
        check(Lease::resolve_root(preview / ".") == fs::canonical(root) / preview.filename() && !fs::exists(preview),
              "root preflight normalizes missing aliases without creating directories");
        {
            Lease owner({a});
            auto first = owner.claim(b);
            auto second = owner.claim(b / ".");
            check(first.root() == fs::canonical(b) && busy(b), "scoped claims share canonical ownership within one owner");
            first = {};
            check(busy(b), "one released view cannot drop another live claim");
            second = {};
            check(!busy(b), "the final scoped view releases its control handle");
            auto fixed = owner.claim(a); fixed = {};
            check(busy(a), "a scoped alias cannot release fixed startup ownership");
            bool released = true;
            for (int i = 0; i < 40; ++i) {
                const auto temporary = root / ("candidate-" + std::to_string(i));
                { auto claim = owner.claim(temporary); }
                released = released && !busy(temporary);
            }
            check(released, "forty discarded directory candidates retain no live handles");
            auto concurrent = std::async(std::launch::async, [&] { return owner.claim(c); });
            auto local = owner.claim(c);
            auto remote = concurrent.get(); local = {};
            check(busy(c), "concurrent claims retain one shared lock through their final view");
            remote = {};
            check(!busy(c), "concurrent completion releases only its scoped root");
        }
        Lease::Claim retained;
        { Lease transient({}); retained = transient.claim(b); }
        check(busy(b), "a retained claim survives its original owner container");
        retained = {};
        check(!busy(b), "retained claim teardown finally reopens its directory");
        const auto control = b / Lease::file_name;
        const auto permissions = fs::status(control).permissions();
        fs::permissions(control, fs::perms::owner_write | fs::perms::group_write | fs::perms::others_write, fs::perm_options::remove);
        { Lease read_only_control({b}); check(busy(b), "an existing read-only control file still provides exclusive ownership"); }
        fs::permissions(control, permissions);
        {
            const auto live = root / "replaced-live-root", moved = root / "retained-moved-root";
            const auto source = live / "source.txt";
            Lease owner({});
            auto retained = owner.claim(live);
            auto alias = owner.claim(live / ".");
            owner.validate();
            { std::ofstream stream(source); stream << "retained source\n"; }
            std::error_code rename_error;
            fs::rename(live, moved, rename_error);
            if (rename_error) {
#ifdef _WIN32
                check(rename_error == std::error_code(ERROR_ACCESS_DENIED, std::system_category()) ||
                      rename_error == std::error_code(ERROR_SHARING_VIOLATION, std::system_category()),
                      "the platform explicitly denies the controlled live-root rename");
                std::fprintf(stderr, "SKIP: live-root replacement requires directory rename capability: %s\n",
                             rename_error.message().c_str());
                auto reused = owner.claim(live / ".");
                check(reused && busy(live) && fs::is_regular_file(source),
                      "a denied rename preserves retained ownership and the source file");
#else
                throw fs::filesystem_error("Controlled live-root rename failed", live, moved, rename_error);
#endif
            } else {
                check(!fs::exists(live) && fs::is_regular_file(moved / "source.txt"),
                      "the controlled rename moves the retained directory before replacement");
                Lease replacement({live});
                { std::ofstream stream(live / "source.txt"); stream << "replacement source\n"; }
                check(busy(live), "an independent owner holds the replacement directory");
                bool invalid = false;
                try { owner.validate(); }
                catch (const std::exception&) { invalid = true; }
                check(invalid, "validation rejects a completed replacement without reacquiring the stale root");
                replacement.validate();
                bool rejected = false;
                try { auto stale = owner.claim(live); }
                catch (const std::exception& error) {
                    rejected = std::string(error.what()) == "Managed directory identity changed: " + retained.root().u8string();
                }
                check(rejected, "exact-root reuse rejects a replacement inode with the fixed identity diagnostic");
#ifndef _WIN32
                const auto child = live / "uncreated-child";
                rejected = false;
                try { auto stale = owner.claim(child); }
                catch (const std::exception& error) {
                    rejected = std::string(error.what()) == "Managed directory identity changed: " + retained.root().u8string();
                }
                check(rejected && !fs::exists(child),
                      "a stale ancestor cannot authorize descendant creation in the replacement directory");
#endif
                auto replacement_alias = replacement.claim(live / ".");
                check(replacement_alias && busy(live), "stale-owner rejection preserves healthy replacement ownership");
                check(busy(moved), "the moved original inode remains excluded by its retained directory handle");
                retained = {};
                check(busy(moved), "a retained alias keeps the moved original inode excluded");
                alias = {};
                check(!busy(moved) && busy(live), "the last old claim releases only the moved original inode");
                std::string old_text, replacement_text;
                { std::ifstream stream(moved / "source.txt"); std::getline(stream, old_text); }
                { std::ifstream stream(live / "source.txt"); std::getline(stream, replacement_text); }
                check(old_text == "retained source" && replacement_text == "replacement source",
                      "replacement rejection and old-claim release preserve both source files");
            }
        }
#ifndef _WIN32
        const auto hierarchy = root / "hierarchy";
        {
            Lease parent({hierarchy});
            check(busy(hierarchy / "new-child"), "another owner cannot acquire a descendant root");
            check(!fs::exists(hierarchy / "new-child"), "rejected descendant acquisition performs no directory creation");
            auto child = parent.claim(hierarchy / "own-child");
            check(busy(hierarchy / "own-child"), "the same owner can retain a nested root under its ancestor");
        }
        {
            Lease child({hierarchy / "child-first"});
            check(busy(hierarchy), "another owner cannot acquire an ancestor root");
            Lease sibling({hierarchy / "independent-sibling"});
            check(busy(hierarchy / "independent-sibling"), "independent siblings share ancestor intent locks");
        }
        {
            Lease nested({hierarchy / "constructor-child", hierarchy});
            check(busy(hierarchy) && busy(hierarchy / "constructor-child"), "startup roots acquire broad ownership before nested claims");
        }
        {
            const auto parent = root / "new-trailing-startup";
            Lease nested({parent / "child", parent / ""});
            check(busy(parent) && busy(parent / "child"), "new parent aliases normalize before startup ownership ordering");
        }
        {
            Lease owner({});
            const auto trailing = root / "new-trailing-claim", dot = root / "new-dot-claim";
            auto first = owner.claim(trailing / "");
            check(first.root() == fs::canonical(trailing) && busy(trailing), "a new trailing-separator claim resolves to the actual directory");
            auto second = owner.claim(dot / ".");
            check(second.root() == fs::canonical(dot) && busy(dot), "a new dot alias cannot acquire its target as an ancestor intent");
        }
        {
            Lease first({root / "prefix"});
            Lease sibling({root / "prefix-sibling"});
            check(busy(root / "prefix") && busy(root / "prefix-sibling"), "similar directory names do not imply an ancestor relationship");
        }
        {
            Lease owner({});
            auto child = owner.claim(hierarchy / "upgrade-child");
            bool rejected = false;
            try { auto broader = owner.claim(hierarchy); } catch (const std::exception&) { rejected = true; }
            check(rejected && busy(hierarchy / "upgrade-child"), "a broader claim cannot drop a live narrower ownership lock");
            child = {};
            auto broader = owner.claim(hierarchy);
            check(busy(hierarchy), "a broader claim succeeds after narrower ownership is released");
        }
        {
            Lease owner({});
            auto parent = owner.claim(hierarchy);
            auto child = owner.claim(hierarchy / "retained-child");
            parent = {};
            check(busy(hierarchy), "a child claim retains the ancestor that authorizes it");
            child = {};
            check(!busy(hierarchy), "the last descendant view releases its inherited ancestor ownership");
        }
        const auto removed_control_root = root / "removed-control";
        {
            Lease owner({removed_control_root});
            const auto removed_control = removed_control_root / Lease::file_name;
            check(fs::remove(removed_control), "the controlled fault removes a live control-file inode");
            check(busy(removed_control_root), "control-file removal cannot admit another owner of the live directory");
            check(!fs::exists(removed_control), "a rejected contender cannot publish a replacement control file");
            owner.protect(removed_control_root);
            check(busy(removed_control_root), "the original owner retains its directory anchor after control-file removal");
        }
        {
            Lease recovered({removed_control_root});
            check(fs::is_regular_file(removed_control_root / Lease::file_name) && busy(removed_control_root),
                  "confirmed owner release permits control-file recreation under a new directory anchor");
        }
        const auto root_permissions = fs::status(b).permissions();
        fs::permissions(b, fs::perms::owner_write | fs::perms::group_write | fs::perms::others_write, fs::perm_options::remove);
        { Lease read_only_root({b}); check(busy(b), "an existing read-only directory supports the inode anchor without writes"); }
        fs::permissions(b, root_permissions);
#endif
    } catch (const std::exception& error) {
        std::fprintf(stderr, "Unexpected directory lease failure: %s\n", error.what());
        return 1;
    }
    std::fprintf(stderr, "Managed directory leases: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
