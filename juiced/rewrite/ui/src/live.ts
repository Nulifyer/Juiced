import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";

export type GpuStats = { id: string; name: string | null; vendor: string | null; gpu_percent: number | null; memory_used_bytes: number | null; memory_total_bytes: number | null };
export type LastInference = { model: string; operation: string; decode_tokens_per_second: number | null; prompt_tokens_per_second: number | null; input_tokens: number | null; output_tokens: number | null; elapsed_ms: number | null; completed_at_ms: number };
export type SystemStats = {
  supported: boolean; sampled_at_ms: number | null; cpu_percent: number | null;
  memory: { total_bytes: number | null; used_bytes: number | null } | null;
  gpus: GpuStats[]; npu_percent: number | null; active_requests: number | null; queued_requests: number | null;
  last_inference: LastInference | null;
};
export type LogLine = { seq: number; timestamp_ms: number; source: string; model: string | null; stream: "service" | "stdout" | "stderr"; level: "error" | "warning" | "info" | "debug" | null; text: string; truncated: boolean };
export type LogPage = { data: LogLine[]; next: number; dropped: number };

const number = (value: unknown): number | null => typeof value === "number" && Number.isFinite(value) ? value : null;
const text = (value: unknown): string | null => typeof value === "string" ? value : null;
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The service returned an invalid response.");
  return value as Record<string, unknown>;
}

export function parseSystemStats(value: unknown): SystemStats {
  const item = record(value);
  const memory = item.memory && typeof item.memory === "object" ? record(item.memory) : null;
  const last = item.last_inference && typeof item.last_inference === "object" ? record(item.last_inference) : null;
  return {
    supported: item.supported === true, sampled_at_ms: number(item.sampled_at_ms), cpu_percent: number(item.cpu_percent),
    memory: memory && { total_bytes: number(memory.total_bytes), used_bytes: number(memory.used_bytes) },
    gpus: Array.isArray(item.gpus) ? item.gpus.slice(0, 16).map((gpu) => { const entry = record(gpu); return { id: text(entry.id) ?? "gpu", name: text(entry.name), vendor: text(entry.vendor), gpu_percent: number(entry.gpu_percent), memory_used_bytes: number(entry.memory_used_bytes), memory_total_bytes: number(entry.memory_total_bytes) }; }) : [],
    npu_percent: number(item.npu_percent), active_requests: number(item.active_requests), queued_requests: number(item.queued_requests),
    last_inference: last && typeof last.model === "string" ? { model: last.model, operation: text(last.operation) ?? "", decode_tokens_per_second: number(last.decode_tokens_per_second), prompt_tokens_per_second: number(last.prompt_tokens_per_second), input_tokens: number(last.input_tokens), output_tokens: number(last.output_tokens), elapsed_ms: number(last.elapsed_ms), completed_at_ms: number(last.completed_at_ms) ?? 0 } : null,
  };
}

export function parseLogPage(value: unknown): LogPage {
  const item = record(value);
  if (!Array.isArray(item.data)) throw new Error("The service returned an invalid log page.");
  const levels = ["error", "warning", "info", "debug"];
  return {
    next: number(item.next) ?? 0, dropped: number(item.dropped) ?? 0,
    data: item.data.slice(0, 500).map((line) => { const entry = record(line); return { seq: number(entry.seq) ?? 0, timestamp_ms: number(entry.timestamp_ms) ?? 0, source: text(entry.source) ?? "service", model: text(entry.model), stream: entry.stream === "stdout" || entry.stream === "stderr" ? entry.stream : "service", level: levels.includes(entry.level as string) ? entry.level as LogLine["level"] : null, text: text(entry.text) ?? "", truncated: entry.truncated === true }; }),
  };
}

export type StatsPoint = { at: number; cpu: number | null; memory: number | null; gpu: number | null; vram: number | null; decode: number | null };
const HISTORY = 60;

export function useSystemStats(client: Client, enabled: boolean) {
  const [stats, setStats] = useState<SystemStats | null>(null);
  const [history, setHistory] = useState<StatsPoint[]>([]);
  const [error, setError] = useState("");
  const busy = useRef(false);
  useEffect(() => {
    setStats(null); setHistory([]); setError("");
    if (!enabled) return;
    const controller = new AbortController();
    const poll = async () => {
      if (busy.current) return;
      busy.current = true;
      try {
        const next = parseSystemStats(await client.request("system-stats", undefined, controller.signal));
        if (controller.signal.aborted) return;
        setStats(next); setError("");
        const gpu = next.gpus[0];
        const share = (used: number | null | undefined, total: number | null | undefined) => used != null && total ? used / total * 100 : null;
        setHistory((points) => [...points.slice(-(HISTORY - 1)), { at: next.sampled_at_ms ?? Date.now(), cpu: next.cpu_percent, memory: share(next.memory?.used_bytes, next.memory?.total_bytes), gpu: gpu?.gpu_percent ?? null, vram: share(gpu?.memory_used_bytes, gpu?.memory_total_bytes), decode: next.last_inference?.decode_tokens_per_second ?? null }]);
      } catch (failure) {
        if (!controller.signal.aborted) setError(failure instanceof Error ? failure.message : "Live stats are unavailable.");
      } finally { busy.current = false; }
    };
    void poll();
    const timer = setInterval(() => void poll(), 2000);
    return () => { controller.abort(); clearInterval(timer); busy.current = false; };
  }, [client, enabled]);
  return { stats, history, error };
}

export function percent(value: number | null | undefined): string {
  return value == null ? "—" : `${value < 10 ? value.toFixed(1) : Math.round(value)}%`;
}
export function rate(value: number | null | undefined): string {
  return value == null ? "—" : `${value < 10 ? value.toFixed(1) : Math.round(value)} tok/s`;
}
export function gib(value: number | null | undefined): string {
  return value == null ? "—" : `${(value / 1024 ** 3).toFixed(value >= 10 * 1024 ** 3 ? 0 : 1)} GiB`;
}
