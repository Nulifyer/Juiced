import { describe as group, expect, test } from "bun:test";
import { accumulate, action, candidates, describe, functionTools, implication, outcome, parseArguments, readOnly, systemPrompt, type Reply } from "../src/baristaTools";
import type { Model } from "../src/domain";

const empty: Reply = { content: "", reasoning: "", toolCalls: [] };

const model = (id: string, change: Partial<Model> = {}): Model => ({ id, policy: "on-demand", revision: 1, state: "unloaded", downloaded: true, loaded: false, pid: null, active_requests: 0, max_inflight: 1, resident_bytes: null, operations: ["chat/completions"], error: null, ...change });

group("barista", () => {
  test("maps tools to function tools and reads the read-only hint", () => {
    const tools = [{ name: "juiced_list_models", description: "List", inputSchema: { type: "object", properties: {} }, annotations: { readOnlyHint: true } }, { name: "juiced_load_model", annotations: { readOnlyHint: false } }];
    expect(readOnly(tools[0])).toBe(true);
    expect(readOnly(tools[1])).toBe(false);
    expect(readOnly(undefined)).toBe(false);
    expect(functionTools(tools)[0]).toEqual({ type: "function", function: { name: "juiced_list_models", description: "List", parameters: { type: "object", properties: {} } } });
  });

  test("offers downloaded chat models, loaded first, and explains loading", () => {
    const list = candidates([model("b"), model("a", { loaded: true, state: "ready" }), model("embed", { operations: ["embeddings"] }), model("missing", { downloaded: false }), model("off", { policy: "disabled" })]);
    expect(list.map((entry) => entry.id)).toEqual(["a", "b", "off"]);
    expect(implication(model("off", { policy: "disabled" }))).toBe("off is Disabled. Turning Barista on sets it to On demand.");
    expect(implication(model("a", { loaded: true }))).toContain("Loaded now");
    expect(implication(model("h", { policy: "hot" }))).toContain("Hot");
    expect(implication(model("b", { resident_bytes: 2 * 1024 ** 3 }))).toContain("when you send the first message. It reserves about");
  });

  test("joins streamed tool-call fragments by index", () => {
    let reply = accumulate(empty, { choices: [{ delta: { content: "Let me check. " } }] });
    reply = accumulate(reply, { choices: [{ delta: { tool_calls: [{ index: 0, id: "a", function: { name: "juiced_set_", arguments: "{\"model\":" } }] } }] });
    reply = accumulate(reply, { choices: [{ delta: { tool_calls: [{ index: 0, function: { name: "policy", arguments: "\"m\"}" } }, { index: 1, id: "b", function: { name: "juiced_status", arguments: "{}" } }] } }] });
    reply = accumulate(reply, { choices: [{ delta: { reasoning_content: "hmm" } }] });
    expect(reply).toEqual({ content: "Let me check. ", reasoning: "hmm", toolCalls: [{ id: "a", name: "juiced_set_policy", arguments: "{\"model\":\"m\"}" }, { id: "b", name: "juiced_status", arguments: "{}" }] });
    expect(accumulate(reply, { choices: [] })).toBe(reply);
    expect(accumulate(empty, { choices: [{ delta: { tool_calls: [{ index: 0, function: { name: "x", arguments: { a: 1 } } }] } }] }).toolCalls[0]!.arguments).toBe("{\"a\":1}");
  });

  test("parses only object arguments", () => {
    expect(parseArguments("")).toEqual({});
    expect(parseArguments("{\"a\":1}")).toEqual({ a: 1 });
    expect(parseArguments("[1]")).toBeNull();
    expect(parseArguments("{oops")).toBeNull();
  });

  test("describes reads, approvals and outcomes in plain words", () => {
    expect(describe("juiced_list_models", {})).toBe("Checked models");
    expect(describe("juiced_search_models", { query: "qwen" })).toBe("Searched for \"qwen\"");
    expect(describe("juiced_load_model", { model: "Ornith-1.5-35B-A3B-GGUF-Q6_K" })).toBe("Load Ornith-1.5-35B-A3B-GGUF-Q6_K?");
    expect(describe("juiced_set_policy", { model: "m", policy: "hot", expected_revision: 2 })).toBe("Set m to Hot?");
    expect(describe("juiced_set_model_settings", { model: "m", context_tokens: 8192, gpu_layers: null, apply: "reload" })).toBe("Change m: context tokens 8192, gpu layers automatic, and reload it now?");
    expect(describe("juiced_download_model", { repo: "o/r", file: "f.gguf" })).toBe("Download f.gguf from o/r?");
    expect(action("juiced_load_model", { model: "m" })).toBe("Load m");
    expect(outcome("juiced_set_policy", { model: "m", policy: "on-demand" }, { ok: true, text: "", value: null })).toBe("m is now On demand.");
    expect(outcome("juiced_set_policy", {}, { ok: false, text: "", value: { error: { code: "revision_conflict", message: "Refresh first." } } })).toBe("Refresh first.");
  });

  test("system prompt names the role, model, authority and machine", () => {
    const bare = systemPrompt("m", "read_only", null);
    expect(bare).toContain("You are Barista");
    expect(bare).toContain("model m");
    expect(bare).toContain("Permission: Read only");
    expect(bare).toContain("only when a tool result shows it");
    expect(systemPrompt("m", "full", null)).toContain("Full control");
    expect(systemPrompt("m", "ask", null)).toContain("approves or skips");
    const prompt = systemPrompt("m", "full", { host: { os: "linux", architecture: "x86_64" }, cpu: { name: "Ryzen", logical_threads: 16 }, ram: { total_bytes: 32 * 1024 ** 3, available_bytes: 16 * 1024 ** 3 }, gpus: [{ name: "Radeon", vram_total_bytes: 16 * 1024 ** 3 }], memory: { resident_budget_bytes: 24 * 1024 ** 3, reserved_bytes: 0 }, recommended_backends: { llamacpp: "vulkan" }, loaded_models: [] });
    expect(prompt).toContain("CPU Ryzen with 16 threads");
    expect(prompt).toContain("GPUs: Radeon 16.0 GiB");
    expect(prompt).toContain("llamacpp: vulkan");
    expect(prompt.length).toBeLessThan(1600);
    const later = systemPrompt("m", "full", { host: { os: "linux", architecture: "x86_64" }, cpu: { name: "Ryzen", logical_threads: 16 }, ram: { total_bytes: 32 * 1024 ** 3, available_bytes: 3 * 1024 ** 3 }, gpus: [{ name: "Radeon", vram_total_bytes: 16 * 1024 ** 3 }], memory: { resident_budget_bytes: 24 * 1024 ** 3, reserved_bytes: 20 * 1024 ** 3 }, recommended_backends: { llamacpp: "vulkan" }, loaded_models: [{ id: "x" }] });
    expect(later).toBe(prompt);
  });
});
