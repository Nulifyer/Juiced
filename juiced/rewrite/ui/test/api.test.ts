import { afterEach, expect, spyOn, test } from "bun:test";
import { Client } from "../src/api";

let fetchSpy: ReturnType<typeof spyOn> | undefined;
afterEach(() => fetchSpy?.mockRestore());

test("credentials use only the authorization header and cancellation is forwarded", async () => {
  let seenUrl = "";
  let seen: RequestInit | undefined;
  fetchSpy = spyOn(globalThis, "fetch").mockImplementation(Object.assign(async (url: Parameters<typeof fetch>[0], options?: Parameters<typeof fetch>[1]) => {
    seenUrl = String(url); seen = options;
    return Response.json({ ok: true });
  }, { preconnect: fetch.preconnect }));
  const controller = new AbortController();
  const client = new Client("http://localhost:13306", "private-key");
  await client.request("load", { model: "one" }, controller.signal);
  expect(seenUrl).toBe("http://localhost:13306/v1/load");
  expect(seen?.headers).toEqual({ Authorization: "Bearer private-key", "Content-Type": "application/json" });
  expect(seen?.body).toBe('{"model":"one"}');
  expect(seen?.signal).toBe(controller.signal);
  expect(seen?.redirect).toBe("error");
});

test("structured conflict errors preserve the server's recovery instruction", async () => {
  fetchSpy = spyOn(globalThis, "fetch").mockResolvedValue(Response.json({ error: { code: "revision_conflict", message: "Refresh and review current policy." } }, { status: 409 }));
  await expect(new Client("", "").request("juiced/policy", {})).rejects.toMatchObject({ status: 409, code: "revision_conflict", message: "Refresh and review current policy." });
});

test("unreadable responses produce a bounded actionable error", async () => {
  fetchSpy = spyOn(globalThis, "fetch").mockResolvedValue(new Response("private stack trace", { status: 502 }));
  await expect(new Client("", "").request("health")).rejects.toMatchObject({ status: 502, code: "invalid_json", message: "The service returned an unreadable response (502)." });
});

test("session-bound completion sends the opaque identifier only in its header", async () => {
  let seenUrl = ""; let seen: RequestInit | undefined;
  fetchSpy = spyOn(globalThis, "fetch").mockImplementation(Object.assign(async (url: Parameters<typeof fetch>[0], options?: Parameters<typeof fetch>[1]) => {
    seenUrl = String(url); seen = options;
    return Response.json({ choices: [{ text: "fixture" }] });
  }, { preconnect: fetch.preconnect }));
  await new Client("", "operator-key").request("completions", { model: "one", prompt: "temporary", stream: false }, undefined, "opaque-context");
  expect(seenUrl).toBe("/v1/completions");
  expect(seen?.headers).toEqual({ Authorization: "Bearer operator-key", "X-Juiced-Session": "opaque-context", "Content-Type": "application/json" });
  expect(seen?.body).not.toContain("opaque-context");
  expect(seen?.body).not.toContain("operator-key");
});

test("snapshot polling forwards one cancellation signal to every inventory request", async () => {
  const seen: (AbortSignal | null | undefined)[] = [];
  fetchSpy = spyOn(globalThis, "fetch").mockImplementation(Object.assign(async (_url: Parameters<typeof fetch>[0], options?: Parameters<typeof fetch>[1]) => {
    seen.push(options?.signal);
    return Response.json({});
  }, { preconnect: fetch.preconnect }));
  const controller = new AbortController();
  await expect(new Client("", "").snapshot(controller.signal)).rejects.toThrow("matching Rust experiment");
  expect(seen).toEqual([controller.signal, controller.signal, controller.signal]);
});


test("temporary chat uses an explicit reply bound within the service profile and forwards cancellation", async () => {
  const calls: RequestInit[] = [];
  fetchSpy = spyOn(globalThis, "fetch").mockImplementation(Object.assign(async (_url: Parameters<typeof fetch>[0], options?: Parameters<typeof fetch>[1]) => {
    calls.push(options!);
    return Response.json({ choices: [{ message: { content: "fixture" } }] });
  }, { preconnect: fetch.preconnect }));
  const client = new Client("", "private-key");
  const controller = new AbortController();
  for (const limit of [undefined, 8, 128]) {
    const request_limits = limit === undefined ? undefined : { input_bytes: 65536, batch: 1, output_tokens: limit };
    expect(await client.testChat({ id: "one", request_limits }, "  preserve this prompt  ", controller.signal)).toBe("fixture");
  }
  expect(calls.map((call) => JSON.parse(String(call.body)).max_tokens)).toEqual([1024, 8, 128]);
  for (const call of calls) {
    expect(JSON.parse(String(call.body)).messages).toEqual([{ role: "user", content: "  preserve this prompt  " }]);
    expect(call.signal).toBe(controller.signal);
    expect(call.body).not.toContain("private-key");
  }
});

test("malformed chat content becomes a static compatibility error instead of rendered object data", async () => {
  fetchSpy = spyOn(globalThis, "fetch").mockResolvedValue(Response.json({ choices: [{ message: { content: { private: "PRIVATE_UPSTREAM_MARKER" } } }] }));
  await expect(new Client("", "").testChat({ id: "one" }, "temporary")).rejects.toMatchObject({ code: "invalid_chat_response", status: 200 });
  try { await new Client("", "").testChat({ id: "one" }, "temporary"); } catch (error) { expect(String(error)).not.toContain("PRIVATE_UPSTREAM_MARKER"); }
});
