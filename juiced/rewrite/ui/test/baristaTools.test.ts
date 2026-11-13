import type { Client } from "../src/api";
import { describe as group, expect, test } from "bun:test";
import { accumulate, action, approvedArgs, candidates, changeLines, describe, functionTools, implication, jobLine, modelText, outcome, parseArguments, readOnly, streamReply, systemPrompt, type PromptIndex, type Reply } from "../src/baristaTools";
import type { Model } from "../src/domain";

const empty: Reply = { content: "", reasoning: "", toolCalls: [] };

const model = (id: string, change: Partial<Model> = {}): Model => ({ id, policy: "on-demand", revision: 1, state: "unloaded", downloaded: true, loaded: false, pid: null, active_requests: 0, max_inflight: 1, resident_bytes: null, operations: ["chat/completions"], error: null, ...change });

group("barista", () => {
  test("maps tools to function tools and reads the read-only hint", () => {
    const tools = [{ name: "juiced_list_models", description: "List", inputSchema: { type: "object", properties: {} }, annotations: { readOnlyHint: true } }, { name: "juiced_load", annotations: { readOnlyHint: false } }];
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
    expect(describe("juiced_load", { model: "Ornith-1.5-35B-A3B-GGUF-Q6_K" })).toBe("Load Ornith-1.5-35B-A3B-GGUF-Q6_K?");
    expect(describe("juiced_set_policy", { model: "m", policy: "hot" })).toBe("Set m to Hot?");
    expect(describe("juiced_set_model_settings", { model: "m", changes: { context_tokens: 8192, gpu_layers: null }, apply: "reload" })).toBe("Change m: context tokens 8192, gpu layers automatic, and reload it now?");
    expect(describe("juiced_download", { repo: "o/r", quant: "Q4_K_M" })).toBe("Download Q4_K_M from o/r?");
    expect(describe("juiced_create_api_key", { name: "SillyTavern", scope: "inference" })).toBe("Create an inference key named SillyTavern?");
    expect(describe("juiced_test_model", { model: "m", kind: "speech" })).toBe("Tested m (speech)");
    expect(describe("juiced_help", { topic: "setup-voice" })).toBe("Read the setup-voice procedure");
    expect(action("juiced_load", { model: "m" })).toBe("Load m");
    expect(describe("juiced_delete_files", { items: ["file:a", "partial:b"] })).toBe("Delete 2 storage items?");
    expect(outcome("juiced_set_policy", {}, { ok: true, text: "", value: { ok: true, summary: "m is now on-demand (unloaded)." } })).toBe("m is now on-demand (unloaded).");
    expect(outcome("juiced_set_policy", {}, { ok: false, text: "", value: { ok: false, error: { code: "revision_conflict", message: "Refresh first." } } })).toBe("Refresh first.");
  });

  test("binds an approved change to what its preview showed", () => {
    const args = { items: ["file:a"], expected_revision: "stale" };
    const preview = { ok: true, text: "", value: { ok: true, data: { apply_with: { expected_revision: "abc123", ignored: { nested: true } } } } };
    expect(approvedArgs(args, preview)).toEqual({ items: ["file:a"], expected_revision: "abc123" });
    expect(approvedArgs(args, { ...preview, ok: false })).toBe(args);
    expect(approvedArgs(args, null)).toBe(args);
    expect(approvedArgs({ model: "m" }, { ok: true, text: "", value: { ok: true, data: {} } })).toEqual({ model: "m" });
  });

  test("shows before and after, and job progress, in plain words", () => {
    expect(changeLines([{ target: "m", field: "policy", before: "on-demand", after: "hot" }, { target: "m", field: "context_tokens", before: null, after: 131072 }])).toEqual(["policy: On demand → Hot", "context tokens: none → 131,072"]);
    expect(changeLines([{ target: "file:1", field: "Tiny-Chat-Q4_K_M.gguf", before: "on disk", after: "deleted" }])).toEqual(["Tiny-Chat-Q4_K_M.gguf: on disk → deleted"]);
    expect(changeLines(undefined)).toEqual([]);
    expect(jobLine({ id: "pull:a", kind: "download", label: "o/r Q4_K_M", state: "downloading", done: false, progress: 0.42, done_bytes: 1024, total_bytes: 4096 })).toBe("o/r Q4_K_M: downloading 42% · 1.0 KiB of 4.0 KiB");
    expect(jobLine({ id: "engine:a", kind: "engine_install", label: "llama.cpp cpu", state: "failed", done: true, progress: null, error: "digest_mismatch" })).toBe("llama.cpp cpu: failed (digest_mismatch)");
    const text = modelText({ ok: true, summary: "s", data: { a: 1 }, source: [{ route: "GET /v1/x" }] });
    expect(JSON.parse(text)).toEqual({ ok: true, summary: "s", data: { a: 1 } });
  });

  test("system prompt is the service's small index plus a stable machine line", () => {
    const index: PromptIndex = { version: 2, role: "You are Barista, the operator assistant.", rules: ["Read before you change.", "For a job with several steps, call juiced_help."], authority: { full: "Full control. Go.", ask: "Ask before changes. The operator approves or skips.", read_only: "Read only. Look only." }, groups: [{ group: "inspect", title: "Inspect", purpose: "read state", tools: ["juiced_status"] }], skills: [{ topic: "setup-voice", title: "Set up Home Assistant voice" }] };
    const bare = systemPrompt("m", "read_only", null, index);
    expect(bare).toContain("You are Barista");
    expect(bare).toContain("You run on the model m.");
    expect(bare).toContain("Permission: Read only. Look only.");
    expect(bare).toContain("Tools: Inspect (read state).");
    expect(bare).toContain("Procedures for juiced_help: setup-voice (Set up Home Assistant voice).");
    expect(bare).not.toContain("juiced_status");
    expect(systemPrompt("m", "full", null)).toContain("Full control");
    expect(systemPrompt("m", "ask", null)).toContain("approves or skips");
    const status = (free: number) => ({ host: { os: "linux", architecture: "x86_64" }, cpu: { name: "Ryzen", logical_threads: 16 }, ram_bytes: 32 * 1024 ** 3, gpus: [{ name: "Radeon", vram_total_bytes: 16 * 1024 ** 3 }], memory: { budget_bytes: 24 * 1024 ** 3, free_bytes: free }, loaded_models: [] });
    const prompt = systemPrompt("m", "full", status(20 * 1024 ** 3), index);
    expect(prompt).toContain("CPU Ryzen with 16 threads");
    expect(prompt).toContain("GPUs: Radeon 16.0 GiB");
    expect(prompt).toContain("Model memory budget 24.0 GiB");
    expect(prompt.length).toBeLessThan(2400);
    expect(systemPrompt("m", "full", status(1024 ** 3), index)).toBe(prompt);
  });
});

const sse = (chunks: unknown[]) => new Response(chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("") + "data: [DONE]\n\n");
test("streamReply asks once more with thinking off when the limit ends a reply that only reasoned", async () => {
  const bodies: any[] = [];
  const replies = [
    sse([{ choices: [{ delta: { reasoning_content: "thinking..." } }] }, { choices: [{ delta: {}, finish_reason: "length" }] }]),
    sse([{ choices: [{ delta: { content: "Paris." } }] }, { choices: [{ delta: {}, finish_reason: "stop" }] }]),
  ];
  const client = { stream: async (_path: string, body: unknown) => { bodies.push(body); return replies.shift()!; } } as unknown as Client;
  const reply = await streamReply(client, "m", [{ role: "user", content: "Capital of France?" }], [], 16, () => undefined);
  expect(reply.content).toBe("Paris.");
  expect(reply.reasoning).toBe("thinking...");
  expect(bodies).toHaveLength(2);
  expect(bodies[0].chat_template_kwargs).toBeUndefined();
  expect(bodies[1].chat_template_kwargs).toEqual({ enable_thinking: false });
});
test("streamReply keeps a reply that was cut off after it answered or called a tool", async () => {
  let calls = 0;
  const client = { stream: async () => { calls += 1; return sse([{ choices: [{ delta: { content: "Partial" } }] }, { choices: [{ delta: {}, finish_reason: "length" }] }]); } } as unknown as Client;
  const reply = await streamReply(client, "m", [{ role: "user", content: "x" }], [], 16, () => undefined);
  expect(reply).toMatchObject({ content: "Partial", finish: "length" });
  expect(calls).toBe(1);
});
