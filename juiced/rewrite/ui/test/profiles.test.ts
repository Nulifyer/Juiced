import { describe, expect, test } from "bun:test";
import { registration } from "../src/management";
import { launchDraft, launchOptions, editedLaunchOptions, parseModelContext, parseModelOptions, sameDraft, servingContext, type ModelContext } from "../src/profiles";

const launch = { context_tokens: 8192, threads: null, gpu_layers: 0, parallel_slots: 2 };
const context: ModelContext = {
  schema_version: 1, model: "model", revision: 3, provider: "llama-cpp",
  declared: { source: "GGUF", format_version: 3, architecture: "llama", training_context_tokens: 131072 },
  configured: launch, applied: null, observed: null, observation_error: "worker_unloaded", qualification: "unqualified",
};

describe("typed provider and launch boundary", () => {
  test("empty preserves defaults while GPU zero remains an explicit value", () => {
    expect(launchOptions({ context: "8192", threads: "", gpu: "0" }, 2)).toEqual(launch);
    expect(launchOptions({ context: "", threads: "", gpu: "" }, null)).toEqual({ context_tokens: null, threads: null, gpu_layers: null, parallel_slots: null });
    expect(launchDraft(launch)).toEqual({ context: "8192", threads: "", gpu: "0" });
    expect(sameDraft(launchDraft(launch), { context: "8192", threads: "", gpu: "0" })).toBe(true);
  });
  test("invalid zero context, zero threads, overflow and fractional launch values are rejected", () => {
    for (const value of ["0", "-1", "2147483648", "1.5", "8e3"]) expect(() => launchOptions({ context: value, threads: "", gpu: "" }, 2)).toThrow();
    for (const value of ["0", "65537"]) expect(() => launchOptions({ context: "", threads: value, gpu: "" }, 2)).toThrow();
    expect(() => launchOptions({ context: "", threads: "", gpu: "-1" }, 2)).toThrow();
  });
  test("provider is explicit and physical slots cannot silently change", () => {
    const args = '["--port","{port}","--model","{model}"]';
    expect(registration("a".repeat(64), "name.gguf", "engine", args, "1024", "2", ["completions"]).execution_profile.adapter).toBe("external-http");
    expect(registration("a".repeat(64), "one", "engine", args, "1024", "2", ["completions"], "llama-cpp", launch)).toMatchObject({ execution_profile: { adapter: "llama-cpp", options: launch } });
    expect(() => registration("a".repeat(64), "one", "engine", args, "1024", "1", ["completions"], "llama-cpp", launch)).toThrow("match");
    expect(() => registration("a".repeat(64), "one", "engine", args, "1024", "2", ["completions"], "external-http", launch)).toThrow("explicit llama.cpp");
  });
});

describe("context evidence and revisions", () => {
  test("training and launch budgets never imply a serving limit", () => {
    expect(parseModelContext(context)).toEqual(context);
    expect(servingContext(context)).toBeNull();
    const observed = { ...context, observed: { props: { source: "/props", per_request_tokens: 6000, slots: 2, engine_build: "fixture" }, slots: [{ id: 0, context_tokens: 2048, is_processing: false }, { id: 1, context_tokens: null, is_processing: null }] } };
    expect(servingContext(parseModelContext(observed))).toBe(6000);
    expect(observed.observed.slots[0]?.context_tokens).toBe(2048);
    expect(observed.observed.slots[1]?.context_tokens).toBeNull();
  });
  test("malformed profile revisions and observed values cannot enable edits or claim capacity", () => {
    const options = { model: "one", revision: 3, provider: "llama-cpp" as const, saved: launch, effective_launch: launch, physical_slots: 2, context_semantics: "total launch budget" };
    expect(parseModelOptions(options)).toEqual(options);
    expect(() => parseModelOptions({ ...options, revision: "3" })).toThrow();
    expect(() => parseModelOptions({ ...options, provider: "inferred-from-gguf" })).toThrow();
    expect(() => parseModelContext({ ...context, observed: { props: { per_request_tokens: "8192", slots: 2 }, slots: [] } })).toThrow();
  });
});


describe("imported settings ownership", () => {
  const imported = { model: "native", revision: 4, provider: "llama-cpp" as const, options_authority: "native-settings" as const, editable_fields: ["context_tokens"], saved: { context_tokens: 768, threads: null, gpu_layers: null, parallel_slots: null }, effective_launch: { context_tokens: 768, threads: null, gpu_layers: 0, parallel_slots: 1 }, physical_slots: 1, context_semantics: "total launch budget" };
  test("context updates and clearing do not create a competing backend or slot override", () => {
    const view = parseModelOptions(imported);
    expect(editedLaunchOptions(view, { context: "1024", threads: "", gpu: "" })).toEqual({ context_tokens: 1024, threads: null, gpu_layers: null, parallel_slots: null });
    expect(editedLaunchOptions(view, { context: "", threads: "", gpu: "" })).toEqual({ context_tokens: null, threads: null, gpu_layers: null, parallel_slots: null });
    expect(() => editedLaunchOptions(view, { context: "1024", threads: "2", gpu: "" })).toThrow("imported profile");
    expect(() => editedLaunchOptions(view, { context: "1024", threads: "", gpu: "0" })).toThrow("imported profile");
  });
  test("unknown owners and broader native editable metadata cannot enable an editor", () => {
    expect(() => parseModelOptions({ ...imported, options_authority: "unowned" })).toThrow();
    expect(() => parseModelOptions({ ...imported, editable_fields: ["context_tokens", "threads"] })).toThrow();
    expect(() => parseModelOptions({ ...imported, saved: { ...imported.saved, gpu_layers: 0 } })).toThrow();
    expect(() => parseModelOptions({ ...imported, provider: "external-http" })).toThrow();
  });
});
