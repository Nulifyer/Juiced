/** Juiced's launch defaults for one llama.cpp model, as `GET /models/{id}/options` reports them. */
export type DefaultSource = "juiced" | "family" | "model-file" | "recipe";
export type DefaultState = "applied" | "overridden" | "shadowed" | "off";
export type DefaultEntry = { flag: string; value: string | null; source: DefaultSource; reason: string; state: DefaultState };
export type SamplerValue = { value: string | number; source: "saved" | "juiced" | "model-file" };
export type ModelDefaults = { available: boolean; enabled: boolean; entries: DefaultEntry[]; sampler: Record<string, SamplerValue | null> };

const sources: DefaultSource[] = ["juiced", "family", "model-file", "recipe"];
const states: DefaultState[] = ["applied", "overridden", "shadowed", "off"];
const record = (value: unknown): Record<string, unknown> => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};

export const sourceLabels: Record<DefaultSource, string> = { juiced: "Juiced", family: "Model family", "model-file": "Model file", recipe: "Tested recipe" };

export function parseModelDefaults(value: unknown): ModelDefaults | null {
  const body = record(value);
  if (typeof body.available !== "boolean" || typeof body.enabled !== "boolean" || !Array.isArray(body.entries)) return null;
  const entries = body.entries.flatMap((item): DefaultEntry[] => {
    const entry = record(item);
    if (typeof entry.flag !== "string" || !(entry.value === null || typeof entry.value === "string") || !sources.includes(entry.source as DefaultSource) || typeof entry.reason !== "string" || !states.includes(entry.state as DefaultState)) return [];
    return [entry as DefaultEntry];
  });
  const sampler = Object.fromEntries(Object.entries(record(body.sampler)).map(([key, item]) => {
    const value = record(item);
    const ok = (typeof value.value === "string" || typeof value.value === "number") && ["saved", "juiced", "model-file"].includes(value.source as string);
    return [key, ok ? value as SamplerValue : null];
  }));
  return { available: body.available, enabled: body.enabled, entries, sampler };
}

export function argument(entry: Pick<DefaultEntry, "flag" | "value">): string {
  return entry.value === null ? entry.flag : `${entry.flag} ${entry.value}`;
}

/** A stable name for what Juiced adds right now, to notice when a release changes it. */
export function appliedKey(defaults: ModelDefaults): string {
  return defaults.entries.filter((entry) => entry.state === "applied").map(argument).join("\n");
}

/** Arguments added since the operator last acknowledged the defaults on this client. */
export function changedSince(defaults: ModelDefaults, seen: string | null): string[] {
  if (!defaults.enabled || seen === appliedKey(defaults)) return [];
  const before = new Set((seen ?? "").split("\n").filter(Boolean));
  return defaults.entries.filter((entry) => entry.state === "applied" && !before.has(argument(entry)) && entry.source !== "juiced").map(argument);
}
