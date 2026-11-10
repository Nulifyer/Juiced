#include "lemon/juiced/snapshot_recovery.h"
#include "lemon/utils/content_hash.h"

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <functional>
#include <map>
#include <stdexcept>
#include <string>
#include <thread>

#ifndef _WIN32
#include <sys/stat.h>
#endif

namespace {
namespace fs = std::filesystem;
using json = nlohmann::json;
using lemon::SnapshotInspectionOptions;
using lemon::SnapshotRecovery;
const std::string id(64, 'a'), profile(64, 'b');
int checks = 0, failures = 0, cases = 0, skipped = 0;

void check(bool value, const std::string& name) {
    ++checks;
    if (!value) { ++failures; std::fprintf(stderr, "FAIL: %s\n", name.c_str()); }
}
void write(const fs::path& path, const std::string& content) {
    std::ofstream output(path, std::ios::binary);
    output << content;
    if (!output) throw std::runtime_error("Cannot write test fixture");
}
json metadata(const std::string& content) {
    return {{"id", id}, {"model", "private-model-identifier"}, {"profile", profile},
        {"state", "saved"}, {"bytes", content.size()}, {"checksum", lemon::utils::string_sha256(content)}};
}
struct Fixture {
    fs::path base, root, state_file;
    json state = {{"version", 1}, {"revision", 1}};
    explicit Fixture(const fs::path& parent) {
        base = parent / std::to_string(++cases);
        root = base / "snapshots";
        state_file = base / "state.json";
        fs::create_directories(root);
    }
    void bind(bool ready = true) {
        state["snapshot_store"] = {{"version", 1}, {"id", "store-identity"},
            {"state_directory", fs::canonical(base).u8string()}, {"directory", fs::canonical(root).u8string()}, {"ready", ready}};
        auto marker = state["snapshot_store"];
        marker.erase("ready");
        write(root / ".juiced-snapshot-store.json", marker.dump());
    }
    void save() {
        state["snapshots"][id] = metadata("old");
        write(root / (id + ".bin"), "old");
    }
    void publish(int stage, bool previous = true) {
        if (previous) save();
        state["snapshots"] = json::object();
        state["snapshot_writes"][id + ".pending"] = {{"owner", "private-owner"}, {"generation", 1},
            {"bytes", 16}, {"state", "publishing"}, {"destination", id + ".bin"},
            {"backup", id + ".previous.bin"}, {"candidate", metadata("new")},
            {"previous", previous ? metadata("old") : json::object()}};
        if (stage != 2) write(root / (id + ".pending"), "new");
        if (previous && stage != 0) fs::rename(root / (id + ".bin"), root / (id + ".previous.bin"));
        if (stage == 2) write(root / (id + ".bin"), "new");
    }
    json& intent() { return state["snapshot_writes"][id + ".pending"]; }
    void pending(const std::string& phase = "writing") {
        intent() = {{"owner", "private-owner"}, {"generation", 1}, {"bytes", 16}, {"state", phase}};
    }
};
using Fingerprint = std::map<std::string, json>;
Fingerprint fingerprint(const fs::path& root) {
    Fingerprint result;
    for (const auto& entry : fs::recursive_directory_iterator(root)) {
        const auto type = entry.symlink_status();
        json row = {{"type", int(type.type())}, {"permissions", int(type.permissions())}};
        if (fs::is_symlink(type)) row["target"] = fs::read_symlink(entry.path()).u8string();
        else {
            row["modified"] = fs::last_write_time(entry.path()).time_since_epoch().count();
            if (fs::is_regular_file(type)) {
                row["bytes"] = fs::file_size(entry.path());
                row["links"] = fs::hard_link_count(entry.path());
                row["checksum"] = lemon::utils::content_sha256(entry.path());
            }
        }
        result.emplace(entry.path().lexically_relative(root).u8string(), std::move(row));
    }
    return result;
}
bool has(const json& result, const std::string& code) {
    for (const auto& row : result["issues"]) if (row["code"] == code) return true;
    return false;
}
void replacement_check(bool condition, const std::string& name, const json& detail) {
    check(condition, name);
    if (!condition) std::fprintf(stderr, "REPLACEMENT DETAIL: %s\n", detail.dump().c_str());
}
json replacement_paths(const fs::path& current, const fs::path& retained) {
    std::error_code current_error, retained_error, equivalent_error;
    const auto current_status = fs::symlink_status(current, current_error);
    const auto retained_status = fs::symlink_status(retained, retained_error);
    const auto equivalent = fs::equivalent(current, retained, equivalent_error);
    return {{"current_type", int(current_status.type())}, {"current_error", current_error.value()},
        {"retained_type", int(retained_status.type())}, {"retained_error", retained_error.value()},
        {"equivalent", equivalent}, {"equivalent_error", equivalent_error.value()}};
}
using Configure = std::function<void(Fixture&)>;
using Verify = std::function<void(const json&)>;
void run_case(const fs::path& parent, const std::string& name, const Configure& configure = {},
              const std::string& expected_issue = {}, const Verify& verify = {},
              const SnapshotInspectionOptions& options = {}) {
    Fixture fixture(parent);
    fixture.bind();
    if (configure) configure(fixture);
    if (!fs::exists(fixture.state_file) && !fs::is_symlink(fs::symlink_status(fixture.state_file)))
        write(fixture.state_file, fixture.state.dump());
    const auto before = fingerprint(fixture.base);
    const auto original_state = fixture.state;
    const auto result = SnapshotRecovery::inspect_file(fixture.state_file, fixture.root, options);
    check(result["read_only"] == true && result["application_authorized"] == false &&
        result["writer_termination_verified"] == false, name + " never authorizes effects or termination");
    check(result["validation_complete"] == expected_issue.empty(), name + " completion status");
    if (!expected_issue.empty()) check(has(result, expected_issue), name + " expected issue " + expected_issue);
    check(fingerprint(fixture.base) == before && fixture.state == original_state, name + " leaves source unchanged");
    const auto serialized = result.dump();
    check(serialized.find("private-model-identifier") == std::string::npos && serialized.find("private-owner") == std::string::npos &&
        serialized.find(fixture.base.u8string()) == std::string::npos, name + " redacts input values and paths");
    for (const auto& row : result["issues"]) {
        check(row.is_object() && (row.size() == 1 || row.size() == 2) && row["code"].is_string(), name + " fixed issue shape");
        if (row.contains("record_digest")) check(lemon::snapshot_metadata::valid_id(row["record_digest"]), name + " hashed record identity");
    }
    if (verify) verify(result);
}
}

int main() {
    const auto parent = fs::temp_directory_path() /
        ("juiced-snapshot-inspection-" + std::to_string(std::chrono::steady_clock::now().time_since_epoch().count()));
    struct Cleanup {
        fs::path path;
        ~Cleanup() { std::error_code error; fs::remove_all(path, error); }
    } cleanup{parent};
    try {
        run_case(parent, "empty bound inventory");
        run_case(parent, "empty legacy state", [](auto& f) { f.state.erase("snapshot_store"); fs::remove(f.root / ".juiced-snapshot-store.json"); });
        run_case(parent, "saved snapshot", [](auto& f) { f.save(); }, {}, [](const auto& r) { check(r["saved_snapshots"] == 1, "saved count"); });
        run_case(parent, "empty snapshot content", [](auto& f) { f.state["snapshots"][id] = metadata(""); write(f.root / (id + ".bin"), ""); });
        run_case(parent, "unknown fields preserved", [](auto& f) { f.save(); f.state["unknown"] = {{"secret", "private-owner"}}; f.state["snapshots"][id]["future"] = json::array({1, 2}); });
        run_case(parent, "missing binding with records", [](auto& f) { f.save(); f.state.erase("snapshot_store"); fs::remove(f.root / ".juiced-snapshot-store.json"); }, "snapshot_binding_missing");
        run_case(parent, "saved file missing", [](auto& f) { f.save(); fs::remove(f.root / (id + ".bin")); }, "snapshot_file_missing");
        run_case(parent, "saved checksum mismatch", [](auto& f) { f.save(); write(f.root / (id + ".bin"), "bad"); }, "snapshot_checksum_mismatch");
        run_case(parent, "saved size mismatch", [](auto& f) { f.save(); write(f.root / (id + ".bin"), "larger"); }, "snapshot_size_mismatch");
        run_case(parent, "saved malformed retention", [](auto& f) { f.save(); f.state["snapshots"][id]["retained"] = "true"; }, "saved_snapshot_schema_invalid");
        run_case(parent, "saved malformed profile", [](auto& f) { f.save(); f.state["snapshots"][id]["profile"] = "../outside"; }, "saved_snapshot_schema_invalid");
        run_case(parent, "saved negative size", [](auto& f) { f.save(); f.state["snapshots"][id]["bytes"] = -1; }, "saved_snapshot_schema_invalid");
        run_case(parent, "saved fractional size", [](auto& f) { f.save(); f.state["snapshots"][id]["bytes"] = 3.0; }, "saved_snapshot_schema_invalid");
        run_case(parent, "saved key traversal", [](auto& f) { f.state["snapshots"]["../outside"] = metadata("old"); }, "saved_snapshot_schema_invalid");
        run_case(parent, "invalid inventory", [](auto& f) { f.state["snapshots"] = json::array(); }, "inventory_schema_invalid");
        run_case(parent, "invalid state version", [](auto& f) { f.state["version"] = 2; }, "state_schema_unknown");
        run_case(parent, "floating state version", [](auto& f) { f.state["version"] = 1.0; }, "state_schema_unknown");
        run_case(parent, "negative revision", [](auto& f) { f.state["revision"] = -1; }, "state_schema_unknown");
        run_case(parent, "malformed JSON", [](auto& f) { write(f.state_file, "{"); }, "state_json_invalid");
        run_case(parent, "duplicate keys", [](auto& f) { write(f.state_file, "{\"version\":1,\"version\":1}"); }, "state_structure_invalid");
        run_case(parent, "duplicate nested keys", [](auto& f) { write(f.state_file, "{\"version\":1,\"x\":{\"a\":0,\"a\":1}}"); }, "state_structure_invalid");
        run_case(parent, "escaped duplicate keys", [](auto& f) { write(f.state_file, "{\"version\":1,\"x\":{\"a\":0,\"\\u0061\":1}}"); }, "state_structure_invalid");
        run_case(parent, "independent sibling keys", [](auto& f) { f.state["x"] = {{"a", 1}}; f.state["y"] = {{"a", 2}}; });
        run_case(parent, "duplicate keys inside array", [](auto& f) { write(f.state_file, "{\"version\":1,\"x\":[{\"a\":0,\"a\":1}]}"); }, "state_structure_invalid");
        run_case(parent, "trailing JSON data", [](auto& f) { write(f.state_file, "{\"version\":1} {}"); }, "state_json_invalid");
        run_case(parent, "overflow JSON number", [](auto& f) { write(f.state_file, "{\"version\":1,\"x\":1e10000}"); }, "state_json_invalid");
        run_case(parent, "deep JSON", [](auto& f) { write(f.state_file, "{\"version\":1,\"x\":" + std::string(70, '[') + "0" + std::string(70, ']') + "}"); }, "state_structure_invalid");
        run_case(parent, "JSON event budget", [](auto& f) { std::string raw = "{\"version\":1,\"x\":[0"; for (int i = 0; i < 262144; ++i) raw += ",0"; write(f.state_file, raw + "]}"); }, "state_structure_invalid");
        run_case(parent, "JSON byte budget", [](auto& f) { write(f.state_file, std::string(16 * 1024 * 1024 + 1, ' ')); }, "state_size_excessive");
        run_case(parent, "unfinished binding", [](auto& f) { f.bind(false); });
        run_case(parent, "unfinished missing marker", [](auto& f) { f.bind(false); fs::remove(f.root / ".juiced-snapshot-store.json"); });
        run_case(parent, "unfinished binding records", [](auto& f) { f.bind(false); f.save(); }, "unfinished_binding_has_snapshot_records");
        run_case(parent, "committed missing marker", [](auto& f) { fs::remove(f.root / ".juiced-snapshot-store.json"); }, "committed_snapshot_marker_missing");
        run_case(parent, "marker mismatch", [](auto& f) { write(f.root / ".juiced-snapshot-store.json", "{}"); }, "snapshot_marker_mismatch");
        run_case(parent, "marker duplicate keys", [](auto& f) { write(f.root / ".juiced-snapshot-store.json", "{\"x\":1,\"x\":1}"); }, "snapshot_marker_structure_invalid");
        run_case(parent, "marker byte budget", [](auto& f) { write(f.root / ".juiced-snapshot-store.json", std::string(4097, ' ')); }, "snapshot_marker_size_invalid");
        run_case(parent, "marker without binding", [](auto& f) { f.state.erase("snapshot_store"); }, "snapshot_marker_without_state_binding");
        run_case(parent, "wrong state directory", [](auto& f) { f.state["snapshot_store"]["state_directory"] = "/other"; }, "state_directory_binding_mismatch");
        run_case(parent, "wrong root directory", [](auto& f) { f.state["snapshot_store"]["directory"] = "/other"; }, "snapshot_root_binding_mismatch");
        run_case(parent, "unknown binding schema", [](auto& f) { f.state["snapshot_store"]["future"] = 1; }, "snapshot_binding_schema_invalid");
        run_case(parent, "unknown file", [](auto& f) { write(f.root / "private-owner", "keep"); }, "unexplained_snapshot_file");
        run_case(parent, "unknown directory", [](auto& f) { fs::create_directory(f.root / "private-owner"); }, "unexplained_snapshot_file");
        run_case(parent, "control directory", [](auto& f) { fs::create_directory(f.root / ".juiced-store.lock"); }, "control_file_identity_unverified");
        run_case(parent, "existing lock untouched", [](auto& f) { write(f.root / ".juiced-store.lock", "retained-control-content"); });
        run_case(parent, "cleanup present", [](auto& f) { f.state["snapshot_cleanup"][id] = metadata("old"); write(f.root / (id + ".bin"), "old"); });
        run_case(parent, "cleanup absent", [](auto& f) { f.state["snapshot_cleanup"][id + ".previous"] = metadata("old"); });
        run_case(parent, "cleanup corruption", [](auto& f) { f.state["snapshot_cleanup"][id] = metadata("old"); write(f.root / (id + ".bin"), "bad"); }, "snapshot_checksum_mismatch");
        run_case(parent, "cleanup incomplete metadata", [](auto& f) { f.state["snapshot_cleanup"][id] = {{"bytes", 0}, {"checksum", id}}; }, "cleanup_metadata_invalid");
        run_case(parent, "cleanup conflicts", [](auto& f) { f.save(); f.state["snapshot_cleanup"][id] = metadata("old"); }, "cleanup_conflicts_with_saved_snapshot");
        for (int stage = 0; stage != 3; ++stage) {
            run_case(parent, "publication stage " + std::to_string(stage), [stage](auto& f) { f.publish(stage); }, {}, [](const auto& r) { check(r["unverified_writers"] == 1 && r["pending_effects"] == 1, "publication remains unverified"); });
            if (stage != 1) run_case(parent, "first publication stage " + std::to_string(stage), [stage](auto& f) { f.publish(stage, false); });
        }
        run_case(parent, "publication source corruption", [](auto& f) { f.publish(0); write(f.root / (id + ".pending"), "bad"); }, "publication_files_do_not_match_intent");
        run_case(parent, "publication backup corruption", [](auto& f) { f.publish(2); write(f.root / (id + ".previous.bin"), "bad"); }, "publication_files_do_not_match_intent");
        run_case(parent, "publication destination directory", [](auto& f) { f.publish(0, false); fs::create_directory(f.root / (id + ".bin")); }, "publication_files_do_not_match_intent");
        run_case(parent, "publication unexpected backup", [](auto& f) { f.publish(0, false); write(f.root / (id + ".previous.bin"), "old"); }, "publication_files_do_not_match_intent");
        run_case(parent, "publication missing source", [](auto& f) { f.publish(0); fs::remove(f.root / (id + ".pending")); }, "publication_files_do_not_match_intent");
        run_case(parent, "publication destination traversal", [](auto& f) { f.publish(0); f.intent()["destination"] = "../outside"; }, "write_intent_schema_invalid");
        run_case(parent, "publication invalid previous", [](auto& f) { f.publish(0); f.intent()["previous"]["id"] = std::string(64, 'c'); }, "write_intent_schema_invalid");
        run_case(parent, "publication candidate exceeds reservation", [](auto& f) { f.publish(0); f.intent()["bytes"] = 1; }, "write_intent_schema_invalid");
        run_case(parent, "publication conflicting files override", [](auto& f) { f.publish(0); f.intent()["files"] = json::object(); }, "write_intent_schema_invalid");
        run_case(parent, "publication conflicts with saved", [](auto& f) { f.publish(0); f.state["snapshots"][id] = metadata("old"); }, "publication_conflicts_with_saved_inventory");
        run_case(parent, "publication conflicts with cleanup", [](auto& f) { f.publish(0); f.state["snapshot_cleanup"][id + ".previous"] = metadata("old"); }, "publication_conflicts_with_cleanup");
        for (const std::string phase : {"writing", "uncertain", "cleanup"})
            run_case(parent, "partial writer " + phase, [phase](auto& f) { f.pending(phase); write(f.root / (id + ".pending"), "partial"); }, {}, [](const auto& r) { check(r["unverified_writers"] == 1, "partial writer remains unverified"); });
        run_case(parent, "writer file directory", [](auto& f) { f.pending(); fs::create_directory(f.root / (id + ".pending")); }, "snapshot_file_identity_unverified");
        run_case(parent, "writer unknown phase", [](auto& f) { f.pending("future"); }, "write_intent_schema_invalid", [](const auto& r) { check(r["unverified_writers"] == 1, "unknown writer counted"); });
        run_case(parent, "writer zero generation", [](auto& f) { f.pending(); f.intent()["generation"] = 0; }, "write_intent_schema_invalid");
        run_case(parent, "writer negative generation", [](auto& f) { f.pending(); f.intent()["generation"] = -1; }, "write_intent_schema_invalid");
        run_case(parent, "writer files traversal", [](auto& f) { f.pending("cleanup"); f.intent()["files"] = {{"../outside", 2}}; }, "write_intent_schema_invalid");
        run_case(parent, "writer files wrong shape", [](auto& f) { f.pending("cleanup"); f.intent()["files"] = json::array(); }, "write_intent_schema_invalid");
        run_case(parent, "writer cleanup remaining files", [](auto& f) { f.pending("cleanup"); f.intent()["files"] = {{id + ".previous.bin", 16}}; write(f.root / (id + ".previous.bin"), "partial"); });
        run_case(parent, "writer omitted pending is unexplained", [](auto& f) { f.pending("cleanup"); f.intent()["files"] = json::object(); write(f.root / (id + ".pending"), "partial"); }, "unexplained_snapshot_file");
        run_case(parent, "writer files inventory conflict", [](auto& f) { f.save(); f.pending("cleanup"); f.intent()["files"] = {{id + ".bin", 16}}; }, "write_files_conflict_with_inventory");
        run_case(parent, "writer files cleanup conflict", [](auto& f) { f.state["snapshot_cleanup"][id] = metadata("old"); f.pending("cleanup"); f.intent()["files"] = {{id + ".bin", 16}}; }, "write_files_conflict_with_inventory");
        run_case(parent, "pending voice", [](auto& f) { f.state["voice_writer"] = {{"owner", "private-owner"}, {"pending", true}, {"state", "dispatching"}}; }, {}, [](const auto& r) { check(r["unverified_writers"] == 1, "voice stays unverified"); });
        run_case(parent, "stopped voice", [](auto& f) { f.state["voice_writer"] = {{"owner", "private-owner"}, {"pending", false}, {"state", "stopped"}}; });
        run_case(parent, "malformed voice", [](auto& f) { f.state["voice_writer"] = {{"pending", "false"}}; }, "voice_intent_schema_invalid", [](const auto& r) { check(r["unverified_writers"] == 1, "malformed voice stays unknown"); });
        run_case(parent, "queued preparation", [](auto& f) { f.state["preparations"]["private-owner"] = {{"model", "private-model-identifier"}, {"pending", true}, {"writer_state", "queued"}}; });
        run_case(parent, "dispatching preparation", [](auto& f) { f.state["preparations"]["private-owner"] = {{"model", "private-model-identifier"}, {"pending", true}, {"writer_state", "dispatching"}}; }, {}, [](const auto& r) { check(r["unverified_writers"] == 1, "dispatching stays unverified"); });
        run_case(parent, "legacy preparation", [](auto& f) { f.state["preparations"]["private-owner"] = {{"model", "private-model-identifier"}, {"pending", false}, {"phase", "complete"}}; }, "preparation_writer_unknown", [](const auto& r) { check(r["unverified_writers"] == 1, "legacy unknown writer counted"); });
        run_case(parent, "unknown preparation state", [](auto& f) { f.state["preparations"]["private-owner"] = {{"model", "private-model-identifier"}, {"pending", true}, {"writer_state", "future"}}; }, "preparation_writer_unknown");
        run_case(parent, "stopped preparation", [](auto& f) { f.state["preparations"]["private-owner"] = {{"model", "private-model-identifier"}, {"pending", false}, {"writer_state", "stopped"}, {"phase", "complete"}}; });
        SnapshotInspectionOptions options;
        options.maximum_hash_bytes = 2;
        run_case(parent, "hash byte budget", [](auto& f) { f.save(); }, "hash_budget_exhausted", {}, options);
        options = {}; options.maximum_records = 1;
        run_case(parent, "record budget", [](auto& f) { f.save(); f.state["snapshot_cleanup"][id + ".previous"] = metadata("old"); }, "record_budget_exhausted", {}, options);
        options = {}; options.maximum_files = 1;
        run_case(parent, "file scan budget", [](auto& f) { f.save(); }, "file_budget_exhausted", {}, options);
        options = {}; options.maximum_issues = 1;
        run_case(parent, "issue report budget", [](auto& f) { write(f.root / "one", "1"); write(f.root / "two", "2"); }, "unexplained_snapshot_file", [](const auto& r) { check(r["issues"].size() == 1 && r["issues_truncated"] == true, "bounded issue output"); }, options);
        options = {}; options.cancelled = [] { return true; };
        run_case(parent, "caller cancellation", {}, "inspection_cancelled", {}, options);
        options = {}; int calls = 0; options.cancelled = [&] { return ++calls >= 40; };
        run_case(parent, "parser cancellation", [](auto& f) { const std::string data(4 * 1024 * 1024, 'z'); f.state["snapshots"][id] = metadata(data); write(f.root / (id + ".bin"), data); }, "inspection_cancelled", {}, options);
        options = {}; options.timeout = std::chrono::milliseconds(1);
        options.cancelled = [] { std::this_thread::sleep_for(std::chrono::milliseconds(2)); return false; };
        run_case(parent, "cooperative deadline", {}, "inspection_deadline", {}, options);
        {
            Fixture f(parent);
            f.bind();
            const std::string data(4 * 1024 * 1024, 'z');
            f.state["snapshots"][id] = metadata(data);
            write(f.root / (id + ".bin"), data);
            write(f.state_file, f.state.dump());
            const auto before = fingerprint(f.base);
            size_t before_hash = 0;
            options = {}; options.maximum_hash_bytes = 1;
            options.cancelled = [&] { ++before_hash; return false; };
            check(has(SnapshotRecovery::inspect_file(f.state_file, f.root, options), "hash_budget_exhausted"), "calibrate first hash callback");
            size_t callbacks = 0;
            options = {}; options.cancelled = [&] { return ++callbacks > before_hash + 2; };
            const auto cancelled = SnapshotRecovery::inspect_file(f.state_file, f.root, options);
            check(has(cancelled, "inspection_cancelled") && callbacks > before_hash + 2, "cancellation during chunked content hash");
            check(cancelled["application_authorized"] == false && cancelled["writer_termination_verified"] == false && fingerprint(f.base) == before,
                "hash cancellation leaves source unchanged and unauthorized");
            callbacks = 0;
            options = {}; options.cancelled = [&] {
                if (++callbacks == before_hash + 1) {
                    auto changed = f.state;
                    changed["revision"] = 2;
                    write(f.state_file, changed.dump());
                }
                return false;
            };
            const auto changed = SnapshotRecovery::inspect_file(f.state_file, f.root, options);
            check(has(changed, "state_changed_during_inspection") && changed["validation_complete"] == false,
                "concurrent state replacement is reported without repair");
            check(lemon::utils::content_sha256(f.root / (id + ".bin")) == metadata(data)["checksum"],
                "observing a changed state preserves snapshot data");
            write(f.state_file, f.state.dump());
            callbacks = 0;
            options = {}; options.cancelled = [&] {
                if (++callbacks == before_hash + 1) write(f.root / ".juiced-snapshot-store.json", "{}");
                return false;
            };
            check(has(SnapshotRecovery::inspect_file(f.state_file, f.root, options), "snapshot_marker_changed_during_inspection"),
                "marker mutation after initial validation is detected");
            f.bind();
            callbacks = 0;
            options = {}; options.cancelled = [&] {
                if (++callbacks == before_hash + 1) write(f.root / (id + ".bin"), "replacement");
                return false;
            };
            check(has(SnapshotRecovery::inspect_file(f.state_file, f.root, options), "snapshot_changed_during_inspection"),
                "snapshot mutation during checksum is detected");
        }
        for (const int invalid : {0, 1, 2, 3, 4}) {
            options = {};
            if (invalid == 0) options.timeout = std::chrono::milliseconds(0);
            if (invalid == 1) options.timeout = std::chrono::minutes(6);
            if (invalid == 2) options.maximum_hash_bytes = 0;
            if (invalid == 3) options.maximum_records = 16385;
            if (invalid == 4) options.maximum_issues = 0;
            bool rejected = false;
            try { SnapshotRecovery::inspect_file(parent / "missing", parent / "missing", options); }
            catch (const std::invalid_argument&) { rejected = true; }
            check(rejected, "invalid inspection options rejected");
        }
        {
            Fixture f(parent);
            f.bind(); write(f.state_file, f.state.dump());
            const auto result = SnapshotRecovery::inspect_file(f.state_file, f.base / "missing");
            check(has(result, "snapshot_root_unavailable") && !fs::exists(f.base / "missing"), "missing root remains absent");
            const auto absent = f.base / "missing-state.json";
            check(has(SnapshotRecovery::inspect_file(absent, f.root), "state_file_identity_unverified") && !fs::exists(absent), "missing state remains absent");
        }
        {
            Fixture f(parent);
            f.bind(); f.save(); write(f.state_file, f.state.dump());
            check(has(SnapshotRecovery::inspect_file({}, f.root), "state_file_identity_unverified"), "empty state never selects CWD");
            check(has(SnapshotRecovery::inspect_file(f.state_file, {}), "snapshot_root_unavailable"), "empty root never selects CWD");
        }
        const auto replacement_case = [&](const std::string& name, bool after_open, const std::function<void(Fixture&, const fs::path&)>& replace) {
            Fixture f(parent);
            f.bind(); f.save(); write(f.state_file, f.state.dump());
            const auto path = f.root / (id + ".bin");
            const auto state_hash = lemon::utils::content_sha256(f.state_file);
            const auto marker_hash = lemon::utils::content_sha256(f.root / ".juiced-snapshot-store.json");
            size_t before_hash = 0;
            SnapshotInspectionOptions probe;
            probe.maximum_hash_bytes = 1;
            probe.cancelled = [&] { ++before_hash; return false; };
            check(has(SnapshotRecovery::inspect_file(f.state_file, f.root, probe), "hash_budget_exhausted"), name + " calibrates snapshot input seam");
            size_t callbacks = 0;
            bool invoked = false, denied = false;
            std::error_code rename_error;
            const auto target_callback = after_open ? before_hash + 1 : before_hash - 1;
            SnapshotInspectionOptions options;
            options.cancelled = [&] {
                if (++callbacks == target_callback) {
                    invoked = true;
                    fs::rename(path, f.base / "retained-original", rename_error);
                    if (rename_error) denied = true;
                    else replace(f, path);
                }
                return false;
            };
            const auto started = std::chrono::steady_clock::now();
            const auto result = SnapshotRecovery::inspect_file(f.state_file, f.root, options);
            const json detail = {{"fixture", name}, {"after_open", after_open}, {"probe_callbacks", before_hash},
                {"target_callback", target_callback}, {"observed_callbacks", callbacks}, {"invoked", invoked},
                {"rename_denied", denied}, {"rename_error", rename_error.value()}, {"rename_category", rename_error.category().name()},
                {"paths", replacement_paths(path, f.base / "retained-original")}, {"result", result}};
            replacement_check(invoked, name + " replacement seam invoked", detail);
#ifdef _WIN32
            if (after_open && denied) {
                replacement_check(result["validation_complete"] == true && fs::is_regular_file(path), name + " retained read sharing denies replacement", detail);
            } else
#endif
            {
                replacement_check(!denied && result["validation_complete"] == false && has(result, "snapshot_changed_during_inspection"), name + " rejects replacement", detail);
            }
            check(std::chrono::steady_clock::now() - started < std::chrono::seconds(2), name + " avoids a blocking replacement open");
            check(result["application_authorized"] == false && result["writer_termination_verified"] == false, name + " remains unauthorized");
            check(lemon::utils::content_sha256(f.state_file) == state_hash &&
                lemon::utils::content_sha256(f.root / ".juiced-snapshot-store.json") == marker_hash, name + " preserves source metadata");
        };
        replacement_case("readable file replacement before open", false, [](auto&, const auto& path) { write(path, "new"); });
        replacement_case("same size and timestamp new inode before open", false, [](auto& f, const auto& path) {
            write(path, "old");
            fs::last_write_time(path, fs::last_write_time(f.base / "retained-original"));
        });
        replacement_case("readable file replacement after open", true, [](auto&, const auto& path) { write(path, "new"); });
#ifndef _WIN32
        replacement_case("FIFO replacement before open", false, [](auto&, const auto& path) {
            if (::mkfifo(path.c_str(), 0600) != 0) throw std::runtime_error("Cannot create replacement FIFO");
        });
        replacement_case("FIFO replacement after open", true, [](auto&, const auto& path) {
            if (::mkfifo(path.c_str(), 0600) != 0) throw std::runtime_error("Cannot create replacement FIFO");
        });
#endif
        const auto state_replacement_case = [&](const std::string& name, const std::function<void(Fixture&)>& replace) {
            Fixture f(parent);
            f.bind(); f.save();
            write(f.state_file, std::string(16 * 1024 * 1024 + 1, ' '));
            size_t before_open = 0;
            SnapshotInspectionOptions probe;
            probe.cancelled = [&] { ++before_open; return false; };
            check(has(SnapshotRecovery::inspect_file(f.state_file, f.root, probe), "state_size_excessive"), name + " calibrates state input seam");
            write(f.state_file, f.state.dump());
            size_t callbacks = 0;
            bool invoked = false;
            SnapshotInspectionOptions options;
            options.cancelled = [&] {
                if (++callbacks == before_open) {
                    invoked = true;
                    std::error_code error;
                    fs::rename(f.state_file, f.base / "retained-state", error);
                    if (error) {
                        const json detail = {{"fixture", name}, {"probe_callbacks", before_open}, {"target_callback", before_open},
                            {"observed_callbacks", callbacks}, {"invoked", invoked}, {"rename_error", error.value()},
                            {"rename_category", error.category().name()}, {"paths", replacement_paths(f.state_file, f.base / "retained-state")}};
                        std::fprintf(stderr, "REPLACEMENT DETAIL: %s\n", detail.dump().c_str());
                        throw fs::filesystem_error("State replacement fixture rename failed", error);
                    }
                    replace(f);
                }
                return false;
            };
            const auto started = std::chrono::steady_clock::now();
            const auto result = SnapshotRecovery::inspect_file(f.state_file, f.root, options);
            const json detail = {{"fixture", name}, {"probe_callbacks", before_open}, {"target_callback", before_open},
                {"observed_callbacks", callbacks}, {"invoked", invoked},
                {"paths", replacement_paths(f.state_file, f.base / "retained-state")}, {"result", result}};
            replacement_check(invoked && result["validation_complete"] == false && has(result, "state_changed_during_inspection"), name + " rejects state substitution", detail);
            check(std::chrono::steady_clock::now() - started < std::chrono::seconds(2), name + " avoids blocked state open");
            check(result["application_authorized"] == false && result["writer_termination_verified"] == false, name + " never authorizes recovery");
        };
        state_replacement_case("state replacement with same bytes and timestamp", [](auto& f) {
            write(f.state_file, f.state.dump());
            fs::last_write_time(f.state_file, fs::last_write_time(f.base / "retained-state"));
        });
#ifndef _WIN32
        state_replacement_case("state FIFO replacement", [](auto& f) {
            if (::mkfifo(f.state_file.c_str(), 0600) != 0) throw std::runtime_error("Cannot create state FIFO");
        });
        {
            Fixture f(parent);
            f.bind(); f.save(); write(f.state_file, f.state.dump());
            size_t before_hash = 0;
            SnapshotInspectionOptions probe;
            probe.maximum_hash_bytes = 1;
            probe.cancelled = [&] { ++before_hash; return false; };
            check(has(SnapshotRecovery::inspect_file(f.state_file, f.root, probe), "hash_budget_exhausted"), "calibrate directory replacement seam");
            size_t callbacks = 0;
            bool replaced = false;
            SnapshotInspectionOptions options;
            options.cancelled = [&] {
                if (++callbacks == before_hash - 1) {
                    replaced = true;
                    fs::rename(f.root, f.base / "retained-directory");
                    fs::create_directory(f.root);
                    if (::mkfifo((f.root / (id + ".bin")).c_str(), 0600) != 0) throw std::runtime_error("Cannot create substituted root FIFO");
                }
                return false;
            };
            const auto started = std::chrono::steady_clock::now();
            const auto result = SnapshotRecovery::inspect_file(f.state_file, f.root, options);
            check(replaced && has(result, "snapshot_root_changed_during_inspection") && result["validation_complete"] == false,
                "retained ancestry detects directory inode replacement");
            check(std::chrono::steady_clock::now() - started < std::chrono::seconds(2), "anchored lookup never opens replacement directory FIFO");
            check(lemon::utils::content_sha256(f.base / "retained-directory" / (id + ".bin")) == metadata("old")["checksum"],
                "directory substitution preserves retained snapshot data");
        }
#endif
        std::error_code error;
        {
            Fixture f(parent); write(f.base / "target", "data");
            fs::create_symlink(f.base / "target", f.base / "probe", error);
            if (error) { ++skipped; std::fprintf(stderr, "SKIP: symlink creation unavailable\n"); }
            else {
                state_replacement_case("state symlink replacement", [](auto& f) {
                    fs::create_symlink(f.base / "retained-state", f.state_file);
                });
                run_case(parent, "configured snapshot root symlink", [](auto& f) {
                    const auto original = f.root;
                    f.root = f.base / "snapshot-alias";
                    fs::create_directory_symlink(original, f.root);
                });
                replacement_case("symlink replacement before open", false, [](auto& f, const auto& path) {
                    write(f.base / "outside", "private-owner"); fs::create_symlink(f.base / "outside", path);
                });
                replacement_case("symlink replacement after open", true, [](auto& f, const auto& path) {
                    write(f.base / "outside", "private-owner"); fs::create_symlink(f.base / "outside", path);
                });
                run_case(parent, "saved symlink", [](auto& s) { s.save(); fs::remove(s.root / (id + ".bin")); write(s.base / "outside", "old"); fs::create_symlink(s.base / "outside", s.root / (id + ".bin")); }, "snapshot_file_identity_unverified");
                run_case(parent, "state symlink", [](auto& s) { write(s.base / "real-state", s.state.dump()); fs::create_symlink(s.base / "real-state", s.state_file); }, "state_file_identity_unverified");
                run_case(parent, "marker symlink", [](auto& s) { fs::rename(s.root / ".juiced-snapshot-store.json", s.base / "real-marker"); fs::create_symlink(s.base / "real-marker", s.root / ".juiced-snapshot-store.json"); }, "snapshot_marker_identity_unverified");
                run_case(parent, "dangling symlink", [](auto& s) { s.save(); fs::remove(s.root / (id + ".bin")); fs::create_symlink(s.base / "absent", s.root / (id + ".bin")); }, "snapshot_file_identity_unverified");
            }
        }
        error.clear();
        {
            Fixture f(parent); write(f.base / "target", "data");
            fs::create_hard_link(f.base / "target", f.base / "probe", error);
            if (error) { ++skipped; std::fprintf(stderr, "SKIP: hardlink creation unavailable\n"); }
            else {
                run_case(parent, "saved hardlink", [](auto& s) { s.save(); fs::create_hard_link(s.root / (id + ".bin"), s.base / "other"); }, "snapshot_file_identity_unverified");
                run_case(parent, "state hardlink", [](auto& s) { write(s.state_file, s.state.dump()); fs::create_hard_link(s.state_file, s.base / "other"); }, "state_file_identity_unverified");
                run_case(parent, "marker hardlink", [](auto& s) { fs::create_hard_link(s.root / ".juiced-snapshot-store.json", s.base / "other"); }, "snapshot_marker_identity_unverified");
            }
        }
    } catch (const std::exception& error) {
        ++failures; std::fprintf(stderr, "Unexpected test failure: %s\n", error.what());
    }
    std::fprintf(stderr, "%s: %d checks, %d fixtures, %d failures, %d capability skips\n", failures ? "FAIL" : "PASS", checks, cases, failures, skipped);
    return failures ? 1 : 0;
}
