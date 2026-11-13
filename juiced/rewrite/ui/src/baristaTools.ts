import { ApiError, type Client } from "./api";
import { REQUEST_LIMIT_BYTES, requestBytes, tooLarge, type MessageContent } from "./chatImages";
import { bytes, policyNames, type Model, type Policy } from "./domain";

export type Permission = "full" | "ask" | "read_only";
export type BaristaTool = { name: string; title?: string; group?: string; description?: string; inputSchema?: unknown; annotations?: { readOnlyHint?: boolean; destructiveHint?: boolean }; requires_confirmation?: boolean; previewable?: boolean; status?: "working" | "partial" | "pending"; pending?: string };
export type PromptIndex = { version: number; role: string; rules: string[]; authority: Partial<Record<Permission, string>>; groups: { group: string; title: string; purpose: string; tools: string[] }[]; skills: { topic: string; title: string }[] };
export type BaristaSettings = { enabled: boolean; model: string | null; permission: Permission; revision: number; qualified: boolean; tools: BaristaTool[]; prompt?: PromptIndex; identity?: { sha256: string; prompt_version: number; tools: number } };
export type ToolCall = { id: string; name: string; arguments: string };
export type ChatMessage =
  | { role: "system"; content: string }
  | { role: "user"; content: MessageContent }
  | { role: "assistant"; content: string | null; tool_calls?: { id: string; type: "function"; function: { name: string; arguments: string } }[] }
  | { role: "tool"; tool_call_id: string; content: string };
export type Change = { target: string; field: string; before: unknown; after: unknown };
export type Job = { id: string; kind: string; label: string; state: string; done: boolean; progress: number | null; done_bytes?: number; total_bytes?: number | null; error?: string | null };
export type Envelope = { ok: boolean; tool?: string; summary?: string; data?: any; source?: unknown[]; changed?: Change[]; job?: Job; next?: string[]; warnings?: string[]; preview?: boolean; error?: { code?: string; message?: string; status?: number } };
export type ToolResult = { ok: boolean; text: string; value: Envelope };
export type Reply = { content: string; reasoning: string; toolCalls: ToolCall[]; finish?: string };

export const MAX_STEPS = 20;
const MAX_TOOL_TEXT = 16000;

export const permissionNames: Record<Permission, string> = { full: "Full control", ask: "Ask before changes", read_only: "Read only" };
export const permissionHelp: Record<Permission, string> = {
  full: "Barista makes changes without asking. Each one shows in the chat.",
  ask: "Barista shows each change first and waits for you to approve it.",
  read_only: "Barista can look but cannot change anything.",
};

export function readOnly(tool: BaristaTool | undefined): boolean {
  return tool?.annotations?.readOnlyHint === true;
}

/** Downloaded chat models that Barista can use, including Disabled ones that setup can make available. */
export function candidates(models: Model[]): Model[] {
  return models.filter((model) => model.downloaded && model.operations.includes("chat/completions")).sort((left, right) => Number(right.loaded) - Number(left.loaded) || left.id.localeCompare(right.id));
}

/** What choosing this model means for loading, in one line. */
export function implication(model: Model): string {
  const size = model.resident_bytes ? ` It reserves about ${bytes(model.resident_bytes)} while loaded.` : "";
  if (model.policy === "disabled") return `${model.id} is Disabled. Turning Barista on sets it to On demand.${size}`;
  if (model.policy === "hot") return `Hot, so Juiced keeps it loaded.${size}`;
  if (model.loaded) return `Loaded now. Barista uses it with its saved settings.${size}`;
  return `Loads with its saved settings when you send the first message.${size}`;
}

export function functionTools(tools: BaristaTool[]) {
  return tools.map((tool) => ({ type: "function" as const, function: { name: tool.name, description: tool.description ?? tool.title ?? tool.name, parameters: tool.inputSchema ?? { type: "object", properties: {} } } }));
}

const gib = (value: unknown) => typeof value === "number" ? `${(value / 1024 ** 3).toFixed(1)} GiB` : "unknown";

const fallbackAuthority: Record<Permission, string> = {
  full: "Full control. Your changes run without asking, so make only the change the operator asked for.",
  ask: "Ask before changes. The operator sees a preview of each change and approves or skips it before it runs.",
  read_only: "Read only. You can inspect Juiced but cannot change anything. Explain what the operator could do instead.",
};

/**
 * The system prompt is a small index: role, rules, tool groups and procedure topics from the
 * service, plus the stable machine summary. Schemas travel as tools; procedures load on demand.
 */
export function systemPrompt(model: string, permission: Permission, status: Record<string, any> | null, index?: PromptIndex): string {
  const lines = [
    `${index?.role ?? "You are Barista, the operator assistant built into Juiced. Your only job is to inspect and manage this Juiced service with the juiced_ tools."} You run on the model ${model}.`,
    `Permission: ${index?.authority?.[permission] ?? fallbackAuthority[permission]}`,
    ...(index?.rules ?? [
      "Read the current state with a tool before you change anything. Use exact model ids and revisions from the tools.",
      "Say a task is done only when a tool result shows it. A saved setting is not the running state, so check the model state after a change.",
      "Never invent model sizes, commands, benchmark results or cache hits. Say when you do not know.",
    ]),
  ];
  if (index?.groups?.length) lines.push(`Tools: ${index.groups.map((group) => `${group.title} (${group.purpose})`).join("; ")}.`);
  if (index?.skills?.length) lines.push(`Procedures for juiced_help: ${index.skills.map((skill) => `${skill.topic} (${skill.title})`).join(", ")}.`);
  if (status) {
    const gpus = Array.isArray(status.gpus) && status.gpus.length ? status.gpus.map((gpu: any) => [gpu.name ?? gpu.vendor ?? "GPU", gpu.addressable_bytes ?? gpu.vram_total_bytes, gpu.integrated ? "(integrated, shares system RAM)" : null].filter(Boolean).map((part: unknown) => typeof part === "number" ? gib(part) : part).join(" ")).join("; ") : "none detected";
    // Live figures stay out: a prompt that changes every turn forces the engine to prefill it again.
    lines.push(`Machine: ${status.host?.os ?? "unknown OS"} ${status.host?.architecture ?? ""}. CPU ${status.cpu?.name ?? "unknown"} with ${status.cpu?.logical_threads ?? "?"} threads. RAM ${gib(status.ram_bytes)}. GPUs: ${gpus}. Model memory budget ${gib(status.memory?.budget_bytes)}. Free memory, loaded models and jobs change; read them with juiced_status.`);
  }
  return lines.join("\n");
}

/** Folds one streamed Chat Completions chunk into the reply, including tool-call fragments. */
export function accumulate(reply: Reply, chunk: any): Reply {
  const choice = chunk?.choices?.[0];
  const delta = choice?.delta ?? choice?.message;
  const finish = typeof choice?.finish_reason === "string" ? choice.finish_reason : reply.finish;
  if (!delta) return finish === reply.finish ? reply : { ...reply, finish };
  const next: Reply = { content: reply.content, reasoning: reply.reasoning, toolCalls: reply.toolCalls.map((call) => ({ ...call })), ...(finish ? { finish } : {}) };
  if (typeof delta.content === "string") next.content += delta.content;
  if (typeof delta.reasoning_content === "string") next.reasoning += delta.reasoning_content;
  if (Array.isArray(delta.tool_calls)) {
    for (const [position, fragment] of delta.tool_calls.entries()) {
      const index = typeof fragment?.index === "number" ? fragment.index : position;
      const call = next.toolCalls[index] ??= { id: "", name: "", arguments: "" };
      if (typeof fragment?.id === "string") call.id = fragment.id;
      if (typeof fragment?.function?.name === "string") call.name += fragment.function.name;
      if (typeof fragment?.function?.arguments === "string") call.arguments += fragment.function.arguments;
      else if (fragment?.function?.arguments && typeof fragment.function.arguments === "object") call.arguments = JSON.stringify(fragment.function.arguments);
    }
  }
  return next;
}

/** Most output tokens one Barista reply may use. */
export const REPLY_TOKENS = 4096;

/**
 * Streams one reply. A thinking model can spend the whole output limit reasoning and stop
 * with no answer and no tool call; the turn is then asked once more with thinking off, so the
 * operator gets an answer instead of an empty reply.
 */
export async function streamReply(client: Client, model: string, messages: ChatMessage[], tools: BaristaTool[], maxTokens: number, onUpdate: (reply: Reply) => void, signal?: AbortSignal): Promise<Reply> {
  const body = { model, messages, tools: functionTools(tools), stream: true, max_tokens: maxTokens };
  const first = await streamOnce(client, body, onUpdate, signal);
  if (first.finish !== "length" || first.content.trim() || first.toolCalls.length) return first;
  const second = await streamOnce(client, { ...body, chat_template_kwargs: { enable_thinking: false } }, (state) => onUpdate({ ...state, reasoning: first.reasoning + state.reasoning }), signal);
  return { ...second, reasoning: first.reasoning + second.reasoning };
}

async function streamOnce(client: Client, body: Record<string, unknown>, onUpdate: (reply: Reply) => void, signal?: AbortSignal): Promise<Reply> {
  if (requestBytes(body) > REQUEST_LIMIT_BYTES) throw new ApiError(tooLarge, 413, "request_too_large");
  const response = await client.stream("chat/completions", body, signal);
  if (!response.ok || !response.body) {
    let message = `The model request failed (${response.status}).`;
    try { const value = await response.json(); message = value?.error?.message ?? message; } catch { /* Error bodies are optional. */ }
    throw new ApiError(message, response.status, "chat_failed");
  }
  let reply: Reply = { content: "", reasoning: "", toolCalls: [] };
  const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
  let buffer = "", received = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      received += value.length;
      if (received > 4 * 1024 * 1024) throw new ApiError("The reply exceeds the 4 MiB limit.", 200, "response_limit");
      buffer += value;
      let boundary;
      while ((boundary = buffer.search(/\r?\n\r?\n/)) >= 0) {
        const event = buffer.slice(0, boundary); buffer = buffer.slice(boundary).replace(/^\r?\n\r?\n/, "");
        for (const line of event.split(/\r?\n/)) {
          if (!line.startsWith("data:")) continue;
          const data = line.slice(5).trim();
          if (data === "[DONE]") continue;
          try { reply = accumulate(reply, JSON.parse(data)); } catch { continue; }
        }
        onUpdate(reply);
      }
    }
  } finally { reader.releaseLock(); }
  reply.toolCalls = reply.toolCalls.filter((call) => call.name).map((call, index) => ({ ...call, id: call.id || `call_${index}_${Date.now().toString(36)}` }));
  return reply;
}

/** Settings changes that end the current request instead of going back to the model. */
export const stopCodes = new Set(["assistant_disabled", "assistant_settings_changed", "operator_required", "unauthorized"]);

/** What the model reads: the envelope without route provenance, which only the console needs. */
export function modelText(envelope: Envelope): string {
  const { source: _source, ...rest } = envelope;
  return JSON.stringify(rest).slice(0, MAX_TOOL_TEXT);
}

export async function callTool(client: Client, name: string, args: unknown, revision: number, confirmed: boolean, signal?: AbortSignal): Promise<ToolResult> {
  try {
    const envelope = await client.request<Envelope>("juiced/barista/tools/call", { name, arguments: args, revision, confirmed }, signal);
    return { ok: envelope?.ok === true, text: modelText(envelope ?? { ok: false }), value: envelope ?? { ok: false } };
  } catch (error) {
    if (signal?.aborted) throw error;
    if (error instanceof ApiError && stopCodes.has(error.code)) throw error;
    const message = error instanceof Error ? error.message : "The tool call failed.";
    const code = error instanceof ApiError ? error.code : "tool_failed";
    const value: Envelope = { ok: false, tool: name, summary: message, error: { code, message } };
    return { ok: false, text: modelText(value), value };
  }
}

export function parseArguments(text: string): Record<string, unknown> | null {
  if (!text.trim()) return {};
  try { const value = JSON.parse(text); return value && typeof value === "object" && !Array.isArray(value) ? value : null; } catch { return null; }
}

const policyName = (value: unknown) => typeof value === "string" && value in policyNames ? policyNames[value as Policy] : String(value);
const quoted = (value: unknown) => typeof value === "string" ? value : JSON.stringify(value);
const on = (value: unknown) => value === true ? "on" : value === false ? "off" : quoted(value);

const reads: Record<string, (args: Record<string, unknown>) => string> = {
  juiced_status: () => "Checked service status",
  juiced_hardware: () => "Checked hardware",
  juiced_list_models: (args) => args.task ? `Checked ${quoted(args.task)} models` : "Checked models",
  juiced_model_details: (args) => `Checked ${quoted(args.model)}`,
  juiced_list_engines: () => "Checked engines",
  juiced_requests: (args) => args.model ? `Checked requests for ${quoted(args.model)}` : "Checked recent requests",
  juiced_logs: (args) => `Read ${quoted(args.source)} logs`,
  juiced_storage: () => "Checked storage",
  juiced_cache: (args) => args.model ? `Checked cache for ${quoted(args.model)}` : "Checked cache",
  juiced_connections: () => "Checked connections",
  juiced_search_models: (args) => args.query ? `Searched for "${quoted(args.query)}"` : "Checked recommended models",
  juiced_inspect_repo: (args) => `Inspected ${quoted(args.repo)}`,
  juiced_fit: (args) => `Estimated fit for ${quoted(args.model ?? `${quoted(args.repo)}${args.quant ? ` ${quoted(args.quant)}` : ""}`)}${args.context_tokens ? ` at ${quoted(args.context_tokens)} tokens` : ""}`,
  juiced_list_local_files: () => "Checked local files",
  juiced_wait_job: () => "Waited for a job",
  juiced_test_model: (args) => `Tested ${quoted(args.model)}${args.kind ? ` (${quoted(args.kind)})` : ""}`,
  juiced_help: (args) => args.topic ? `Read the ${quoted(args.topic)} procedure` : "Listed procedures",
  juiced_docs: (args) => args.page ? `Read docs ${quoted(args.page)}` : args.query ? `Searched docs for "${quoted(args.query)}"` : "Listed docs",
};

const changes: Record<string, (args: Record<string, unknown>) => string> = {
  juiced_set_policy: (args) => `Set ${quoted(args.model)} to ${policyName(args.policy)}`,
  juiced_load: (args) => `Load ${quoted(args.model)}`,
  juiced_unload: (args) => `Unload ${quoted(args.model)}`,
  juiced_set_model_settings: (args) => {
    const changed = args.changes && typeof args.changes === "object" ? Object.entries(args.changes as Record<string, unknown>).map(([key, value]) => `${key.replaceAll("_", " ")} ${value === null ? "automatic" : quoted(value)}`) : [];
    return `Change ${quoted(args.model)}${changed.length ? `: ${changed.join(", ")}` : ""}${args.apply === "reload" ? ", and reload it now" : ""}`;
  },
  juiced_set_cache: (args) => args.model ? `Set the cache for ${quoted(args.model)} to ${quoted(args.mode ?? "the global mode")}` : "Change cache settings",
  juiced_set_protocol: (args) => `Turn ${quoted(args.protocol)} ${on(args.enabled)}`,
  juiced_set_alias: (args) => args.target === null ? `Remove alias ${quoted(args.alias)}` : `Point ${quoted(args.alias)} at ${quoted(args.target)}`,
  juiced_set_wyoming: (args) => args.enabled === false ? "Turn Wyoming off" : "Configure Wyoming",
  juiced_create_api_key: (args) => `Create ${/^[aeiou]/i.test(quoted(args.scope)) ? "an" : "a"} ${quoted(args.scope)} key named ${quoted(args.name)}`,
  juiced_download: (args) => `Download ${args.quant ? `${quoted(args.quant)} from ` : args.file ? `${quoted(args.file)} from ` : ""}${quoted(args.repo)}`,
  juiced_add_model: (args) => `Add ${args.id ? quoted(args.id) : "a model"}${args.policy ? ` (${policyName(args.policy)})` : ""}`,
  juiced_install_engine: (args) => `Install ${quoted(args.engine)} for ${quoted(args.backend)}`,
  juiced_cancel_job: () => "Cancel a download",
  juiced_benchmark: (args) => `Benchmark ${quoted(args.model)}`,
  juiced_remove_model: (args) => `Remove ${quoted(args.model)}${args.delete_files ? " and its files" : ""}`,
  juiced_delete_files: (args) => { const count = Array.isArray(args.items) ? args.items.length : 0; return `Delete ${count} storage item${count === 1 ? "" : "s"}`; },
  juiced_forget_sessions: (args) => `Forget saved sessions${args.model ? ` for ${quoted(args.model)}` : args.all ? "" : ""}`,
  juiced_uninstall_engine: (args) => `Uninstall ${quoted(args.engine)} ${quoted(args.backend)}`,
  juiced_pause_service: (args) => args.paused ? "Pause new requests" : "Resume requests",
  juiced_restart_model: (args) => `Restart ${quoted(args.model)}`,
};

/** The arguments an approved change runs with: what the operator saw in its preview binds the apply, such as the storage revision a deletion was previewed at. */
export function approvedArgs(args: Record<string, unknown>, preview: ToolResult | null): Record<string, unknown> {
  const bind = preview?.ok ? preview.value?.data?.apply_with : undefined;
  if (!bind || typeof bind !== "object" || Array.isArray(bind)) return args;
  const bound = Object.fromEntries(Object.entries(bind as Record<string, unknown>).filter(([, value]) => typeof value === "string" || typeof value === "number"));
  return { ...args, ...bound };
}

/** A short line for a read, or the question on an approval card. */
export function describe(name: string, args: Record<string, unknown>): string {
  const read = reads[name];
  if (read) return read(args);
  const change = changes[name];
  return change ? `${change(args)}?` : name.replace(/^juiced_/, "").replaceAll("_", " ");
}

/** The action line for a change that runs without a prompt. */
export function action(name: string, args: Record<string, unknown>): string {
  return describe(name, args).replace(/\?$/, "");
}

/** One line telling the operator what a finished tool did. */
export function outcome(_name: string, _args: Record<string, unknown>, result: ToolResult): string {
  const value = result.value;
  if (!result.ok) return value?.error?.message ?? value?.summary ?? "The action failed.";
  return value?.summary ?? "Done.";
}

function shown(value: unknown): string {
  if (value === null || value === undefined) return "none";
  if (typeof value === "boolean") return value ? "on" : "off";
  if (typeof value === "string") return value in policyNames ? policyNames[value as Policy] : value;
  if (typeof value === "number") return Number.isInteger(value) ? value.toLocaleString("en-US") : String(value);
  return JSON.stringify(value);
}

/** Before and after for each changed field, for the compact change rows. */
export function changeLines(changed: Change[] | undefined): string[] {
  return (changed ?? []).slice(0, 6).map((change) => `${/^[a-z0-9_.]+$/.test(change.field) ? change.field.replaceAll("_", " ") : change.field}: ${shown(change.before)} → ${shown(change.after)}`);
}

export function jobLine(job: Job): string {
  const percent = typeof job.progress === "number" ? ` ${Math.round(job.progress * 100)}%` : "";
  const sizes = typeof job.total_bytes === "number" && job.total_bytes > 0 ? ` · ${bytes(job.done_bytes ?? 0)} of ${bytes(job.total_bytes)}` : "";
  if (job.state === "complete") return `${job.label}: finished${sizes}`;
  if (job.state === "failed") return `${job.label}: failed${job.error ? ` (${job.error})` : ""}`;
  return `${job.label}: ${job.state}${percent}${sizes}`;
}

function pullJob(pull: any): Job {
  const parts: any[] = Array.isArray(pull?.files) ? pull.files : [];
  const total = parts.reduce((sum, part) => sum + (typeof part.bytes === "number" ? part.bytes : 0), 0);
  const done = parts.reduce((sum, part) => sum + (part.state === "present" ? part.bytes ?? 0 : part.downloaded_bytes ?? 0), 0);
  const state = typeof pull?.state === "string" ? pull.state : "unknown";
  return { id: `pull:${pull?.pull}`, kind: "download", label: `${pull?.repo ?? "download"} ${pull?.label ?? pull?.file ?? ""}`.trim(), state, done: state === "complete" || state === "failed", progress: total > 0 ? done / total : null, done_bytes: done, total_bytes: total, error: pull?.error ?? null };
}

/** Reads a job straight from its native owner, so progress shows after the chat turn ends. */
export async function readJob(client: Client, id: string, signal?: AbortSignal): Promise<Job | null> {
  const [kind, native] = id.split(":", 2);
  if (!native || !/^[A-Za-z0-9_-]+$/.test(native)) return null;
  if (kind === "pull") return pullJob(await client.request(`juiced/library/pulls/${native}`, undefined, signal));
  if (kind === "engine") {
    const engines = await client.request<{ jobs?: any[] }>("juiced/engines", undefined, signal);
    const job = engines.jobs?.find((entry) => entry.id === native);
    if (!job) return null;
    const total = typeof job.total_bytes === "number" ? job.total_bytes : null;
    return { id, kind: "engine_install", label: `${job.engine} ${job.backend}`, state: job.state, done: job.state === "complete" || job.state === "failed", progress: total ? (job.downloaded_bytes ?? 0) / total : null, done_bytes: job.downloaded_bytes ?? 0, total_bytes: total, error: job.error ?? null };
  }
  return null;
}
