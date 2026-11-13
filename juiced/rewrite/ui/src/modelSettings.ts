export type TunedKey = "temperature" | "top_p" | "top_k" | "min_p" | "repeat_penalty" | "reasoning_budget" | "batch_threads" | "batch_size" | "ubatch_size" | "flash_attention" | "cache_type_k" | "cache_type_v";
export type Field = { key: TunedKey; label: string; help: string; kind: "count" | "signed" | "decimal" | "choice"; min: number; max: number; step?: number; choices?: string[]; advanced: boolean };
export type TunedDraft = Record<TunedKey, string> & { extra: string };
export type TunedOptions = Record<TunedKey, number | string | null>;

const cacheTypes = ["f16", "bf16", "f32", "q8_0", "q5_1", "q5_0", "q4_1", "q4_0", "iq4_nl"];
export const tunedFields: Field[] = [
  { key: "temperature", label: "Temperature", help: "Higher values give more varied replies.", kind: "decimal", min: 0, max: 100, step: 0.05, advanced: false },
  { key: "top_p", label: "Top P", help: "Keeps the most likely tokens that add up to this probability.", kind: "decimal", min: 0, max: 1, step: 0.05, advanced: false },
  { key: "top_k", label: "Top K", help: "Picks from this many candidate tokens. 0 turns the limit off.", kind: "count", min: 0, max: 2147483647, advanced: false },
  { key: "min_p", label: "Min P", help: "Drops tokens less likely than this share of the best token.", kind: "decimal", min: 0, max: 1, step: 0.01, advanced: false },
  { key: "repeat_penalty", label: "Repetition penalty", help: "1 leaves repetition unchanged. Higher values discourage repeats.", kind: "decimal", min: 0, max: 100, step: 0.05, advanced: false },
  { key: "reasoning_budget", label: "Reasoning budget", help: "Tokens a thinking model may spend reasoning. 0 turns thinking off; -1 is unlimited.", kind: "signed", min: -1, max: 2147483647, advanced: false },
  { key: "batch_threads", label: "Prompt threads", help: "CPU threads used to read the prompt. Empty uses the generation threads.", kind: "count", min: 1, max: 65536, advanced: true },
  { key: "batch_size", label: "Batch", help: "Prompt tokens sent to the engine per step.", kind: "count", min: 1, max: 1048576, advanced: true },
  { key: "ubatch_size", label: "Physical batch", help: "Prompt tokens processed at once. Lower it to reduce peak memory.", kind: "count", min: 1, max: 1048576, advanced: true },
  { key: "flash_attention", label: "Flash attention", help: "Faster attention where the backend supports it. Quantized V caches need it.", kind: "choice", min: 0, max: 0, choices: ["auto", "on", "off"], advanced: true },
  { key: "cache_type_k", label: "K cache", help: "Precision of the key cache. Quantized caches use less memory and may lower quality.", kind: "choice", min: 0, max: 0, choices: cacheTypes, advanced: true },
  { key: "cache_type_v", label: "V cache", help: "Precision of the value cache. Quantized V caches need flash attention.", kind: "choice", min: 0, max: 0, choices: cacheTypes, advanced: true },
];
const keys = tunedFields.map((field) => field.key);

const text = (value: unknown) => typeof value === "number" ? String(value) : typeof value === "string" ? value : "";
const record = (value: unknown): Record<string, unknown> => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};

export function tunedDraft(saved: unknown, extra: readonly string[]): TunedDraft {
  const values = record(saved);
  return { ...Object.fromEntries(keys.map((key) => [key, text(values[key])])) as Record<TunedKey, string>, extra: joinArgs(extra) };
}

export function sameTuned(left: TunedDraft, right: TunedDraft): boolean {
  return left.extra === right.extra && keys.every((key) => left[key] === right[key]);
}

export function tunedOptions(draft: TunedDraft): TunedOptions {
  return Object.fromEntries(tunedFields.map((field) => {
    const value = draft[field.key].trim();
    if (!value) return [field.key, null];
    if (field.kind === "choice") {
      if (!field.choices!.includes(value)) throw new Error(`${field.label} must be one of ${field.choices!.join(", ")}.`);
      return [field.key, value];
    }
    const number = Number(value);
    const whole = field.kind !== "decimal";
    if (!/^-?(\d+\.?\d*|\.\d+)$/.test(value) || !Number.isFinite(number) || (whole && !Number.isSafeInteger(number)) || number < field.min || number > field.max)
      throw new Error(`${field.label} must be ${whole ? "a whole number" : "a number"} from ${field.min} to ${field.max}, or empty for the default.`);
    return [field.key, number];
  })) as TunedOptions;
}

/** What an empty field falls back to: the value the profile already passes, else the engine default. */
export function fallback(key: TunedKey, effective: unknown, defaults: unknown): string {
  const current = record(effective)[key], engine = record(defaults)[key];
  if (current !== undefined && current !== null) return `${text(current)} (profile)`;
  if (engine !== undefined && engine !== null) return `Default ${text(engine)}`;
  return "Engine default";
}

// Argument text uses shell-style quoting but is never evaluated by a shell.
export function splitArgs(source: string): string[] {
  const tokens: string[] = [];
  let token = "", quote = "", escaped = false, started = false;
  for (const character of source) {
    if (escaped) { token += character; escaped = false; started = true; }
    else if (character === "\\" && quote !== "'") { escaped = true; started = true; }
    else if (quote) { if (character === quote) quote = ""; else token += character; }
    else if (character === "'" || character === '"') { quote = character; started = true; }
    else if (/\s/.test(character)) { if (started) { tokens.push(token); token = ""; started = false; } }
    else { token += character; started = true; }
  }
  if (escaped || quote) throw new Error("Close the quote in the extra arguments before saving.");
  if (started) tokens.push(token);
  return tokens;
}

export function joinArgs(tokens: readonly string[]): string {
  return tokens.map((token) => /^[A-Za-z0-9_./:=,+@%-]+$/.test(token) ? token : `'${token.replace(/'/g, `'\\''`)}'`).join(" ");
}

export type EngineField = { key: string; label: string; help: string; kind: "count" | "signed" | "decimal" | "name"; min: number; max: number; step?: number };
export const imageFields: EngineField[] = [
  { key: "steps", label: "Steps", help: "Denoising steps per image. Turbo models need few.", kind: "count", min: 1, max: 150 },
  { key: "cfg_scale", label: "CFG scale", help: "How closely images follow the prompt.", kind: "decimal", min: 0, max: 50, step: 0.5 },
  { key: "width", label: "Width", help: "Pixels, a multiple of 8 from 64 to 2048.", kind: "count", min: 64, max: 2048 },
  { key: "height", label: "Height", help: "Pixels, a multiple of 8 from 64 to 2048.", kind: "count", min: 64, max: 2048 },
  { key: "sampling_method", label: "Sampler", help: "Sampler name, such as euler or dpm++2m.", kind: "name", min: 0, max: 0 },
  { key: "scheduler", label: "Scheduler", help: "Noise schedule, such as karras or simple.", kind: "name", min: 0, max: 0 },
  { key: "flow_shift", label: "Flow shift", help: "Timestep shift for flow models.", kind: "decimal", min: 0, max: 100, step: 0.5 },
  { key: "seed", label: "Seed", help: "-1 picks a new random seed for each image.", kind: "signed", min: -1, max: 4294967295 },
];
export const asrFields: EngineField[] = [
  { key: "threads", label: "Threads", help: "CPU threads for transcription.", kind: "count", min: 1, max: 65536 },
  { key: "commit", label: "Commit", help: "Streaming commit window in frames.", kind: "count", min: 1, max: 65536 },
  { key: "right", label: "Right context", help: "Look-ahead frames per streaming step.", kind: "count", min: 1, max: 65536 },
];

export function engineDraft(fields: EngineField[], values: unknown): Record<string, string> {
  const current = record(values);
  return Object.fromEntries(fields.map((field) => [field.key, text(current[field.key])]));
}

/** A merge patch with only the changed keys; empty clears a key back to the engine default. */
export function enginePatch(fields: EngineField[], saved: Record<string, string>, draft: Record<string, string>): Record<string, number | string | null> {
  const patch: Record<string, number | string | null> = {};
  for (const field of fields) {
    const value = (draft[field.key] ?? "").trim();
    if (value === (saved[field.key] ?? "").trim()) continue;
    if (!value) { patch[field.key] = null; continue; }
    if (field.kind === "name") {
      if (!/^[A-Za-z0-9_+.-]{1,32}$/.test(value)) throw new Error(`${field.label} must be a short name of letters, digits and _ + . -.`);
      patch[field.key] = value; continue;
    }
    const number = Number(value);
    const whole = field.kind !== "decimal";
    if (!Number.isFinite(number) || (whole && !Number.isSafeInteger(number)) || number < field.min || number > field.max || ((field.key === "width" || field.key === "height") && number % 8 !== 0))
      throw new Error(`${field.label} must be ${whole ? "a whole number" : "a number"} from ${field.min} to ${field.max}${field.key === "width" || field.key === "height" ? ", a multiple of 8" : ""}.`);
    patch[field.key] = number;
  }
  return patch;
}
