import { describe, expect, test } from "bun:test";
import { ago, baseUrl, checkText, parseAccess, parseCreated, parseHealth, snippets, summary, until, wyomingHost } from "../src/connectData";

const health = {
  schema_version: 1,
  protocols: [
    { id: "openai", title: "OpenAI", enabled: true, status: "healthy", models: 2, check: { ok: true, status: 200, route: "GET /v1/models", elapsed_ms: 1 }, last_request: { at_unix_ms: 1_000_000, status: 200 }, note: "Forwarded", operations: ["chat/completions"] },
    { id: "ollama", title: "Ollama", enabled: false, status: "disabled", models: 1, check: null, last_request: null },
    { id: "wyoming", title: "Wyoming", enabled: true, status: "error", models: 1, check: { ok: false, status: null, route: "TCP listener", message: "Address in use" }, address: null, listen: "0.0.0.0:10300", last_request: { at_unix_ms: 1_000_000, outcome: "model_unavailable" } },
  ],
};

describe("protocol health", () => {
  test("parses entries and summarizes their state", () => {
    const [openai, ollama, wyoming] = parseHealth(health);
    expect(openai!.operations).toEqual(["chat/completions"]);
    expect(summary(openai!, 1_000_000 + 120_000)).toBe("2 models · Last request 2 min ago");
    expect(checkText(openai!)).toBe("GET /v1/models → 200");
    expect(summary(ollama!)).toBe("");
    expect(checkText(ollama!)).toBe("Not run while off");
    expect(summary(wyoming!)).toBe("Self-check failed: Address in use");
    expect(() => parseHealth({ schema_version: 1, protocols: [{ id: "gopher", enabled: true, status: "idle", models: 0 }] })).toThrow();
    expect(() => parseHealth({ schema_version: 2, protocols: [] })).toThrow();
  });

  test("uses each client family's base URL", () => {
    const origin = "http://juiced.local:8080";
    expect(baseUrl({ id: "openai" }, origin, "juiced.local")).toBe(`${origin}/v1`);
    expect(baseUrl({ id: "anthropic" }, origin, "juiced.local")).toBe(origin);
    expect(baseUrl({ id: "ollama" }, origin, "juiced.local")).toBe(origin);
    expect(baseUrl({ id: "mcp" }, origin, "juiced.local")).toBe(`${origin}/mcp`);
    expect(baseUrl({ id: "realtime" }, "https://juiced.local", "juiced.local")).toBe("wss://juiced.local/v1/realtime?intent=transcription");
    expect(baseUrl({ id: "wyoming", listen: "0.0.0.0:10300", address: null }, origin, "juiced.local")).toBe("juiced.local:10300");
    expect(wyomingHost("[::1]:10300", null, "x")).toBe("::1:10300");
  });

  test("snippets name a key placeholder only when keys are enforced", () => {
    const open = snippets("openai", "http://h/v1", false);
    expect(open.map((item) => item.label)).toEqual(["curl", "Python", "JavaScript"]);
    expect(open[0]!.text).not.toContain("Authorization");
    expect(snippets("openai", "http://h/v1", true)[0]!.text).toContain("Authorization: Bearer YOUR_API_KEY");
    expect(JSON.parse(snippets("mcp", "http://h/mcp", true)[0]!.text).mcpServers.juiced.headers.Authorization).toBe("Bearer YOUR_API_KEY");
    expect(snippets("anthropic", "http://h", true)[1]!.text).toContain("x-api-key: YOUR_API_KEY");
    expect(snippets("wyoming", "h:1", true)).toEqual([]);
  });
});

describe("API keys", () => {
  const access = { schema_version: 1, mode: "keys", store_error: null, limits: { keys: 64, name_chars: 64, expiry_days: 3650 }, keys: [
    { id: "env:LEMONADE_API_KEY", name: "LEMONADE_API_KEY", scope: "inference", source: "environment" },
    { id: "key_1", name: "console", scope: "operator", source: "issued", prefix: "juiced_abcdef", created_at_ms: 1, expires_at_ms: null, expired: false, last_used_at_ms: 5 },
  ] };
  test("parses environment and issued keys", () => {
    const parsed = parseAccess(access);
    expect(parsed.keys[0]!.prefix).toBeNull();
    expect(parsed.keys[1]!.lastUsed).toBe(5);
    expect(() => parseAccess({ ...access, keys: [{ id: "x", name: "x", scope: "admin", source: "issued" }] })).toThrow();
  });
  test("requires the created secret", () => {
    expect(parseCreated({ ...access, created: { key: { id: "key_1" }, secret: "juiced_secret" } }).secret).toBe("juiced_secret");
    expect(() => parseCreated({ ...access, created: { key: { id: "key_1" } } })).toThrow();
  });
  test("formats times", () => {
    expect(ago(0, 30_000)).toBe("just now");
    expect(ago(0, 3 * 86_400_000)).toBe("3 d ago");
    expect(until(10, 20)).toBe("Expired");
    expect(until(30 * 86_400_000, 0)).toBe("Expires in 30 days");
  });
});
