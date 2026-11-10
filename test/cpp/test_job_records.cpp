#include "lemon/jobs/job_manager.h"
#include "support/test_state_isolation.h"

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <future>
#include <iterator>
#include <thread>

using lemon::ResourceAdmission;
using lemon::jobs::JobManager;
using lemon::jobs::JobError;
using lemon::jobs::json;
using namespace std::chrono_literals;
namespace fs = std::filesystem;
static int checks = 0, failures = 0;
static void check(bool ok, const char* label) {
    ++checks;
    std::fprintf(stderr, "%s: %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) ++failures;
}
static void write(const fs::path& file, const std::string& text) { std::ofstream output(file); output << text; }
static std::string read(const fs::path& file) { std::ifstream input(file); return {std::istreambuf_iterator<char>(input), {}}; }
namespace lemon::jobs {
struct JobRecordTestHook {
    static std::unique_lock<std::mutex> lock(JobManager& jobs) { return std::unique_lock(jobs.mutex_); }
};
}
static void history() {
    const auto root = lemon::test::make_scratch_dir("retained-history");
    const auto target = root / "jobs.json";
    const json retained = {{"id", "history"}, {"status", "running"}, {"cursor", "first"},
        {"custom", {{"future", json::array({1, 2, 3})}}},
        {"steps", json::array({{{"id", "first"}, {"op", "unsupported"}, {"status", "running"}, {"future_step", true}}})}};
    const json pending = {{"id", "pending"}, {"status", "interrupted"}, {"deleted", true}, {"unknown_owner", "prior-manager"}};
    const json document = {{"version", 1}, {"future_document", {{"retained", true}}},
        {"jobs", json::array({retained, pending, {{"id", "delete"}, {"status", "completed"}}})}};
    const auto bytes = document.dump(3);
    write(target, bytes);
    ResourceAdmission admission;
    {
        JobManager jobs(root.string(), admission);
        const auto view = jobs.get("history");
        check(view && (*view)["status"] == "interrupted" && (*view)["custom"] == retained["custom"] &&
              (*view)["steps"] == retained["steps"], "historical reads retain unknown fields without executing or changing definitions");
        const auto snapshot = jobs.snapshot();
        check(snapshot["jobs"].size() == 2 && snapshot["metadata_only"] == true &&
              snapshot["recovery"]["required"] == true && snapshot["recovery"]["pending_cleanup"] == 1 &&
              snapshot["recovery"]["quarantined"][0]["id"] == "pending" && !jobs.get("pending"),
              "prior cleanup is quarantined and visible without exposing it as an active job");
        bool rejected = false;
        try { jobs.remove("pending"); } catch (const JobError& error) { rejected = error.status == 409; }
        check(rejected && read(target) == bytes, "unresolved cleanup cannot be forgotten or rewritten by ordinary deletion");
        check(jobs.remove("delete"), "explicit history deletion commits only selected metadata");
        auto expected = document; expected["jobs"].erase(2);
        check(json::parse(read(target)) == expected, "history deletion preserves other records and unknown document fields exactly");
        check(admission.wait_for_writes(20ms), "history deletion releases its writer only after durable publication");
    }
    const auto committed = read(target);
    JobManager restarted(root.string(), admission);
    check(!restarted.get("delete") && restarted.snapshot()["recovery"]["required"] == true &&
          restarted.get("history").has_value() && read(target) == committed,
          "restart preserves quarantine and does not rewrite or resume historical graphs");
}
static void malformed_storage() {
    for (const auto& bytes : {"{", "[]", "{\"version\":2,\"jobs\":[]}", "{\"jobs\":{}}",
        "{\"jobs\":[{\"id\":\"a\"},{\"id\":\"a\"}]}", "{\"jobs\":[{\"id\":7}]}",
        "{\"jobs\":[{\"id\":\"a\",\"deleted\":\"false\"}]}", "{\"jobs\":[{\"id\":\"a\",\"status\":7}]}",
        "{\"jobs\":[null]}"}) {
        const auto root = lemon::test::make_scratch_dir("retained-malformed");
        const auto target = root / "jobs.json"; write(target, bytes);
        ResourceAdmission admission;
        JobManager jobs(root.string(), admission);
        bool rejected = false;
        try { jobs.remove("a"); } catch (const JobError& error) { rejected = error.status == 409; }
        check(rejected && jobs.snapshot()["recovery"]["required"] == true && read(target) == bytes &&
              admission.wait_for_writes(20ms), "malformed or unsupported historical storage is preserved and blocks mutation");
        JobManager restarted(root.string(), admission);
        check(restarted.snapshot()["recovery"]["required"] == true && read(target) == bytes,
              "ambiguous historical storage remains quarantined after restart");
    }
    const auto root = lemon::test::make_scratch_dir("retained-absent");
    JobManager empty(root.string(), ResourceAdmission{});
    check(empty.snapshot()["jobs"].empty() && empty.snapshot()["recovery"]["required"] == false &&
          !empty.remove("missing") && !fs::exists(root / "jobs.json"), "absent history needs neither recovery nor a startup write");
}
static void bounded_recovery() {
    const auto root = lemon::test::make_scratch_dir("retained-recovery-bound");
    json records = json::array();
    for (int i = 0; i < 64; ++i) records.push_back({{"id", std::to_string(i)}, {"deleted", true}});
    const auto bytes = json{{"jobs", records}}.dump();
    write(root / "jobs.json", bytes);
    JobManager jobs(root.string(), ResourceAdmission{});
    const auto snapshot = jobs.snapshot();
    check(snapshot["jobs"].empty() && snapshot["recovery"]["pending_cleanup"] == 64 &&
          snapshot["recovery"]["quarantined"].size() == 50 && read(root / "jobs.json") == bytes,
          "recovery observations stay bounded while preserving every quarantined record");
}
static void maintenance() {
    const auto root = lemon::test::make_scratch_dir("retained-maintenance");
    const auto target = root / "jobs.json";
    const auto bytes = json{{"jobs", json::array({{{"id", "accepted"}}, {{"id", "rejected"}}})}}.dump();
    write(target, bytes);
    ResourceAdmission admission;
    JobManager jobs(root.string(), admission);
    auto lock = lemon::jobs::JobRecordTestHook::lock(jobs);
    auto deleting = std::async(std::launch::async, [&] { return jobs.remove("accepted"); });
    const auto deadline = std::chrono::steady_clock::now() + 5s;
    while (admission.snapshot()["writes"]["active_operations"] == 0 && std::chrono::steady_clock::now() < deadline)
        std::this_thread::sleep_for(1ms);
    admission.maintenance(true);
    check(!admission.wait_for_writes(1ms), "accepted history deletion remains owned while waiting for its publication mutex");
    bool rejected = false;
    try { jobs.remove("rejected"); }
    catch (const lemon::AdmissionError& error) { rejected = error.status == 503 && error.code == "maintenance_active"; }
    check(rejected && read(target) == bytes, "fresh history deletion rejects before locking or staging during maintenance");
    lock.unlock();
    check(deleting.get() && admission.wait_for_writes(50ms) && !jobs.get("accepted") && jobs.get("rejected"),
          "accepted history publication finishes while fresh writers remain closed");
    check(jobs.snapshot()["jobs"].size() == 1, "history inspection remains available during maintenance");
}
int main() {
    history(); malformed_storage(); bounded_recovery(); maintenance();
    std::fprintf(stderr, "Retained jobs: %d checks, %d failures\n", checks, failures);
    return failures ? 1 : 0;
}
