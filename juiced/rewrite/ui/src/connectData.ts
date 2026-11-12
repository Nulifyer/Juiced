export const protocolIds = ["openai", "anthropic", "ollama", "mcp", "automatic1111", "realtime", "wyoming"] as const;
export type ProtocolId = typeof protocolIds[number];
export type HealthStatus = "healthy" | "idle" | "no_models" | "disabled" | "error";
export type ProtocolCheck = { ok: boolean; status: number | null; route: string; elapsedMs: number | null; message: string | null };
export type ProtocolHealth = {
  id: ProtocolId; title: string; enabled: boolean; status: HealthStatus; models: number; check: ProtocolCheck | null;
  last: { at: number; status: number | null; outcome: string | null } | null; address: string | null; listen: string | null;
  support: string | null; note: string | null; operations: string[];
};
export type Scope = "read" | "inference" | "operator";
export type AccessKey = { id: string; name: string; scope: Scope; source: "issued" | "environment"; prefix: string | null; created: number | null; expires: number | null; expired: boolean; lastUsed: number | null };
export type Access = { mode: "trusted-lan" | "keys"; storeError: string | null; keys: AccessKey[]; limits: { keys: number; nameChars: number; expiryDays: number } };

const statuses: readonly HealthStatus[] = ["healthy", "idle", "no_models", "disabled", "error"];
const scopes: readonly Scope[] = ["read", "inference", "operator"];
const object = (value: unknown): Record<string, any> => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, any> : {};
const count = (value: unknown) => typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : null;
const text = (value: unknown) => typeof value === "string" ? value : null;

export function parseHealth(value: unknown): ProtocolHealth[] {
  const body = object(value);
  if (body.schema_version !== 1 || !Array.isArray(body.protocols)) throw new Error("The service returned unexpected protocol health. Check its version.");
  return body.protocols.map((raw: unknown) => {
    const entry = object(raw);
    if (!protocolIds.includes(entry.id) || typeof entry.enabled !== "boolean" || !statuses.includes(entry.status) || count(entry.models) === null) throw new Error("The service returned an invalid protocol health entry.");
    const check = entry.check === null || entry.check === undefined ? null : object(entry.check);
    if (check && typeof check.ok !== "boolean") throw new Error("The service returned an invalid protocol check.");
    const last = entry.last_request ? object(entry.last_request) : null;
    return {
      id: entry.id, title: text(entry.title) ?? entry.id, enabled: entry.enabled, status: entry.status, models: entry.models,
      check: check && { ok: check.ok, status: count(check.status), route: text(check.route) ?? "", elapsedMs: count(check.elapsed_ms), message: text(check.message) },
      last: last && count(last.at_unix_ms) !== null ? { at: last.at_unix_ms, status: count(last.status), outcome: text(last.outcome) } : null,
      address: text(entry.address), listen: text(entry.listen),
      support: text(entry.support), note: text(entry.note), operations: Array.isArray(entry.operations) ? entry.operations.filter((item: unknown) => typeof item === "string") : [],
    };
  });
}

export function parseAccess(value: unknown): Access {
  const body = object(value);
  if (body.schema_version !== 1 || (body.mode !== "trusted-lan" && body.mode !== "keys") || !Array.isArray(body.keys)) throw new Error("The service returned unexpected API key settings. Check its version.");
  const limits = object(body.limits);
  return {
    mode: body.mode, storeError: text(body.store_error),
    limits: { keys: count(limits.keys) ?? 64, nameChars: count(limits.name_chars) ?? 64, expiryDays: count(limits.expiry_days) ?? 3650 },
    keys: body.keys.map((raw: unknown) => {
      const key = object(raw);
      if (typeof key.id !== "string" || typeof key.name !== "string" || !scopes.includes(key.scope) || (key.source !== "issued" && key.source !== "environment")) throw new Error("The service returned an invalid API key entry.");
      return { id: key.id, name: key.name, scope: key.scope, source: key.source, prefix: text(key.prefix), created: count(key.created_at_ms), expires: count(key.expires_at_ms), expired: key.expired === true, lastUsed: count(key.last_used_at_ms) };
    }),
  };
}

export function parseCreated(value: unknown): { access: Access; secret: string; key: AccessKey } {
  const access = parseAccess(value);
  const created = object(object(value).created);
  const id = object(created.key).id;
  const key = access.keys.find((entry) => entry.id === id);
  if (typeof created.secret !== "string" || !created.secret || !key) throw new Error("The service did not return the new key. Refresh and check the key list.");
  return { access, secret: created.secret, key };
}

export function wyomingHost(listen: string, address: string | null, hostname: string) {
  const value = address ?? listen;
  const port = value.slice(value.lastIndexOf(":") + 1);
  const host = value.slice(0, value.lastIndexOf(":")).replace(/^\[|\]$/g, "");
  return `${["0.0.0.0", "::", ""].includes(host) ? hostname : host}:${port}`;
}

/** Each client family expects its own base: OpenAI SDKs include /v1, Anthropic, Ollama and AUTOMATIC1111 clients append their paths to the origin. */
export function baseUrl(protocol: ProtocolHealth | { id: ProtocolId; listen?: string | null; address?: string | null }, origin: string, hostname: string): string {
  switch (protocol.id) {
    case "openai": return `${origin}/v1`;
    case "mcp": return `${origin}/mcp`;
    case "realtime": return `${origin.replace(/^http/, "ws")}/v1/realtime?intent=transcription`;
    case "wyoming": return wyomingHost(protocol.listen ?? "0.0.0.0:10300", protocol.address ?? null, hostname);
    default: return origin;
  }
}

export type Snippet = { label: string; text: string };
export function snippets(id: ProtocolId, base: string, keyed: boolean): Snippet[] {
  const model = "YOUR_MODEL_ID";
  const key = keyed ? "YOUR_API_KEY" : "unused";
  const bearer = keyed ? ` \\\n  -H 'Authorization: Bearer ${key}'` : "";
  const json = (path: string, body: string, extra = "") => `curl ${base}${path}${bearer}${extra} \\\n  -H 'Content-Type: application/json' \\\n  -d '${body}'`;
  switch (id) {
    case "openai": return [
      { label: "curl", text: json("/chat/completions", `{"model":"${model}","messages":[{"role":"user","content":"Hello"}]}`) },
      { label: "Python", text: `from openai import OpenAI\n\nclient = OpenAI(base_url="${base}", api_key="${key}")\nreply = client.chat.completions.create(\n    model="${model}",\n    messages=[{"role": "user", "content": "Hello"}],\n)\nprint(reply.choices[0].message.content)` },
      { label: "JavaScript", text: `import OpenAI from "openai";\n\nconst client = new OpenAI({ baseURL: "${base}", apiKey: "${key}" });\nconst reply = await client.chat.completions.create({\n  model: "${model}",\n  messages: [{ role: "user", content: "Hello" }],\n});\nconsole.log(reply.choices[0].message.content);` },
    ];
    case "anthropic": return [
      { label: "Python", text: `from anthropic import Anthropic\n\nclient = Anthropic(base_url="${base}", api_key="${key}")\nreply = client.messages.create(\n    model="${model}",\n    max_tokens=256,\n    messages=[{"role": "user", "content": "Hello"}],\n)\nprint(reply.content[0].text)` },
      { label: "curl", text: `curl ${base}/v1/messages${keyed ? ` \\\n  -H 'x-api-key: ${key}'` : ""} \\\n  -H 'anthropic-version: 2023-06-01' \\\n  -H 'Content-Type: application/json' \\\n  -d '{"model":"${model}","max_tokens":256,"messages":[{"role":"user","content":"Hello"}]}'` },
    ];
    case "ollama": return [{ label: "curl", text: json("/api/chat", `{"model":"${model}","stream":false,"messages":[{"role":"user","content":"Hello"}]}`) }];
    case "mcp": return [{ label: "Client config", text: JSON.stringify({ mcpServers: { juiced: { type: "http", url: base, ...(keyed ? { headers: { Authorization: `Bearer ${key}` } } : {}) } } }, null, 2) }];
    case "automatic1111": return [{ label: "curl", text: json("/sdapi/v1/txt2img", `{"prompt":"a lighthouse at dusk","width":512,"height":512}`) }];
    case "realtime": return [{ label: "JavaScript", text: `const socket = new WebSocket(\n  "${base}",\n  ${keyed ? `["realtime", "openai-insecure-api-key.${key}"]` : `["realtime"]`},\n);` }];
    case "wyoming": return [];
  }
}

export const clientHints: Record<ProtocolId, string> = {
  openai: "Open WebUI, SillyTavern, Pi and other OpenAI clients: use this base URL.",
  anthropic: "Anthropic SDKs and Claude-compatible tools: use this base URL.",
  ollama: "Ollama clients: use this as the Ollama URL.",
  mcp: "MCP clients: add a Streamable HTTP server with this URL.",
  automatic1111: "SillyTavern and other AUTOMATIC1111 clients: use this as the server URL.",
  realtime: "Open a WebSocket to this URL for live transcription.",
  wyoming: "Home Assistant: add the Wyoming Protocol integration with this host and port.",
};

export function ago(ms: number, now = Date.now()) {
  const seconds = Math.max(0, Math.round((now - ms) / 1000));
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.round(seconds / 60)} min ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)} h ago`;
  return `${Math.round(seconds / 86400)} d ago`;
}

export function until(ms: number, now = Date.now()) {
  const days = Math.ceil((ms - now) / 86_400_000);
  if (ms <= now) return "Expired";
  return days <= 1 ? "Expires today" : `Expires in ${days} days`;
}

export function lastRequest(protocol: ProtocolHealth, now = Date.now()): string | null {
  const last = protocol.last;
  if (!last) return null;
  const failure = last.status && last.status >= 400 ? `answered ${last.status}` : last.outcome && last.outcome !== "completed" ? last.outcome.replaceAll("_", " ") : null;
  return `${ago(last.at, now)}${failure ? ` · ${failure}` : ""}`;
}

const needs: Record<ProtocolId, string> = { openai: "", anthropic: "chat", ollama: "chat or embedding", mcp: "chat or embedding", automatic1111: "image", realtime: "transcription", wyoming: "speech" };

export function summary(protocol: ProtocolHealth, now = Date.now()): string {
  const recent = lastRequest(protocol, now);
  const last = recent ? `Last request ${recent}` : "No requests yet";
  switch (protocol.status) {
    case "disabled": return "";
    case "error": return protocol.check ? `Self-check failed: ${checkText(protocol)}` : "Self-check failed";
    case "no_models": return `Needs an enabled ${needs[protocol.id] ? `${needs[protocol.id]} ` : ""}model`;
    default: return `${protocol.models} ${protocol.models === 1 ? "model" : "models"} · ${last}`;
  }
}

export function checkText(protocol: ProtocolHealth): string {
  const check = protocol.check;
  if (!check) return protocol.enabled ? "Not available" : "Not run while off";
  if (!check.ok) return check.message ?? `${check.route} ${check.status ? `answered ${check.status}` : "did not answer"}`;
  return protocol.id === "wyoming" ? "Listening" : `${check.route} → ${check.status ?? "ok"}`;
}

export const statusLabels: Record<HealthStatus, string> = { healthy: "Healthy", idle: "Idle", no_models: "No models", disabled: "Off", error: "Error" };
export const statusTones: Record<HealthStatus, "good" | "warning" | "bad" | "quiet"> = { healthy: "good", idle: "quiet", no_models: "warning", disabled: "quiet", error: "bad" };
export const scopeLabels: Record<Scope, string> = { read: "Read only", inference: "Inference", operator: "Operator" };
