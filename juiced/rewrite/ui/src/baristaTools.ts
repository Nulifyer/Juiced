import { ApiError, type Client } from "./api";
import { bytes, policyNames, type Model, type Policy } from "./domain";

export type Permission = "full" | "ask" | "read_only";
export type BaristaTool = { name: string; title?: string; description?: string; inputSchema?: unknown; annotations?: { readOnlyHint?: boolean; destructiveHint?: boolean }; requires_confirmation?: boolean };
export type BaristaSettings = { enabled: boolean; model: string | null; permission: Permission; revision: number; qualified: boolean; tools: BaristaTool[] };
export type ToolCall = { id: string; name: string; arguments: string };
export type ChatMessage =
  | { role: "system" | "user"; content: string }
  | { role: "assistant"; content: string | null; tool_calls?: { id: string; type: "function"; function: { name: string; arguments: string } }[] }
  | { role: "tool"; tool_call_id: string; content: string };
export type ToolResult = { ok: boolean; text: string; value: unknown };
export type Reply = { content: string; reasoning: string; toolCalls: ToolCall[] };

export const MAX_STEPS = 8;
const MAX_TOOL_TEXT = 16000;

export const permissionNames: Record<Permission, string> = { full: "Full control", ask: "Ask before changes", read_only: "Read only" };
export const permissionHelp: Record<Permission, string> = {
  full: "Barista makes changes without asking. Each one shows in the chat.",
  ask: "Barista waits for you to approve each change.",
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

const authority: Record<Permission, string> = {
  full: "Full control. Your changes run without asking, so make only the change the operator asked for.",
  ask: "Ask before changes. The operator approves or skips each change before it runs.",
  read_only: "Read only. You can inspect Juiced but cannot change anything. Explain what the operator could do instead.",
};

export function systemPrompt(model: string, permission: Permission, status: Record<string, any> | null): string {
  const lines = [
    `You are Barista, the operator assistant built into Juiced. Your only job is to inspect and manage this Juiced service with the juiced_ tools. You run on the model ${model}.`,
    `Permission: ${authority[permission]}`,
    "Read the current state with a tool before you change anything. Use exact model ids and revisions from the tools. After a revision_conflict, read again and retry once.",
    "Say a task is done only when a tool result shows it. A saved setting is not the running state, so check the model state after a change.",
    "Never invent model sizes, commands, benchmark results or cache hits. Say when you do not know.",
    "Ask the operator only when a choice that matters is still open. Do not ask for facts a tool can give you. Keep replies short and plain.",
  ];
  if (status) {
    const cpu = status.cpu ?? {};
    const gpus = Array.isArray(status.gpus) && status.gpus.length ? status.gpus.map((gpu: any) => [gpu.name ?? gpu.vendor ?? "GPU", gpu.vram_total_bytes].filter(Boolean).map((part: unknown) => typeof part === "number" ? gib(part) : part).join(" ")).join("; ") : "none detected";
    const backends = Object.entries(status.recommended_backends ?? {}).map(([engine, backend]) => `${engine}: ${backend}`).join(", ") || "none";
    lines.push(`Machine: ${status.host?.os ?? "unknown OS"} ${status.host?.architecture ?? ""}. CPU ${cpu.name ?? "unknown"} with ${cpu.logical_threads ?? "?"} threads. RAM ${gib(status.ram?.total_bytes)} total, ${gib(status.ram?.available_bytes)} available. GPUs: ${gpus}.`);
    lines.push(`Model memory budget ${gib(status.memory?.resident_budget_bytes)}, ${gib(status.memory?.reserved_bytes)} reserved now. Recommended backends: ${backends}. ${Array.isArray(status.loaded_models) ? status.loaded_models.length : 0} models loaded.`);
  }
  return lines.join("\n");
}

/** Folds one streamed Chat Completions chunk into the reply, including tool-call fragments. */
export function accumulate(reply: Reply, chunk: any): Reply {
  const delta = chunk?.choices?.[0]?.delta ?? chunk?.choices?.[0]?.message;
  if (!delta) return reply;
  const next: Reply = { content: reply.content, reasoning: reply.reasoning, toolCalls: reply.toolCalls.map((call) => ({ ...call })) };
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

export async function streamReply(client: Client, model: string, messages: ChatMessage[], tools: BaristaTool[], maxTokens: number, onUpdate: (reply: Reply) => void, signal?: AbortSignal): Promise<Reply> {
  const response = await client.stream("chat/completions", { model, messages, tools: functionTools(tools), stream: true, max_tokens: maxTokens }, signal);
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

export async function callTool(client: Client, name: string, args: unknown, revision: number, confirmed: boolean, signal?: AbortSignal): Promise<ToolResult> {
  try {
    const reply = await client.request<{ ok: boolean; result: unknown }>("juiced/barista/tools/call", { name, arguments: args, revision, confirmed }, signal);
    return { ok: reply.ok === true, text: JSON.stringify(reply.result ?? null).slice(0, MAX_TOOL_TEXT), value: reply.result ?? null };
  } catch (error) {
    if (signal?.aborted) throw error;
    if (error instanceof ApiError && stopCodes.has(error.code)) throw error;
    const message = error instanceof Error ? error.message : "The tool call failed.";
    const code = error instanceof ApiError ? error.code : "tool_failed";
    return { ok: false, text: JSON.stringify({ error: { code, message } }), value: { error: { code, message } } };
  }
}

export function parseArguments(text: string): Record<string, unknown> | null {
  if (!text.trim()) return {};
  try { const value = JSON.parse(text); return value && typeof value === "object" && !Array.isArray(value) ? value : null; } catch { return null; }
}

const policyName = (value: unknown) => typeof value === "string" && value in policyNames ? policyNames[value as Policy] : String(value);
const quoted = (value: unknown) => typeof value === "string" ? value : JSON.stringify(value);

/** A short line for a read, or the question on an approval card. */
export function describe(name: string, args: Record<string, unknown>): string {
  const model = quoted(args.model ?? "");
  switch (name) {
    case "juiced_status": return "Checked service status";
    case "juiced_list_models": return "Checked models";
    case "juiced_model_settings": return `Checked settings for ${model}`;
    case "juiced_list_engines": return "Checked engines";
    case "juiced_search_models": return args.query ? `Searched for "${quoted(args.query)}"` : "Checked recommended models";
    case "juiced_list_local_files": return "Checked local files";
    case "juiced_list_downloads": return "Checked downloads";
    case "juiced_set_policy": return `Set ${model} to ${policyName(args.policy)}?`;
    case "juiced_load_model": return `Load ${model}?`;
    case "juiced_unload_model": return `Unload ${model}?`;
    case "juiced_set_model_settings": {
      const changes = ["context_tokens", "gpu_layers", "threads", "parallel_slots"].filter((key) => key in args).map((key) => `${key.replace("_", " ")} ${args[key] === null ? "automatic" : quoted(args[key])}`);
      return `Change ${model}${changes.length ? `: ${changes.join(", ")}` : ""}${args.apply === "reload" ? ", and reload it now" : ""}?`;
    }
    case "juiced_download_model": return `Download ${args.file ? `${quoted(args.file)} from ` : ""}${quoted(args.repo ?? "")}?`;
    case "juiced_add_model": return `Add ${quoted(args.path ?? args.artifact ?? args.artifact_set ?? "")} as ${args.id ? quoted(args.id) : "a model"}${args.policy ? ` (${policyName(args.policy)})` : ""}?`;
    case "juiced_install_engine": return `Install ${quoted(args.engine ?? "")} for ${quoted(args.backend ?? "")}?`;
    default: return name.replace(/^juiced_/, "").replaceAll("_", " ");
  }
}

/** The action line for a change that runs without a prompt. */
export function action(name: string, args: Record<string, unknown>): string {
  return describe(name, args).replace(/\?$/, "");
}

/** One line telling the operator what a finished change did. */
export function outcome(name: string, args: Record<string, unknown>, result: ToolResult): string {
  if (!result.ok) {
    const error = (result.value as { error?: { message?: string; code?: string } } | null)?.error;
    return error?.message ?? "The action failed.";
  }
  const model = quoted(args.model ?? "");
  switch (name) {
    case "juiced_set_policy": return `${model} is now ${policyName(args.policy)}.`;
    case "juiced_load_model": return `${model} is loaded.`;
    case "juiced_unload_model": return `${model} is unloaded.`;
    case "juiced_set_model_settings": return `Saved settings for ${model}.`;
    case "juiced_download_model": return "Download started. Models, Downloads shows its progress.";
    case "juiced_add_model": return "Model added.";
    case "juiced_install_engine": return "Engine install started.";
    default: return "Done.";
  }
}
