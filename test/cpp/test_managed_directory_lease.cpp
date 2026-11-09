#include "lemon/utils/managed_directory_lease.h"

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <string>

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
        failed = false;
        try { Lease empty({fs::path{}}); } catch (const std::exception&) { failed = true; }
        check(failed, "empty managed roots never silently select a default store");
    } catch (const std::exception& error) {
        std::fprintf(stderr, "Unexpected directory lease failure: %s\n", error.what());
        return 1;
    }
    std::fprintf(stderr, "Managed directory leases: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
