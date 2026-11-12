import { describe, expect, test } from "bun:test";
import { downloadRequest, firstInventoryPages, inventoryPath, jobActions, nextInventoryPage, parseArtifactInventory, previousInventoryPage, parseCacheView, parseCacheInspection, parseEndpoints, parseProtocols, positiveBytes, registration, removalReason, sessionValue, type ArtifactJob } from "../src/management";
import type { Model } from "../src/domain";

const hash = "a".repeat(64);
const job: ArtifactJob = { id: "a".repeat(32), revision: 3, state: "downloading", source_url: "https://example.com/file.gguf", expected_sha256: hash, expected_bytes: 16, maximum_bytes: 32, downloaded_bytes: 8, artifact: null, error: null, writer_active: true };

describe("bounded acquisition and explicit registration", () => {
  test("unknown size still reserves a positive exact ceiling", () => {
    expect(downloadRequest("https://example.com/file.gguf", hash.toUpperCase(), "", "32")).toEqual({ source_url: "https://example.com/file.gguf", expected_sha256: hash, expected_bytes: null, maximum_bytes: 32 });
    for (const value of ["", "0", "-1", "1.2", "2e6", "9007199254740992"]) expect(() => positiveBytes(value, "Ceiling")).toThrow();
    expect(() => downloadRequest("https://example.com/file", hash, "33", "32")).toThrow("exceeds");
  });
  test("direct credentials, signed queries, fragments and malformed hashes fail before sending", () => {
    for (const url of ["https://u:p@example.com/file", "https://example.com/file?token=private", "https://example.com/file#secret", "file:///private/weights"]) expect(() => downloadRequest(url, hash, "16", "32")).toThrow();
    expect(() => downloadRequest("https://example.com/file", "not-a-checksum", "16", "32")).toThrow("SHA256");
  });
  test("managed registration binds artifact and never starts an engine implicitly", () => {
    const result = registration(hash, " model.one ", "/opt/llama-server", '["--port","{port}","--model","{model}"]', "1024", "1", ["completions"]);
    expect(result).toMatchObject({ id: "model.one", artifact: hash, model_file: "managed", resident_bytes: 1024, max_inflight: 1, policy: "disabled", revision: 0 });
    expect(result.execution_profile).toEqual({ schema_version: 1, adapter: "external-http", implementation_id: null, command: "/opt/llama-server", args: ["--port", "{port}", "--model", "{model}"], options: {}, placement: "unresolved" });
    for (const legacyField of ["provider", "command", "args", "options"]) expect(Object.hasOwn(result, legacyField)).toBe(false);
    for (const args of ['"--port {port}"', '["--port",7]', '["--port","{port}"]', "[invalid]"]) expect(() => registration(hash, "one", "engine", args, "1024", "1", ["completions"])).toThrow();
    expect(() => registration("../../other", "one", "engine", '["{port}","{model}"]', "1024", "1", ["completions"])).toThrow();
  });
  test("selected engine identity and literal arguments do not imply device placement", () => {
    const args = ["--port", "{port}", "--model", "{model}", "--n-gpu-layers", "99", "literal $HOME; no shell", "line one\nline two"];
    const options = { context_tokens: 8192, threads: null, gpu_layers: 0, parallel_slots: 2 };
    const result = registration(hash, "voice.gguf", " /opt/llama-server ", JSON.stringify(args), "2048", "2", ["completions"], "llama-cpp", options);
    const wire = JSON.parse(JSON.stringify(result));
    expect(wire.execution_profile).toEqual({ schema_version: 1, adapter: "llama-cpp", implementation_id: "llama-cpp", command: "/opt/llama-server", args, options, placement: "unresolved" });
    expect(wire).toMatchObject({ artifact: hash, max_inflight: 2, operations: ["completions"], policy: "disabled" });
    for (const field of ["provider", "command", "args", "options", "placement", "implementation_id", "qualified", "supported"]) expect(Object.hasOwn(wire, field)).toBe(false);
    expect(Object.hasOwn(wire.execution_profile, "qualified")).toBe(false);
    expect(Object.hasOwn(wire.execution_profile, "supported")).toBe(false);
  });
});

describe("settled writer and ownership guards", () => {
  test("pause and cancellation only target states accepted by the owner", () => {
    expect(jobActions(job)).toEqual(["pause", "cancel"]);
    expect(jobActions({ ...job, state: "paused", writer_active: false })).toEqual(["resume", "cancel"]);
    expect(jobActions({ ...job, state: "pausing" })).toEqual(["cancel"]);
    expect(jobActions({ ...job, state: "cancelling" })).toEqual([]);
    expect(jobActions({ ...job, state: "complete", writer_active: false })).toEqual(["forget"]);
    expect(jobActions({ ...job, state: "cancelled", writer_active: false })).toEqual(["forget"]);
    expect(jobActions({ ...job, state: "complete", writer_active: true })).toEqual([]);
    expect(jobActions({ ...job, state: "quarantined", writer_active: false })).toEqual(["retry", "cancel"]);
    expect(jobActions({ ...job, state: "interrupted", writer_active: false })).toEqual(["resume", "retry", "cancel"]);
    expect(jobActions({ ...job, state: "failed", writer_active: false })).toEqual(["retry", "cancel"]);
    expect(jobActions({ ...job, state: "interrupted", error: "artifact_publication_uncertain", writer_active: false })).toEqual([]);
  });
  test("registration removal requires Disabled and unloaded with no active requests", () => {
    const model = { policy: "disabled", loaded: false, state: "unloaded", active_requests: 0 } as Model;
    expect(removalReason(model)).toBeUndefined();
    expect(removalReason({ ...model, policy: "on-demand" })).toContain("Disabled");
    expect(removalReason({ ...model, loaded: true })).toContain("Unload");
    expect(removalReason({ ...model, active_requests: 1 })).toContain("active");
  });
  test("session identifiers are bounded by UTF-8 bytes and reject header control characters", () => {
    expect(sessionValue("opaque-session" )).toBe("opaque-session");
    for (const invalid of ["", "x\nInjected: value", "x".repeat(513), "é".repeat(257)]) expect(() => sessionValue(invalid)).toThrow();
  });
});

const view = "c".repeat(64);
const page = (total: number, next_after: string | null = null) => ({ total, next_after });
function libraryPage(rows: { jobs?: unknown[]; artifacts?: unknown[]; quarantined_files?: unknown[] } = {}) {
  const jobs = rows.jobs ?? [], artifacts = rows.artifacts ?? [], quarantined_files = rows.quarantined_files ?? [];
  return { schema_version: 2, generation: 1, view, limit: 64, charged_bytes: 32, source_policy: "public-https", read_only: true, recovery_authorized: false, jobs, artifacts, artifact_sets: [], quarantined_files, pages: { jobs: page(jobs.length), artifacts: page(artifacts.length), artifact_sets: page(0), quarantined_files: page(quarantined_files.length) }, selection: { artifact: null, artifact_set: null } };
}

describe("management response boundaries", () => {
  test("invalid generations and file paths never enable removal controls", () => {
    const file = { id: hash, sha256: hash, bytes: 16, references: [], reference_count: 0, active_holds: 0, removing: false };
    const inventory = libraryPage({ jobs: [job], artifacts: [file] });
    expect(parseArtifactInventory(inventory)).toEqual(inventory as unknown as ReturnType<typeof parseArtifactInventory>);
    expect(() => parseArtifactInventory({ ...inventory, view: "short" })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, limit: 129 })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, artifacts: [{ ...file, reference_count: -1 }] })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, artifacts: [{ ...file, references: ["model:a"], reference_count: 0 }] })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, pages: { ...inventory.pages, artifacts: page(0) } })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, pages: { ...inventory.pages, jobs: null } })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, selection: { artifact: { ...file, id: "../../file" }, artifact_set: null } })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, generation: "1" })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, artifacts: [{ ...file, id: "../../file" }] })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, jobs: [{ ...job, revision: -1 }] })).toThrow();
  });
  test("cache config and snapshot revisions are checked before compare-and-swap actions", () => {
    const value = { schema_version: 1, enabled: true, configuration_generation: 3, generation: 7, qualification: "unqualified", data: [{ id: "a".repeat(32), revision: 2, state: "saved", reserved_bytes: 32, bytes: 16, protocol: "openai.completions", session_digest: hash, active_readers: 0 }] };
    expect(parseCacheView(value)).toEqual(value);
    expect(() => parseCacheView({ ...value, configuration_generation: null })).toThrow();
    expect(() => parseCacheView({ ...value, data: [{ ...value.data[0], revision: "2" }] })).toThrow();
    for (const key of ["writer_bound", "writer_current_owner", "writer_actor_present", "cleanup_eligible"]) {
      expect(() => parseCacheView({ ...value, data: [{ ...value.data[0], [key]: "true" }] })).toThrow();
      expect(parseCacheView({ ...value, data: [{ ...value.data[0], [key]: true }] }).data[0]).toHaveProperty(key, true);
    }
    const reader = { id: "a".repeat(32), revision: 1, writer_current_owner: false, writer_actor_present: true };
    expect(parseCacheView({ ...value, data: [{ ...value.data[0], retained_readers: [reader] }] }).data[0]?.retained_readers).toEqual([reader]);
    for (const readers of [[{ ...reader, id: "../../private" }], [{ ...reader, id: ["a".repeat(32)] }], [{ ...reader, revision: 0 }], [{ ...reader, revision: -1 }], [{ ...reader, writer_actor_present: "true" }], [{ ...reader, release_eligible: "true" }], [reader, reader], {}]) expect(() => parseCacheView({ ...value, data: [{ ...value.data[0], retained_readers: readers }] })).toThrow();

  });
  test("unclaimed retained files expose bounded observations without authorizing artifacts", () => {
    const retained = { filename: `job-${"b".repeat(32)}.part`, reserved_bytes: 32, reason: "unreferenced_staging" };
    const inventory = libraryPage({ quarantined_files: [retained] });
    expect(parseArtifactInventory(inventory).quarantined_files).toEqual([retained]);
    expect(parseArtifactInventory(inventory).artifacts).toEqual([]);
    for (const invalid of [{ ...retained, filename: "../../other.bin" }, { ...retained, reserved_bytes: -1 }, { ...retained, reason: "x".repeat(129) }, null]) expect(() => parseArtifactInventory({ ...inventory, quarantined_files: [invalid] })).toThrow();
  });
  test("library cursors advance one section within a single membership view and keep selections", () => {
    const first = firstInventoryPages();
    expect(inventoryPath(first)).toBe("juiced/artifacts?limit=64");
    const observed = { ...libraryPage({ artifacts: [] }), pages: { ...libraryPage().pages, artifacts: page(130, hash) } } as unknown as Parameters<typeof nextInventoryPage>[1];
    const second = nextInventoryPage(first, observed, "artifacts");
    expect(second.view).toBe(view);
    expect(second.stacks.artifacts).toEqual([hash]);
    expect(second.stacks.jobs).toEqual([]);
    const path = new URLSearchParams(inventoryPath(second, { artifact: hash, artifact_set: `set-${hash}` }).split("?")[1]);
    expect(Object.fromEntries(path)).toEqual({ limit: "64", artifacts_after: hash, expected_view: view, artifact: hash, artifact_set: `set-${hash}` });
    expect(nextInventoryPage(second, { ...observed, view: "d".repeat(64) }, "artifacts")).toBe(second);
    expect(nextInventoryPage(second, observed, "jobs")).toBe(second);
    const back = previousInventoryPage(second, "artifacts");
    expect(back).toEqual(first);
    expect(inventoryPath(back)).toBe("juiced/artifacts?limit=64");
    expect(inventoryPath(first, {}, "artifact_sets", 128)).toBe("juiced/artifacts?limit=128&section=artifact_sets");
  });
  test("live protocol limitations and fixed endpoint paths remain distinct", () => {
    expect(parseProtocols({ data: [{ id: "mcp", support: "partial", note: "Stateless JSON only" }] })[0]?.support).toBe("partial");
    expect(parseEndpoints({ schema_version: 1, endpoints: [{ method: "POST", path: "/mcp", prefixes: [""], authority: "inference" }] })[0]?.path).toBe("/mcp");
    expect(() => parseEndpoints({ schema_version: 1, endpoints: [{ method: "POST", path: "/mcp", prefixes: "v1" }] })).toThrow();
  });
});

test("cache continuation validation keeps page order, view scope and incomplete orphan counts explicit", () => {
  const row = { id:"a".repeat(32), revision:2, state:"saved", reserved_bytes:64, bytes:64, protocol:"openai.completions", session_digest:"b".repeat(64), active_readers:0 };
  const page = { view:"c".repeat(64), limit:1, next_after:row.id };
  const view = { schema_version:1, enabled:true, configuration_generation:1, generation:2, qualification:"unqualified", data:[row], page };
  expect(parseCacheView(view).page).toEqual(page);
  for (const changed of [{ ...page, view:"private-instance" }, { ...page, limit:0 }, { ...page, limit:16385 }, { ...page, next_after:"d".repeat(32) }, { ...page, limit:2 }]) expect(() => parseCacheView({ ...view, page:changed })).toThrow();
  expect(() => parseCacheView({ ...view, data:[row,row], page:{ ...page, limit:2 } })).toThrow();
  expect(() => parseCacheView({ ...view, data:[{ ...row, id:"d".repeat(32) },row], page:{ ...page, limit:2 } })).toThrow();
  const inspection = { generation:2, read_only:true, recovery_authorized:false, charged_bytes:64, snapshots:[{ snapshot:row, issues:[], checksum_valid:true }], orphan_files:["kv-observed.bin"], issues:[], page, orphan_page:{ limit:1, next_after:null, observed_count:1, scan_complete:true } };
  expect(parseCacheInspection(inspection)).toEqual(inspection);
  expect(parseCacheInspection({ ...inspection, orphan_page:{ ...inspection.orphan_page, observed_count:4, scan_complete:false } }).orphan_page?.scan_complete).toBe(false);
  for (const changed of [{ ...inspection, generation:null }, { ...inspection, recovery_authorized:true }, { ...inspection, snapshots:[{ snapshot:row, issues:[], checksum_valid:"true" }] }, { ...inspection, orphan_page:{ ...inspection.orphan_page, observed_count:0 } }, { ...inspection, orphan_page:{ ...inspection.orphan_page, scan_complete:false, next_after:"kv-observed.bin" } }]) expect(() => parseCacheInspection(changed)).toThrow();
});
