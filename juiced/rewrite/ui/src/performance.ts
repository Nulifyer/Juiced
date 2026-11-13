import type { RequestRecord } from "./domain";

export type Timing = { queue: number; prompt: number; generate: number; other: number; total: number };
export type ModelPerformance = {
  model: string; count: number; failed: number; medianMs: number | null;
  promptRate: number | null; decodeRate: number | null; cachedShare: number | null;
  reusedPromptMs: number | null; fullPromptMs: number | null; decodeTrend: number[];
};

export const pathNames: Record<string, string> = { "slot-hit": "Still loaded", restored: "Restored from disk", cold: "Processed in full", bypass: "Not cached" };

export function failed(record: RequestRecord): boolean {
  return (record.outcome !== "completed" && record.outcome !== "cancelled") || (record.status ?? 0) >= 400;
}

function processedTokens(record: RequestRecord): number | null {
  if (record.prompt_processed_tokens != null) return record.prompt_processed_tokens;
  if (record.input_tokens == null) return null;
  return Math.max(0, record.input_tokens - (record.cached_tokens ?? 0));
}

/** Splits elapsed time using the engine's own token rates; the remainder covers transport, restores and non-token work. */
export function timing(record: RequestRecord): Timing {
  const total = Math.max(0, record.elapsed_ms);
  const queue = Math.min(total, Math.max(0, record.queue_wait_ms ?? 0));
  const processed = processedTokens(record);
  let prompt = processed != null && record.prompt_tokens_per_second ? processed / record.prompt_tokens_per_second * 1000 : 0;
  let generate = record.output_tokens != null && record.decode_tokens_per_second ? record.output_tokens / record.decode_tokens_per_second * 1000 : 0;
  const room = total - queue, engine = prompt + generate;
  if (engine > room && engine > 0) { prompt *= room / engine; generate *= room / engine; }
  return { queue, prompt, generate, other: Math.max(0, total - queue - prompt - generate), total };
}

export function median(values: number[]): number | null {
  if (!values.length) return null;
  const sorted = [...values].sort((left, right) => left - right);
  const middle = sorted.length >> 1;
  return sorted.length % 2 ? sorted[middle]! : (sorted[middle - 1]! + sorted[middle]!) / 2;
}

const known = (values: (number | null | undefined)[]) => values.filter((value): value is number => value != null && Number.isFinite(value));

/** Records arrive newest first; trends read oldest to newest. */
export function modelPerformance(records: RequestRecord[]): ModelPerformance[] {
  const groups = new Map<string, RequestRecord[]>();
  for (const record of records) groups.set(record.model, [...groups.get(record.model) ?? [], record]);
  return [...groups].map(([model, all]) => {
    const ok = all.filter((record) => !failed(record) && record.outcome === "completed");
    const promptMs = (paths: string[]) => median(ok.filter((record) => record.session_cache && paths.includes(record.session_cache.path)).map((record) => timing(record).prompt));
    const cachedInput = ok.filter((record) => record.cached_tokens != null && record.input_tokens);
    const input = cachedInput.reduce((sum, record) => sum + record.input_tokens!, 0);
    return {
      model, count: all.length, failed: all.filter(failed).length, medianMs: median(ok.map((record) => record.elapsed_ms)),
      promptRate: median(known(ok.map((record) => record.prompt_tokens_per_second))),
      decodeRate: median(known(ok.map((record) => record.decode_tokens_per_second))),
      cachedShare: input ? cachedInput.reduce((sum, record) => sum + record.cached_tokens!, 0) / input : null,
      reusedPromptMs: promptMs(["slot-hit", "restored"]), fullPromptMs: promptMs(["cold", "bypass"]),
      decodeTrend: known(ok.map((record) => record.decode_tokens_per_second)).reverse(),
    };
  });
}

export function formatMs(ms: number | null): string {
  if (ms == null) return "—";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
}

export const count = (value: number | null | undefined) => value == null ? "—" : value.toLocaleString("en-US");
