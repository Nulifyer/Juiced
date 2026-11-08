import { useEffect, useState } from "react";
import { Activity, Cpu, MemoryStick, Gauge } from "lucide-react";
import { api } from "../api";
import "./RuntimeMetrics.css";
export type RuntimeTelemetry = {
  system?: any;
  inference?: any;
  stale: boolean;
  history?: { at: number; system: any; inference: any }[];
};
export function useRuntimeTelemetry() {
  const [data, setData] = useState<RuntimeTelemetry>({ stale: true });
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const signal = AbortSignal.any([
          controller.signal,
          AbortSignal.timeout(5000),
        ]);
        const [system, inference] = await Promise.all([
          api("system-stats", undefined, signal),
          api("stats", undefined, signal),
        ]);
        if (!controller.signal.aborted)
          setData((previous) => ({ system, inference, stale: false,
            history: [...(previous.history || []).slice(-59), { at: Date.now(), system, inference }] }));
      } catch {
        if (!controller.signal.aborted)
          setData((previous) => ({ ...previous, stale: true }));
      } finally {
        if (!controller.signal.aborted) timer = setTimeout(poll, 2000);
      }
    }
    void poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, []);
  return data;
}
const number = (value: unknown, suffix = "") =>
  typeof value === "number" && Number.isFinite(value)
    ? value.toFixed(1) + suffix
    : "—";
export function RuntimeMetrics({ data }: { data: RuntimeTelemetry }) {
  const system = data.stale ? undefined : data.system;
  const stats = data.stale ? undefined : data.inference;
  const gpu = system?.gpus?.find((g: any) => g.id === system.primary_gpu_id);
  const active = stats?.live?.active_requests;
  const generating = stats?.live?.generating_requests ?? active;
  return (
    <footer
      className="runtime-metrics"
      aria-label="Runtime hardware and inference metrics"
    >
      <span title="Host CPU usage reported by the runtime">
        <Cpu size={13} />
        CPU <strong>{number(system?.cpu_percent, "%")}</strong>
      </span>
      <span title="Host RAM used. Shared GPU memory overlaps host memory; do not add the two.">
        <MemoryStick size={13} />
        RAM <strong>{number(system?.memory_gb, " GiB")}</strong>
      </span>
      <span title={gpu?.name || "GPU utilization reported by the runtime"}>
        <Gauge size={13} />
        GPU <strong>{number(system?.gpu_percent, "%")}</strong>
      </span>
      <span title="GPU memory allocation. Shared memory overlaps RAM.">
        {gpu?.memory_type === "shared" ? "GPU shared" : "VRAM"}{" "}
        <strong>{number(system?.vram_gb, " GiB")}</strong>
      </span>
      <span title="NPU usage; unavailable when the runtime cannot report it">
        NPU <strong>{number(system?.npu_percent, "%")}</strong>
      </span>
      <span
        className="metrics-throughput"
        title="Recent text output estimate. Prefill, speech and images can be active without emitting text tokens."
      >
        <Activity size={13} />
        {generating > 0 ? "Live estimate" : active > 0 ? "Text output" : "Last decode"}
        <strong>
          {number(
            generating > 0 || active > 0
              ? stats?.live?.estimated_tokens_per_second
              : stats?.tokens_per_second,
            " tok/s",
          )}
        </strong>
      </span>
      <span title="Native model calls in progress, including prefill, speech and images">
        Active <strong>{active ?? "—"}</strong>
      </span>
      <details>
        <summary>Details</summary>
        <div>
          <span>
            Last request input{" "}
            <strong>
              {stats?.input_tokens?.toLocaleString() ?? "—"} tokens
            </strong>
          </span>
          <span>
            Last request output{" "}
            <strong>
              {stats?.output_tokens?.toLocaleString() ?? "—"} tokens
            </strong>
          </span>
          <span>
            First token{" "}
            <strong>{number(stats?.time_to_first_token, " s")}</strong>
          </span>
          <span>
            Cached tokens{" "}
            <strong>{stats?.cache_tokens?.toLocaleString() ?? "—"}</strong>
          </span>
          <span>
            {data.stale
              ? "Telemetry unavailable. Reconnecting…"
              : "Updates every 2 seconds. Host RAM and shared GPU allocation overlap."}
          </span>
        </div>
      </details>
    </footer>
  );
}
