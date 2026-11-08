#include "lemon/memory_budget.h"
#include <chrono>
#include <cstdio>
#include <fstream>
#include <limits>

namespace fs = std::filesystem;

struct Fixture {
    fs::path root = fs::temp_directory_path() /
        ("juiced-memory-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    Fixture() {
        write("proc/meminfo", "MemTotal: 1000 kB\nMemAvailable: 400 kB\n");
    }
    ~Fixture() { std::error_code ec; fs::remove_all(root, ec); }
    void write(const fs::path& name, const std::string& text) {
        fs::create_directories((root / name).parent_path());
        std::ofstream(root / name) << text;
    }
    void v2(const std::string& group = "/", const std::string& mount_root = "/") {
        write("proc/self/cgroup", "0::" + group + "\n");
        write("proc/self/mountinfo", "31 24 0:28 " + mount_root +
              " /sys/fs/cgroup rw - cgroup2 cgroup rw\n");
    }
    void scope(const fs::path& group, const std::string& limit, const std::string& used) {
        write(fs::path("sys/fs/cgroup") / group / "memory.max", limit + "\n");
        write(fs::path("sys/fs/cgroup") / group / "memory.current", used + "\n");
    }
    lemon::MemoryBudget read() const { return lemon::read_linux_memory_budget(root / "proc", root); }
};

int main() {
    int failed = 0;
    int checks = 0;
    auto check = [&](bool condition, const char* name) {
        ++checks;
        if (!condition) { std::fprintf(stderr, "FAIL: %s\n", name); ++failed; }
    };
    {
        Fixture f; f.v2(); f.scope("", "200000", "150000");
        const auto b = f.read();
        check(b.host_total_bytes == 1024000 && b.host_available_bytes == 409600,
              "host MemAvailable is distinct from total capacity");
        check(b.available_bytes() == 50000, "finite container headroom limits host capacity");
    }
    {
        Fixture f; f.v2(); f.scope("", "max", "100");
        check(f.read().available_bytes() == 409600, "unlimited cgroup preserves host available memory");
        f.scope("", "100", "101");
        check(f.read().available_bytes() == 0, "over-limit is measured zero without unsigned underflow");
        f.scope("", "0", "0");
        check(f.read().available_bytes() == 0, "explicit zero ceiling remains zero");
    }
    {
        Fixture f; f.v2("/parent/child");
        f.write("sys/fs/cgroup/cgroup.controllers", "cpu memory\n");
        f.scope("parent", "200000", "190000"); f.scope("parent/child", "max", "100");
        check(f.read().available_bytes() == 10000, "unlimited leaf obeys tighter parent headroom");
        f.scope("parent/child", "150000", "149000");
        check(f.read().available_bytes() == 1000, "each ancestor uses its own charge and ceiling");
        check(f.read().constraints.size() == 2, "ancestor constraints are not added together");
        fs::remove(f.root / "sys/fs/cgroup/parent/memory.current");
        check(!f.read().available_bytes(), "unreadable finite ancestor makes effective headroom unknown");
    }
    {
        Fixture f; f.v2("/", "/docker/container"); f.scope("", "200000", "100000");
        check(f.read().available_bytes() == 100000, "container namespace maps group root to mounted subtree");
        f.v2("/docker/container/child", "/docker/container"); f.scope("child", "80000", "79000");
        check(f.read().available_bytes() == 1000, "host cgroup namespace resolves mounted subtree correctly");
    }
    {
        Fixture f; f.v2("/outside", "/container"); f.scope("", "200000", "100000");
        check(!f.read().available_bytes(), "unresolvable mount is not unlimited capacity");
        f.v2("/../escape");
        check(!f.read().available_bytes(), "membership cannot escape mount boundary");
    }
    {
        Fixture f; f.v2(); f.scope("", "18446744073709551616", "1");
        check(!f.read().available_bytes(), "overflowing limit is unknown");
        f.scope("", "-1", "0");
        check(!f.read().available_bytes(), "negative ceiling is not unlimited v2");
        f.scope("", "10", "unknown");
        check(!f.read().available_bytes(), "invalid usage does not become zero");
        f.scope("", "10", "0 trailing-data");
        check(!f.read().available_bytes(), "malformed usage is not accepted partially");
    }
    {
        Fixture f; f.v2(); f.scope("", "max", "0");
        f.write("proc/meminfo", "MemTotal: 100 kB\nMemAvailable: 200 kB\n");
        check(!f.read().available_bytes(), "inconsistent physical sample is not free memory");
        f.write("proc/meminfo", "MemTotal: 100 kB\nMemFree: 50 kB\n");
        check(!f.read().available_bytes(), "MemFree is not substituted for missing MemAvailable");
        f.write("proc/meminfo", "MemTotal: 18446744073709551615 kB\nMemAvailable: 1 kB\n");
        check(!f.read().available_bytes(), "meminfo byte conversion cannot overflow");
    }
    {
        Fixture f;
        check(!f.read().available_bytes(), "missing membership never implies unlimited containment");
    }
    {
        Fixture f;
        f.write("proc/self/cgroup", "4:cpu,cpuacct:/box\n6:memory:/box\n");
        f.write("proc/self/mountinfo", "31 24 0:28 /box /sys/fs/cgroup/memory rw - cgroup cgroup rw,memory\n");
        f.write("sys/fs/cgroup/memory/memory.limit_in_bytes", "200000\n");
        f.write("sys/fs/cgroup/memory/memory.usage_in_bytes", "160000\n");
        check(f.read().available_bytes() == 40000, "v1 memory controller mount is selected");
        f.write("sys/fs/cgroup/memory/memory.limit_in_bytes", "9223372036854771712\n");
        check(f.read().available_bytes() == 409600, "v1 unlimited sentinel is not physical capacity");
    }
    {
        Fixture f;
        f.write("proc/self/cgroup", "6:memory:/parent/child\n");
        f.write("proc/self/mountinfo", "31 24 0:28 / /sys/fs/cgroup rw - cgroup cgroup rw,memory\n");
        for (const auto& path : {"", "parent", "parent/child"}) {
            f.write(fs::path("sys/fs/cgroup") / path / "memory.limit_in_bytes", "max\n");
            f.write(fs::path("sys/fs/cgroup") / path / "memory.usage_in_bytes", "0\n");
            f.write(fs::path("sys/fs/cgroup") / path / "memory.use_hierarchy", "1\n");
        }
        f.write("sys/fs/cgroup/parent/child/memory.limit_in_bytes", "200000\n");
        f.write("sys/fs/cgroup/parent/memory.limit_in_bytes", "100000\n");
        f.write("sys/fs/cgroup/parent/memory.usage_in_bytes", "95000\n");
        f.write("sys/fs/cgroup/memory.limit_in_bytes", "9223372036854771712\n");
        check(f.read().available_bytes() == 5000, "v1 hierarchical ancestor limit applies");
        f.write("sys/fs/cgroup/parent/memory.use_hierarchy", "0\n");
        check(f.read().available_bytes() == 200000, "v1 nonhierarchical parent is not falsely charged");
    }
    std::printf("Memory budget checks: %d checks, %d failures\n", checks, failed);
    return failed ? 1 : 0;
}
