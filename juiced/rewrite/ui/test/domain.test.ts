import { describe, expect, test } from "bun:test";
import { bytes, eligible, loadReason, matchingModels, normalizeBase, parseSnapshot, parseSuitability, unloadReason, workerState, workerReady, workerTone, type Model, type Suitability } from "../src/domain";

const model: Model = { id: "Publisher/Long-Model-Q4", policy: "on-demand", revision: 0, state: "unloaded", downloaded: true, loaded: false, pid: null, active_requests: 0, max_inflight: 2, resident_bytes: 1024 ** 3, operations: ["chat/completions"], error: null };

describe("runtime controller reservations", () => {
  const control = { state: "accounted", metadata_bytes: 4, binding_bytes: 2, binding_count: 1, scratch_bytes: 4, active_operations: 1, reserved_bytes: 10, outstanding_bytes: 10, bounds: { metadata_bytes: 4, bootstrap_bytes: 8, import_bytes: 16, response_work_bytes: 4, response_bytes: 2, binding_work_bytes: 4 } };
  const parse = (value: unknown) => parseSnapshot({ schema_version: 1, data: [] }, { schema_version: 1, implementation: "rust-experiment", version: "0.1.0", resources: { reserved_bytes: 10, runtime_control_memory: value } }, { data: [] }).resources.runtime_control_memory;
  test("retained metadata, bindings and actual work share one reported total without inventing residency", () => {
    expect(parse(control)?.reserved_bytes).toBe(10);
    expect(parse({ ...control, scratch_bytes: 0, active_operations: 0, reserved_bytes: 6, outstanding_bytes: 6 })?.binding_count).toBe(1);
    expect(parse({ ...control, metadata_bytes: 0, binding_bytes: 0, binding_count: 0, reserved_bytes: 4, outstanding_bytes: 4 })?.metadata_bytes).toBe(0);
    expect(parse({ ...control, state: "legacy", metadata_bytes: 0, binding_bytes: 0, scratch_bytes: 0, reserved_bytes: 0, outstanding_bytes: 0 })?.state).toBe("legacy");
    expect(parse(undefined)).toBeUndefined();
  });
  test("inconsistent or unknown observations cannot grant memory visibility", () => {
    for (const changed of [{ state: "qualified" }, { metadata_bytes: -1 }, { metadata_bytes: 3, reserved_bytes: 9, outstanding_bytes: 9 }, { reserved_bytes: 9 }, { outstanding_bytes: 9 }, { active_operations: 0 }, { binding_count: 0 }, { binding_bytes: 0, reserved_bytes: 8, outstanding_bytes: 8 }, { bounds: {} }, { state: "legacy" }]) expect(() => parse({ ...control, ...changed })).toThrow("runtime memory reservations");
    expect(() => parse(null)).toThrow("runtime memory reservations");
  });
});

describe("model eligibility and lifecycle guards", () => {
  test("tests never offer disabled, missing or non-chat models", () => {
    expect(eligible(model)).toBe(true);
    expect(eligible({ ...model, policy: "disabled" })).toBe(false);
    expect(eligible({ ...model, downloaded: false })).toBe(false);
    expect(eligible({ ...model, operations: ["responses"] })).toBe(false);
  });
  test("load and unload explain disabled, starting, missing, Hot and busy states", () => {
    expect(loadReason(model)).toBeUndefined();
    expect(loadReason({ ...model, policy: "disabled" })).toContain("policy");
    expect(loadReason({ ...model, state: "loading" })).toContain("starting");
    expect(loadReason({ ...model, downloaded: false })).toContain("unavailable");
    expect(unloadReason({ ...model, loaded: true, policy: "hot" })).toContain("On demand");
    expect(unloadReason({ ...model, loaded: true, active_requests: 1 })).toContain("active");
    expect(unloadReason({ ...model, loaded: true })).toBeUndefined();
  });
  test("uncertain worker generations retain cleanup access and cannot accept tests", () => {
    const pending: Model = { ...model, state: "cleanup-pending", loaded: true, pid: 123 };
    expect(eligible(pending)).toBe(false);
    expect(loadReason(pending)).toContain("confirm");
    expect(unloadReason(pending)).toBeUndefined();
    const stopping: Model = { ...pending, state: "stopping" };
    expect(eligible(stopping)).toBe(false);
    expect(unloadReason(stopping)).toContain("progress");
  });
  test("queued startup is distinct from a running or actively starting worker", () => {
    const queued: Model = { ...model, state: "loading", startup_waiting: true };
    expect(workerState(queued)).toBe("Waiting to start");
    expect(loadReason(queued)).toContain("waiting");
    expect(unloadReason(queued)).toContain("not loaded");
    expect(workerState({ ...queued, startup_waiting: false })).toBe("loading");
    expect(workerState({ ...model, state: "ready", loaded: true })).toBe("ready");
  });
  test("server-authorized cleanup can settle retained Hot work without enabling ordinary busy unload", () => {
    const pending: Model = { ...model, state: "cleanup-pending", policy: "hot", loaded: true, active_requests: 1, cleanup_retry_available: true };
    expect(unloadReason(pending)).toBeUndefined();
    expect(loadReason(pending)).toContain("confirm");
    expect(eligible(pending)).toBe(false);
    expect(workerReady(pending)).toBe(false);
    expect(workerTone(pending)).toBe("warning");
    expect(workerReady({ ...pending, state: "ready" })).toBe(true);
    expect(workerTone({ ...pending, state: "ready" })).toBe("good");
    expect(unloadReason({ ...pending, cleanup_retry_available: false })).toContain("active");
    expect(unloadReason({ ...pending, state: "ready" })).toContain("active");
    expect(unloadReason({ ...pending, state: "stopping" })).toContain("progress");
    expect(unloadReason({ ...pending, state: "ready", active_requests: 0 })).toContain("On demand");
  });
  test("search matches model identity, policy and operation without mutating inventory", () => {
    expect(matchingModels([model], " long-model ")).toEqual([model]);
    expect(matchingModels([model], "CHAT/COMPLETIONS")).toEqual([model]);
    expect(matchingModels([model], "hot")).toEqual([]);
  });
});

describe("connection boundary", () => {
  test("only accepts service origins and preserves same-origin selection", () => {
    expect(normalizeBase(" ")).toBe("");
    expect(normalizeBase("http://localhost:13306/")).toBe("http://localhost:13306");
    for (const invalid of ["file:///tmp", "http://user:secret@localhost", "http://localhost/v1", "http://localhost?key=secret", "http://localhost/#key"]) {
      expect(() => normalizeBase(invalid)).toThrow();
    }
  });
  test("rejects production or mismatched services before exposing mutation controls", () => {
    const catalog = { schema_version: 1, data: [model] };
    const state = { schema_version: 1, implementation: "rust-experiment", version: "0.1.0", resources: { reserved_bytes: 0 } };
    expect(parseSnapshot(catalog, state, { data: [] }).models).toEqual([model]);
    expect(() => parseSnapshot(catalog, { ...state, implementation: "native" }, { data: [] })).toThrow();
    expect(() => parseSnapshot({ ...catalog, schema_version: 2 }, state, { data: [] })).toThrow();
    expect(() => parseSnapshot({ ...catalog, data: [{ ...model, revision: "0" }] }, state, { data: [] })).toThrow();
    expect(() => parseSnapshot({ ...catalog, data: [{ ...model, startup_waiting: "true" }] }, state, { data: [] })).toThrow();
    expect(() => parseSnapshot({ ...catalog, data: [{ ...model, cleanup_retry_available: "true" }] }, state, { data: [] })).toThrow();
    expect(() => parseSnapshot({ ...catalog, data: [{ ...model, uncertain_state_operations: -1 }] }, state, { data: [] })).toThrow();
    expect(() => parseSnapshot({ ...catalog, data: [{ ...model, uncertain_state_operations: "1" }] }, state, { data: [] })).toThrow();
    expect(() => parseSnapshot({ ...catalog, data: [{ ...model, uncertain_requests: -1 }] }, state, { data: [] })).toThrow();
    expect(() => parseSnapshot({ ...catalog, data: [{ ...model, uncertain_requests: "1" }] }, state, { data: [] })).toThrow();
    expect(() => parseSnapshot({ ...catalog, data: [{ ...model, request_limits: { input_bytes: 1, batch: 1, output_tokens: 0 } }] }, state, { data: [] })).toThrow();
  });
  test("unknown memory stays unavailable", () => {
    expect(bytes(null)).toBe("Unavailable");
    expect(bytes(0)).toBe("0 B");
    expect(bytes(4096)).toBe("4.0 KiB");
    expect(bytes(1024 ** 3)).toBe("1.0 GiB");
  });
});


describe("read-only host budget observations", () => {
  const observation: Suitability = { schema_version: 1, read_only: true, resources: { fit: "unknown", blockers: [], declared_budget: { available_bytes: 0, additional_worker_bytes: 1024, fits_additional_reservation: false } } };
  test("preserves zero exhaustion and nullable unknown capacity without claiming fit", () => {
    expect(parseSuitability(observation).resources.declared_budget.available_bytes).toBe(0);
    const unknown = { ...observation, resources: { ...observation.resources, declared_budget: { available_bytes: null, additional_worker_bytes: null, fits_additional_reservation: null } } };
    expect(parseSuitability(unknown)).toBe(unknown);
  });
  test("rejects incomplete, malformed or qualified observations", () => {
    for (const budget of [{}, { available_bytes: -1, additional_worker_bytes: 1, fits_additional_reservation: true }, { available_bytes: 1, additional_worker_bytes: null, fits_additional_reservation: "true" }]) expect(() => parseSuitability({ ...observation, resources: { ...observation.resources, declared_budget: budget } })).toThrow();
    expect(() => parseSuitability({ ...observation, read_only: false })).toThrow();
    expect(() => parseSuitability({ ...observation, resources: { ...observation.resources, fit: "qualified" } })).toThrow();
  });
});

test("cache controller reservations remain distinct from model claims and unknown availability", () => {
  const catalog = { schema_version:1, data:[model] };
  const control = { state:"accounted" as const, metadata_bytes:64, scratch_bytes:32, reserved_bytes:96, outstanding_bytes:96, active_operations:1, bounds:{ bootstrap_bytes:128, metadata_bytes:64, scratch_bytes:32, state_scratch_bytes:32, page_scratch_bytes:16, response_bytes:32 } };
  const state = { schema_version:1, implementation:"rust-experiment", version:"0.1.0", resources:{ reserved_bytes:104, cache_control_memory:control } };
  expect(parseSnapshot(catalog, state, { data:[] }).resources.cache_control_memory).toEqual({ ...control, reason:null });
  for (const patch of [{ reserved_bytes:1 }, { active_operations:-1 }, { metadata_bytes:null }, { scratch_bytes:0 }, { state:"ready" }, { bounds:{ ...control.bounds, metadata_bytes:63 } }]) {
    expect(() => parseSnapshot(catalog, { ...state, resources:{ ...state.resources, cache_control_memory:{ ...control, ...patch } } }, { data:[] })).toThrow("cache memory");
  }
  const unavailable = { state:"unavailable", reason:"unknown_host_capacity", metadata_bytes:0, scratch_bytes:0, reserved_bytes:0, outstanding_bytes:0, active_operations:0 };
  expect(parseSnapshot(catalog, { ...state, resources:{ reserved_bytes:0, outstanding_bytes:0, cache_control_memory:unavailable } }, { data:[] }).resources.cache_control_memory?.state).toBe("unavailable");
  expect(() => parseSnapshot(catalog, { ...state, resources:{ cache_control_memory:{ ...unavailable, metadata_bytes:1, reserved_bytes:1 } } }, { data:[] })).toThrow("cache memory");
  expect(() => parseSnapshot(catalog, { ...state, resources:{ cache_control_memory:{ ...unavailable, state:["legacy"] } } }, { data:[] })).toThrow("cache memory");
});
