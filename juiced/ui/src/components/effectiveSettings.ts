import { readArg, runtimeFields, withExtraArgs, type OptionResponse, type Options } from "./modelSettings";

export function effectiveRows(options: OptionResponse, draft: Options, running?: Options, extra?: string) {
  const desired: Options = { ...options.effective };
  for (const [key, value] of Object.entries(draft)) desired[key] = value ?? options.defaults[key];
  if (extra !== undefined) desired.llamacpp_args = withExtraArgs(String(desired.llamacpp_args || ""), extra);
  return [...new Set([...Object.keys(options.effective), ...Object.keys(draft), ...Object.keys(running || {})])]
    .filter((key) => !["model_name", "pinned"].includes(key))
    .sort((a, b) => Number(b === "ctx_size") - Number(a === "ctx_size") || a.localeCompare(b))
    .map((key) => ({ key, saved: options.effective[key], running: running?.[key], draft: desired[key],
      source: Object.prototype.hasOwnProperty.call(options.saved, key) ? "Saved override" : "Backend default",
      changed: JSON.stringify(options.effective[key]) !== JSON.stringify(desired[key]) }));
}

export function slotCapacity(options?: Options): { total: number; slots: number; perSlot: number } | null {
  if (!options || typeof options.ctx_size !== "number" || !Number.isSafeInteger(options.ctx_size) || options.ctx_size <= 0) return null;
  let slots = 1;
  try { slots = Number(readArg(String(options.llamacpp_args || ""), runtimeFields[0]) || 1); }
  catch { return null; }
  if (!Number.isSafeInteger(slots) || slots < 1) return null;
  return { total: options.ctx_size, slots, perSlot: Math.floor(options.ctx_size / slots) };
}

export function settingValue(value: unknown): string {
  if (value === undefined) return "Not reported";
  if (value === null) return "Automatic";
  if (value === "") return "Default";
  return typeof value === "object" ? JSON.stringify(value) : String(value);
}
