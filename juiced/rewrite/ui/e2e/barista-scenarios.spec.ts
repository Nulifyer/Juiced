import type { APIRequestContext } from "@playwright/test";
import { api, expect, hotModel, modelId, speechModel, test } from "./support";

/**
 * Deterministic tool-call scripts for the eight Barista jobs, run against the fixture service.
 * Each script is what a correct model would call; the assertions are what Barista must be able
 * to prove from the results. This checks the tools, not a model: see scripts/barista-eval.ts.
 */
type Envelope = { ok: boolean; tool: string; summary: string; data?: any; changed?: any[]; job?: any; next?: string[]; warnings?: string[]; preview?: boolean; error?: { code: string; message: string } };

function barista(request: APIRequestContext) {
  const service = api(request);
  let revision = 0;
  const log: string[] = [];
  return {
    service, log,
    async start(permission: "full" | "ask" | "read_only" = "full") { revision = (await service.setBarista(true, hotModel, permission)).revision; },
    async raw(name: string, args: unknown = {}, confirmed = false) {
      return request.post("/v1/juiced/barista/tools/call", { data: { name, arguments: args, revision, confirmed } });
    },
    async call(name: string, args: Record<string, unknown> = {}, confirmed = false): Promise<Envelope> {
      const response = await this.raw(name, args, confirmed);
      expect(response.status(), `${name}: ${await response.text()}`).toBe(200);
      const envelope = await response.json() as Envelope;
      expect(envelope.tool).toBe(name);
      expect(JSON.stringify(envelope).length).toBeLessThanOrEqual(12 * 1024);
      log.push(`${name} ${envelope.ok ? "ok" : `failed ${envelope.error?.code}`}: ${envelope.summary}`);
      return envelope;
    },
    async ok(name: string, args: Record<string, unknown> = {}, confirmed = false) {
      const envelope = await this.call(name, args, confirmed);
      expect(envelope.ok, `${name}: ${JSON.stringify(envelope)}`).toBe(true);
      return envelope;
    },
    async wait(id: string) {
      for (let attempt = 0; attempt < 4; attempt += 1) {
        const waited = await this.ok("juiced_wait_job", { id, max_seconds: 30 });
        if (waited.job.done) return waited;
      }
      throw new Error(`${id} did not finish`);
    },
  };
}

test.describe("Barista jobs", () => {
  test.beforeEach(({}, info) => { test.skip(info.project.name !== "desktop", "The scripts call the service directly; one viewport is enough."); });
  test.setTimeout(120000);

  test("1. set this box up for Home Assistant voice", async ({ request }) => {
    const b = barista(request);
    await b.start();
    const procedure = await b.ok("juiced_help", { topic: "setup-voice" });
    expect(procedure.data.procedure).toContain("juiced_set_wyoming");
    const status = await b.ok("juiced_status");
    expect(status.data.memory.free_bytes).toBeGreaterThan(0);
    await b.ok("juiced_hardware");
    const stt = await b.ok("juiced_list_models", { task: "transcription" });
    expect(stt.data.models.items.map((model: any) => model.id)).toContain(speechModel);
    const tts = await b.ok("juiced_list_models", { task: "tts" });
    expect(tts.data.models.total).toBe(0);
    const preview = await b.ok("juiced_set_policy", { model: modelId, policy: "hot", preview: true });
    expect(preview.data.effect.fits_budget).toBe(true);
    const hot = await b.ok("juiced_set_policy", { model: modelId, policy: "hot" });
    expect(hot.changed).toEqual([{ target: modelId, field: "policy", before: "on-demand", after: "hot" }]);
    const wyoming = await b.ok("juiced_set_wyoming", { enabled: true, stt_model: speechModel, listen: "127.0.0.1:13526" });
    expect(wyoming.data.listener.listening).toBe(true);
    const transcript = await b.ok("juiced_test_model", { model: speechModel, kind: "transcribe" });
    expect(transcript.data.text.length).toBeGreaterThan(0);
    expect(hot.next).toEqual(["juiced_load"]);
    const loaded = await b.ok("juiced_load", { model: modelId });
    expect(loaded.changed![0]).toMatchObject({ field: "state", after: "ready" });
    const chat = await b.ok("juiced_test_model", { model: modelId });
    expect(chat.data.output.length).toBeGreaterThan(0);
    const connections = await b.ok("juiced_connections");
    expect(connections.data.wyoming.listening).toBe(true);
    expect(connections.data.protocols.find((row: any) => row.id === "openai").url).toMatch(/\/v1$/);
    await b.ok("juiced_set_wyoming", { enabled: false });
    await b.ok("juiced_set_policy", { model: modelId, policy: "on-demand" });
    await b.ok("juiced_unload", { model: modelId });
  });

  test("2. find and run the latest model that fits", async ({ request }) => {
    const b = barista(request);
    await b.start();
    await b.ok("juiced_help", { topic: "find-and-run-model" });
    const found = await b.ok("juiced_search_models", { query: "Tiny-Chat", task: "chat" });
    expect(found.data.models[0].repo).toBe("fixture-labs/Tiny-Chat-GGUF");
    const repo = await b.ok("juiced_inspect_repo", { repo: "fixture-labs/Tiny-Chat-GGUF" });
    const quant = repo.data.quants.find((row: any) => row.label === "Q8_0");
    expect(["fits", "partial_offload", "cpu_only"]).toContain(quant.fit.verdict);
    const started = await b.ok("juiced_download", { repo: "fixture-labs/Tiny-Chat-GGUF", quant: "Q8_0" });
    expect(started.job.id).toMatch(/^pull:/);
    await b.wait(started.job.id);
    const local = await b.ok("juiced_list_local_files", { query: "Tiny-Chat" });
    const source = local.data.groups.items[0].quants.find((row: any) => row.label === "Q8_0").add;
    let added = await b.call("juiced_add_model", { ...source, id: "tiny-chat-q8", policy: "on-demand" });
    if (!added.ok && added.error?.code === "engine_not_installed") {
      const engines = await b.ok("juiced_list_engines");
      const build = engines.data.installable.find((row: any) => row.engine === "llama.cpp" && row.availability === "available");
      const install = await b.ok("juiced_install_engine", { engine: build.engine, backend: build.backend });
      if (install.job) await b.wait(install.job.id);
      added = await b.ok("juiced_add_model", { ...source, id: "tiny-chat-q8", policy: "on-demand" });
    }
    expect(added.ok, JSON.stringify(added)).toBe(true);
    const details = await b.ok("juiced_model_details", { model: "tiny-chat-q8" });
    expect(details.data.task).toBe("chat");
    const loaded = await b.call("juiced_load", { model: "tiny-chat-q8" });
    if (loaded.ok) {
      const test = await b.ok("juiced_test_model", { model: "tiny-chat-q8" });
      expect(test.data.kind).toBe("chat");
      await b.ok("juiced_unload", { model: "tiny-chat-q8" });
    } else {
      // The fixture's GGUF holds metadata only, so a real engine may refuse it; the failure must say why.
      expect(loaded.summary.length).toBeGreaterThan(0);
    }
    await b.ok("juiced_set_policy", { model: "tiny-chat-q8", policy: "disabled" });
    const removed = await b.ok("juiced_remove_model", { model: "tiny-chat-q8" });
    expect(removed.data.removed).toBe(true);
    // Later specs expect the fixture hub's quants to start undownloaded.
    for (const pull of (await b.service.json<{ data: any[] }>("/v1/juiced/library/pulls")).data) await request.post(`/v1/juiced/library/pulls/${pull.pull}`, { data: { action: "dismiss" } });
    const q8 = "90385a03c0f9f8ce448ffd8267737c48fc94142a83b5eed787e6e43f97c0d1df";
    await expect.poll(async () => {
      const inventory = await b.service.json<any>("/v1/juiced/artifacts?limit=128");
      if (!inventory.artifacts.some((item: any) => item.id === q8)) return "gone";
      const response = await request.post("/v1/juiced/artifacts/remove", { data: { artifact: q8, expected_generation: inventory.generation } });
      return response.ok() ? "gone" : (await response.json()).error.code;
    }, { timeout: 15000 }).toBe("gone");
  });

  test("3. increase a model's context, previewing the fit first", async ({ request }) => {
    const b = barista(request);
    await b.start();
    const model = "fixture.managed-runtime";
    await b.ok("juiced_help", { topic: "increase-context" });
    const before = await b.ok("juiced_model_details", { model });
    const changes = { context_tokens: 4096, cache_type_k: "q8_0", cache_type_v: "q8_0" };
    const preview = await b.ok("juiced_set_model_settings", { model, changes, preview: true });
    expect(preview.preview).toBe(true);
    expect(preview.changed).toEqual([
      { target: model, field: "context_tokens", before: before.data.settings.saved.context_tokens ?? null, after: 4096 },
      { target: model, field: "cache_type_k", before: null, after: "q8_0" },
      { target: model, field: "cache_type_v", before: null, after: "q8_0" },
    ]);
    expect(preview.summary).toContain("context_tokens");
    expect(preview.data.memory ?? preview.warnings?.find((warning) => warning.startsWith("No memory estimate"))).toBeTruthy();
    expect((await b.ok("juiced_model_details", { model })).data.settings.saved).toEqual(before.data.settings.saved);
    const applied = await b.ok("juiced_set_model_settings", { model, changes, apply: "next_load" });
    expect(applied.changed).toEqual(preview.changed);
    expect(applied.data.saved).toMatchObject(changes);
    const after = await b.ok("juiced_model_details", { model });
    expect(after.data.context.configured_tokens).toBe(4096);
    expect(after.data.settings.saved.cache_type_k).toBe("q8_0");
    const speech = await b.call("juiced_set_model_settings", { model: speechModel, changes: { context_tokens: 4096 }, preview: true });
    expect(speech.error?.code).toBe("settings_unsupported");
    await b.ok("juiced_set_model_settings", { model, changes: { context_tokens: null, cache_type_k: null, cache_type_v: null } });
    expect((await b.ok("juiced_model_details", { model })).data.settings.saved).toEqual(before.data.settings.saved);
  });

  test("4. explain why the last chat was slow from request records", async ({ request }) => {
    const b = barista(request);
    await b.start();
    await b.ok("juiced_help", { topic: "diagnose-slow" });
    await b.ok("juiced_test_model", { model: hotModel, input: "Summarize what a model server does." });
    const records = await b.ok("juiced_requests", { model: hotModel, limit: 5 });
    const latest = records.data.records[0];
    for (const field of ["elapsed_ms", "queue_wait_ms", "input_tokens", "prompt_processed_tokens", "decode_tokens_per_second"]) expect(latest, field).toHaveProperty(field);
    const details = await b.ok("juiced_model_details", { model: hotModel });
    expect(details.data.state).toBe("ready");
    const cache = await b.ok("juiced_cache", { model: hotModel });
    expect(cache.data.models[0]).toHaveProperty("reason");
    expect(cache.data.reuse_evidence.requests).toBeGreaterThan(0);
  });

  test("5. free disk space only through previews and reference checks", async ({ request }) => {
    const b = barista(request);
    await b.start();
    const before = await b.service.catalog();
    const procedure = await b.ok("juiced_help", { topic: "free-disk" });
    expect(procedure.data.procedure).toContain("expected_revision");
    const free = await b.ok("juiced_storage", { summary: true });
    expect(free.data.volumes.length).toBeGreaterThan(0);
    const tiny = "45957cc9a23e06484db4e445c3ad4250730296d331118687038f4c1d4d403f7f";
    await b.service.removeArtifact(tiny);
    const started = await b.ok("juiced_download", { repo: "fixture-labs/Tiny-Chat-GGUF", quant: "Q4_K_M" });
    await b.wait(started.job.id);
    const storage = await b.ok("juiced_storage", { kind: "file" });
    expect(typeof storage.data.revision).toBe("string");
    const file = storage.data.items.items.find((item: any) => item.title.includes("Tiny-Chat") && item.reclaimable);
    expect(file, JSON.stringify(storage.data.items)).toBeTruthy();
    const leftovers = await b.ok("juiced_storage", { kind: "legacy" });
    expect(leftovers.data.items.items.map((item: any) => item.id)).toContain("legacy:cache");
    expect(leftovers.warnings?.some((warning) => warning.includes("v0.9.2 leftovers"))).toBe(true);
    const unnamed = await b.call("juiced_delete_files", { items: ["legacy:cache"], preview: true });
    expect(unnamed.error?.code).toBe("legacy_requires_operator");
    const sessions = await b.ok("juiced_forget_sessions", { all: true, preview: true });
    expect(sessions.data).toHaveProperty("bytes_freed");
    const remove = await b.ok("juiced_remove_model", { model: "fixture.missing-weights", delete_files: true, preview: true });
    expect(remove.data.blockers).toEqual([]);
    expect(typeof remove.data.storage_revision).toBe("string");
    const preview = await b.ok("juiced_delete_files", { items: [file.id], preview: true });
    expect(preview.data.freed_bytes).toBe(file.bytes);
    expect(preview.changed).toEqual([{ target: file.id, field: file.title, before: "on disk", after: "deleted" }]);
    const unrevised = await b.call("juiced_delete_files", { items: [file.id] });
    expect(unrevised.error?.code).toBe("storage_revision_required");
    const stale = await b.call("juiced_delete_files", { items: [file.id], expected_revision: "0000000000000000" });
    expect(stale.error?.code).toBe("storage_revision_conflict");
    const deleted = await b.ok("juiced_delete_files", { items: [file.id], expected_revision: preview.data.apply_with.expected_revision });
    expect(deleted.data.freed_bytes).toBe(file.bytes);
    expect(deleted.changed).toEqual(preview.changed);
    const after = await b.ok("juiced_storage", { kind: "file" });
    expect(after.data.items.items.map((item: any) => item.id)).not.toContain(file.id);
    for (const pull of (await b.service.json<{ data: any[] }>("/v1/juiced/library/pulls")).data) await request.post(`/v1/juiced/library/pulls/${pull.pull}`, { data: { action: "dismiss" } });
    await b.service.removeArtifact(tiny);
    expect(await b.service.catalog()).toEqual(before);
  });

  test("6. connect SillyTavern without the model seeing a secret", async ({ request }) => {
    const b = barista(request);
    await b.start();
    await b.ok("juiced_help", { topic: "connect-client" });
    const connections = await b.ok("juiced_connections");
    expect(connections.data.protocols.find((row: any) => row.id === "openai").enabled).toBe(true);
    expect(connections.data.model_ids.map((row: any) => row.id)).toContain(hotModel);
    const key = await b.call("juiced_create_api_key", { name: "SillyTavern", scope: "inference" });
    expect(key.error?.code).toBe("first_key_from_connect");
    const alias = await b.ok("juiced_set_alias", { alias: "sillytavern", target: hotModel });
    expect(alias.changed![0]).toMatchObject({ before: null, after: hotModel });
    expect((await b.ok("juiced_connections")).data.aliases).toContainEqual({ alias: "sillytavern", target: hotModel });
    const off = await b.ok("juiced_set_protocol", { protocol: "ollama", enabled: false });
    expect(off.changed![0]).toMatchObject({ before: true, after: false });
    await b.ok("juiced_set_protocol", { protocol: "ollama", enabled: true });
    await b.ok("juiced_set_alias", { alias: "sillytavern", target: null });
  });

  test("7. check the cache with evidence, not file presence", async ({ request }) => {
    const b = barista(request);
    await b.start();
    const before = await b.ok("juiced_cache", { model: hotModel });
    expect(["off", "memory", "persistent"]).toContain(before.data.models[0].effective);
    for (let run = 0; run < 2; run += 1) await b.ok("juiced_test_model", { model: hotModel, input: "Repeat check for the cache." });
    const after = await b.ok("juiced_cache", { model: hotModel });
    expect(after.data.reuse_evidence.requests).toBeGreaterThanOrEqual(before.data.reuse_evidence.requests + 2);
    expect(after.summary).toMatch(/recent request\(s\) show reuse/);
  });

  test("8. answer a how-to question from the bundled docs", async ({ request }) => {
    const b = barista(request);
    await b.start();
    await b.ok("juiced_help", { topic: "how-to" });
    const found = await b.ok("juiced_docs", { query: "loading policy" });
    expect(found.data.matches.length).toBeGreaterThan(0);
    const page = await b.ok("juiced_docs", { page: found.data.matches[0].page });
    expect(page.data.text.length).toBeGreaterThan(0);
    expect(page.data.console).toBeTruthy();
  });

  test("revoking Barista stops a server-side wait and later changes", async ({ request }) => {
    const b = barista(request);
    await b.start();
    const tiny = "45957cc9a23e06484db4e445c3ad4250730296d331118687038f4c1d4d403f7f";
    await b.service.removeArtifact(tiny);
    const started = await b.ok("juiced_download", { repo: "fixture-labs/Tiny-Chat-GGUF", quant: "Q4_K_M" });
    const waiting = b.raw("juiced_wait_job", { id: started.job.id, max_seconds: 30 });
    await new Promise((resolve) => setTimeout(resolve, 600));
    const began = Date.now();
    await b.service.setBarista(true, hotModel, "read_only");
    const stopped = await (await waiting).json();
    expect(Date.now() - began).toBeLessThan(5000);
    expect(stopped.error.code).toBe("assistant_settings_changed");
    const later = await b.raw("juiced_set_policy", { model: modelId, policy: "hot" });
    expect(later.status()).toBe(409);
    await expect.poll(async () => (await b.service.json<{ data: any[] }>("/v1/juiced/library/pulls")).data.find((pull) => `pull:${pull.pull}` === started.job.id)?.state, { timeout: 20000 }).toBe("complete");
    for (const pull of (await b.service.json<{ data: any[] }>("/v1/juiced/library/pulls")).data) await request.post(`/v1/juiced/library/pulls/${pull.pull}`, { data: { action: "dismiss" } });
    await b.service.removeArtifact(tiny);
  });
});
