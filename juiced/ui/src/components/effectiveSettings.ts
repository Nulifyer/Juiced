import { withExtraArgs, type OptionResponse, type Options } from "./modelSettings";
import type { ContextObservation, Model } from "../api";

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

export function observedCapacity(observation?: ContextObservation) {
  const positive = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value > 0;
  if (!observation || !positive(observation.per_request_tokens)) return null;
  return { perSlot: observation.per_request_tokens, slots: positive(observation.slots) ? observation.slots : null };
}

export function discoveryContextLabel(model: Pick<Model, "context" | "context_length">): string | null {
  const value = model.context?.advertised_tokens ?? model.context_length;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) return null;
  const source = model.context?.advertised_source;
  const label = source === "runtime_observation" ? "tokens per chat" : source === "configured_options" ? "configured context tokens"
    : source === "applied_options" ? "context tokens requested at load" : source === "model_metadata" || source === "provider_metadata" ? "declared context tokens" : "reported context tokens";
  return value.toLocaleString() + " " + label;
}

export function settingValue(value: unknown): string {
  if (value === undefined) return "Not reported";
  if (value === null) return "Automatic";
  if (value === "") return "Default";
  return typeof value === "object" ? JSON.stringify(value) : String(value);
}
