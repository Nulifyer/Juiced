import { useEffect, useMemo, useState } from "react";
import { Activity, Pause, Play, RefreshCw, Search } from "lucide-react";
import { api, type Model } from "../api";
import type { RuntimeTelemetry } from "./RuntimeMetrics";
import type { RuntimeLogs as LogState } from "../logs/useLogStream";
import { RuntimeLogs } from "./RuntimeLogs";
import { Button } from "./t3/button";
import "./Diagnostics.css";

const finite = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value);
const format = (value: unknown, unit = "", digits = 1) => finite(value) ? value.toLocaleString(undefined, { maximumFractionDigits: digits }) + unit : "Not reported";

function HistoryChart({ title, values, times, unit, ceiling }: { title: string; values: (number | undefined)[]; times: number[]; unit: string; ceiling?: number }) {
  const valid = values.filter(finite);
  const maximum = ceiling || Math.max(1, ...valid);
  const paths: string[] = [];
  let path = "";
  values.forEach((value, index) => {
    if (!finite(value)) { if (path) paths.push(path); path = ""; return; }
    const x = (times[index] - (times.at(-1)! - 120000)) / 120000 * 300;
    if (index > 0 && times[index] - times[index - 1] > 6000) { if (path) paths.push(path); path = ""; }
    const y = 64 - Math.min(1, Math.max(0, value / maximum)) * 60;
    path += `${path ? " L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`;
  });
  if (path) paths.push(path);
  return <section className="history-chart"><header><span>{title}</span><strong>{format(values.at(-1), unit)}</strong></header>
    <svg viewBox="0 0 300 72" role="img" aria-label={`${title} history, maximum scale ${format(maximum, unit)}`}><title>{title}, latest {format(values.at(-1), unit)}</title>
      <path d="M0,64 H300" className="chart-baseline" />{paths.map((d, i) => <path key={i} d={d} className="chart-line" />)}
      {finite(values.at(-1)) && <circle cx={300} cy={64 - Math.min(1, Math.max(0, values.at(-1)! / maximum)) * 60} r={2.5} fill="currentColor" />}
    </svg><footer><span>{valid.length ? "Up to 2 minutes in this tab" : "Waiting for native measurements"}</span><span>0–{format(maximum, unit)}</span></footer></section>;
}

function Performance({ data, models }: { data: RuntimeTelemetry; models: Model[] }) {
  const [frozen, setFrozen] = useState<RuntimeTelemetry | null>(null);
  const [gpuId, setGpuId] = useState("");
  const current = frozen || data;
  const end = current.history?.at(-1)?.at || Date.now();
  const samples = (current.history || []).filter((sample) => sample.at >= end - 120000);
  const system = current.stale ? undefined : current.system;
  const stats = current.stale ? undefined : current.inference;
  const gpu = system?.gpus?.find((g: any) => g.id === (gpuId || system.primary_gpu_id));
  const active = stats?.live?.active_requests;
  const rate = finite(active) ? active > 0 ? stats?.live?.estimated_tokens_per_second : 0 : undefined;
  const last = (stats?.request_count_total ?? stats?.output_tokens ?? 0) > 0 ? stats : undefined;
  return <section className="performance-view" aria-label="Runtime performance">
    <div className="diagnostics-toolbar"><p>{current.stale ? "Telemetry is unavailable. Retained charts may be stale." : frozen ? "View paused. Inference and data collection continue." : "Native runtime measurements. Updates every 2 seconds."}</p>
      <Button variant="outline" size="sm" onClick={() => setFrozen(frozen ? null : data)}>{frozen ? <Play size={14} /> : <Pause size={14} />}{frozen ? "Resume performance" : "Pause performance"}</Button></div>
    <div className="performance-summary">
      <div><span>Active requests</span><strong>{format(stats?.live?.active_requests, "", 0)}</strong></div>
      <div><span>{stats?.live?.active_requests > 0 ? "Live output estimate" : "Live output"}</span><strong>{current.stale ? "Not reported" : format(rate, " tok/s")}</strong></div>
      <div><span>Loaded workers</span><strong>{models.filter((model) => model.loaded).length}</strong></div>
      <div><span>Last first token</span><strong>{format(last?.time_to_first_token, " s", 3)}</strong></div>
    </div>
    {(system?.gpus?.length || 0) > 1 && <label className="gpu-chart-select">GPU shown in chart<select aria-label="GPU history device" value={gpuId || system.primary_gpu_id} onChange={(e) => setGpuId(e.target.value)}>{system.gpus.map((item: any) => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label>}
    <div className="performance-charts">
      <HistoryChart times={samples.map((sample) => sample.at)} title="CPU utilization" values={samples.map((sample) => sample.system.cpu_percent)} unit="%" ceiling={100} />
      <HistoryChart times={samples.map((sample) => sample.at)} title="Host RAM used" values={samples.map((sample) => sample.system.memory_gb)} unit=" GiB" />
      <HistoryChart times={samples.map((sample) => sample.at)} title="GPU utilization" values={samples.map((sample) => sample.system.gpus?.find((g: any) => g.id === (gpuId || sample.system.primary_gpu_id))?.gpu_percent)} unit="%" ceiling={100} />
      <HistoryChart times={samples.map((sample) => sample.at)} title="Live output estimate" values={samples.map((sample) => finite(sample.inference?.live?.active_requests) ? sample.inference.live.active_requests > 0 ? sample.inference.live.estimated_tokens_per_second : 0 : undefined)} unit=" tok/s" />
    </div>
    <section className="diagnostic-details"><h2>Last completed inference</h2><dl>
      <div><dt>Decode rate</dt><dd>{format(last?.tokens_per_second, " tok/s")}</dd></div><div><dt>Input tokens</dt><dd>{format(last?.input_tokens, "", 0)}</dd></div>
      <div><dt>Output tokens</dt><dd>{format(last?.output_tokens, "", 0)}</dd></div><div><dt>Cached tokens</dt><dd>{format(last?.cache_tokens, "", 0)}</dd></div>
      <div><dt>GPU allocation</dt><dd>{format(gpu?.memory_used_gb, " GiB")}{gpu?.memory_type === "shared" ? " shared with host RAM" : ""}</dd></div>
      <div><dt>NPU utilization</dt><dd>{format(system?.npu_percent, "%")}</dd></div>
    </dl><p>Shared GPU allocation overlaps host RAM. Do not add the two. Prefill speed, load time and per-worker KV occupancy are not inferred from aggregate counters when the backend does not report them.</p></section>
  </section>;
}

export type RequestRecord = { id: number; timestamp_ms: number; model: string | null; endpoint: string; protocol: string; stream: boolean; status: number; duration_ms: number; input_tokens: number | null; output_tokens: number | null };
function Requests({ active, models, inspect }: { active: boolean; models: Model[]; inspect: (model: Model) => void }) {
  const [records, setRecords] = useState<RequestRecord[]>([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState("all");
  const [paused, setPaused] = useState(false);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (!active || paused) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const result = await api<{ data: RequestRecord[] }>("juiced/requests", undefined, AbortSignal.any([controller.signal, AbortSignal.timeout(5000)]));
        if (!controller.signal.aborted) { setRecords(result.data); setError(""); }
      } catch (error) { if (!controller.signal.aborted) setError((error as Error).message); }
      finally { if (!controller.signal.aborted) { setLoading(false); timer = setTimeout(poll, 2000); } }
    }
    void poll();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [active, paused, revision]);
  const visible = useMemo(() => records.filter((record) => `${record.model || ""} ${record.endpoint} ${record.protocol}`.toLowerCase().includes(query.toLowerCase()) &&
    (status === "all" || (status === "errors" ? record.status >= 400 : record.status < 400))), [records, query, status]);
  return <section className="request-view" aria-label="Inference request history">
    <div className="request-controls"><label className="search-input"><Search size={15} /><input aria-label="Search requests" placeholder="Find a model, endpoint or protocol…" value={query} onChange={(event) => setQuery(event.target.value)} /></label>
      <select aria-label="Request status" value={status} onChange={(event) => setStatus(event.target.value)}><option value="all">All requests</option><option value="success">Successful</option><option value="errors">Errors</option></select>
      <Button variant="outline" size="sm" onClick={() => setPaused(!paused)}>{paused ? <Play size={14} /> : <Pause size={14} />}{paused ? "Resume requests" : "Pause requests"}</Button>
      <Button variant="ghost" size="sm" aria-label="Refresh request history" disabled={paused} onClick={() => setRevision(revision + 1)}><RefreshCw size={14} /></Button></div>
    <p className="page-note">{visible.length} retained requests shown. {paused ? "View paused. " : ""}Latest 200 completed inference requests since manager restart. Metadata only; prompts, output and client session IDs are never captured here.</p>
    {error && <p role="alert" className="inline-error">Request history unavailable: {error}. <Button size="sm" variant="outline" onClick={() => { setPaused(false); setRevision(revision + 1); }}>Retry requests</Button></p>}
    <div className="request-table-scroll" tabIndex={0} aria-label="Completed requests">
      <table><thead><tr><th>Completed</th><th>Model / endpoint</th><th>Status</th><th>Duration</th><th>Input / output</th></tr></thead><tbody>
        {visible.map((record) => <tr key={record.id}><td><time dateTime={new Date(record.timestamp_ms).toISOString()}>{new Date(record.timestamp_ms).toLocaleTimeString()}</time></td><td><details><summary>{record.model || "Model not reported"}<small>{record.protocol} · {record.endpoint}</small></summary><p>Request {record.id}. {record.stream ? "Streaming" : "Non-streaming"}. Duration includes loading, queueing and inference.</p>{models.some((model) => model.id === record.model) && <Button variant="outline" size="sm" onClick={() => inspect(models.find((model) => model.id === record.model)!)}>Inspect model</Button>}</details></td>
          <td><span className={record.status >= 400 ? "request-error" : "is-ready"}>{record.status}</span></td><td>{format(record.duration_ms / 1000, " s", 3)}</td><td>{record.input_tokens ?? "—"} / {record.output_tokens ?? "—"}</td></tr>)}
      </tbody></table>
      {!visible.length && <div className="requests-empty"><Activity size={26} /><h2>{loading ? "Reading request history…" : error ? "Request history is unavailable" : records.length ? "No requests match these filters" : "No completed inference requests yet"}</h2><p>Requests appear after they finish. Streaming tokens are not guessed from another request's counters.</p></div>}
    </div>
  </section>;
}

export function Diagnostics({ data, logs, models, inspect, initialTab = "performance" }: { initialTab?: string; data: RuntimeTelemetry; logs: LogState; models: Model[]; inspect: (model: Model) => void }) {
  const [tab, setTab] = useState(initialTab);
  useEffect(() => setTab(initialTab), [initialTab]);
  return <div className="diagnostics-workspace"><header className="page-heading"><div><h1>Diagnostics</h1><p>Understand resource use, completed requests and runtime output.</p></div></header>
    <nav className="workspace-tabs" aria-label="Diagnostic views">{["performance", "requests", "logs"].map((name) => <button key={name} aria-current={tab === name ? "page" : undefined} className={tab === name ? "active" : ""} onClick={() => setTab(name)}>{name[0].toUpperCase() + name.slice(1)}</button>)}</nav>
    {tab === "performance" && <Performance data={data} models={models} />}
    <div hidden={tab !== "requests"}><Requests active={tab === "requests"} models={models} inspect={inspect} /></div>
    <div className="diagnostics-logs" hidden={tab !== "logs"}><RuntimeLogs logs={logs} /></div>
  </div>;
}
