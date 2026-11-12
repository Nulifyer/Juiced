import { useEffect, useMemo, useRef, useState } from "react";
import type { Client } from "./api";
import { Sparkline } from "./charts";
import { bytes, type Snapshot } from "./domain";
import { gib, parseLogPage, percent, rate, type LogLine, type StatsPoint, type SystemStats } from "./live";
import { Badge, Button, Empty } from "./ui";

type Tab = "performance" | "requests" | "logs";
type Props = { client: Client; snapshot: Snapshot; stats: SystemStats | null; history: StatsPoint[]; tab: Tab };

export function Diagnostics({ client, snapshot, stats, history, tab }: Props) {
  return <>
    {tab === "performance" && <Performance snapshot={snapshot} stats={stats} history={history} />}
    {tab === "requests" && <Requests snapshot={snapshot} />}
    {tab === "logs" && <Logs client={client} models={snapshot.models.map((model) => model.id)} />}
  </>;
}

function Performance({ snapshot, stats, history }: { snapshot: Snapshot; stats: SystemStats | null; history: StatsPoint[] }) {
  const gpu = stats?.gpus[0];
  const resources = snapshot.resources;
  const charts = [
    ["CPU", percent(stats?.cpu_percent), history.map((point) => point.cpu), 100],
    ["Memory", `${gib(stats?.memory?.used_bytes)} / ${gib(stats?.memory?.total_bytes)}`, history.map((point) => point.memory), 100],
    ["GPU", gpu ? percent(gpu.gpu_percent) : "No GPU", history.map((point) => point.gpu), 100],
    ["VRAM", gpu ? `${gib(gpu.memory_used_bytes)} / ${gib(gpu.memory_total_bytes)}` : "No GPU", history.map((point) => point.vram), 100],
  ] as const;
  const cache = resources.cache_control_memory, runtime = resources.runtime_control_memory;
  return <div className="performance">
    <div className="stat-grid">{charts.map(([label, value, series, max]) => <article className="chart-card" key={label} aria-label={label}><div className="stat-head"><span>{label}</span><strong>{value}</strong></div><Sparkline values={[...series]} max={max} height={72} label={`${label} over the last two minutes`} /></article>)}</div>
    <section className="panel" aria-label="Reservations">
      <header className="panel-head"><h2>Reservations</h2><span className="muted">Declared limits used to admit work</span></header>
      <dl className="fact-grid">
        <div><dt>Budget</dt><dd>{bytes(resources.resident_budget_bytes ?? null)}</dd></div>
        <div><dt>Reserved</dt><dd>{bytes(resources.reserved_bytes)}</dd></div>
        {cache?.state === "accounted" && <><div><dt>Cache metadata</dt><dd>{bytes(cache.metadata_bytes)}</dd></div><div><dt>Cache work</dt><dd>{bytes(cache.scratch_bytes)}</dd></div></>}
        {runtime?.state === "accounted" && <><div><dt>Runtime metadata</dt><dd>{bytes(runtime.metadata_bytes)}</dd></div><div><dt>Runtime bindings</dt><dd>{bytes(runtime.binding_bytes)}</dd></div><div><dt>Runtime work</dt><dd>{bytes(runtime.scratch_bytes)}</dd></div></>}
        <div><dt>Active requests</dt><dd>{resources.active_requests}</dd></div>
      </dl>
      {cache?.state === "unavailable" && <p className="inline-warning">Not enough host memory to open the snapshot cache. Free memory and restart the service to use snapshots.</p>}
    </section>
  </div>;
}

function Requests({ snapshot }: { snapshot: Snapshot }) {
  if (snapshot.requests.length === 0) return <Empty title="No requests yet">Requests from apps and tests show here. Prompts and replies are never stored.</Empty>;
  return <div className="data-table requests-table" role="table" aria-label="Recent requests">
    <div role="row" className="data-head"><span role="columnheader">Model</span><span role="columnheader">Operation</span><span role="columnheader">Outcome</span><span role="columnheader" className="numeric">Tokens in / out</span><span role="columnheader" className="numeric">Speed</span><span role="columnheader" className="numeric">Time</span></div>
    {snapshot.requests.map((request) => <div role="row" className="data-row" key={request.id}>
      <span role="cell" className="break-word">{request.model}</span>
      <code role="cell" className="muted">{request.operation}</code>
      <span role="cell"><Badge tone={request.outcome === "completed" ? "good" : request.outcome === "cancelled" ? "quiet" : "bad"}>{request.outcome}{request.status ? ` · ${request.status}` : ""}</Badge></span>
      <span role="cell" className="numeric">{request.input_tokens ?? "—"} / {request.output_tokens ?? "—"}</span>
      <span role="cell" className="numeric">{rate(request.decode_tokens_per_second)}</span>
      <span role="cell" className="numeric muted">{request.elapsed_ms} ms</span>
    </div>)}
  </div>;
}

const MAX_LINES = 2000;
function Logs({ client, models }: { client: Client; models: string[] }) {
  const [lines, setLines] = useState<LogLine[]>([]);
  const [source, setSource] = useState("");
  const [level, setLevel] = useState<"all" | "warning" | "error">("all");
  const [query, setQuery] = useState("");
  const [follow, setFollow] = useState(true);
  const [dropped, setDropped] = useState(0);
  const [error, setError] = useState("");
  const cursor = useRef(0);
  const view = useRef<HTMLDivElement>(null);
  useEffect(() => {
    setLines([]); setDropped(0); setError(""); cursor.current = 0;
    const controller = new AbortController();
    let busy = false;
    const poll = async () => {
      if (busy) return;
      busy = true;
      try {
        const params = new URLSearchParams({ after: String(cursor.current), limit: "500" });
        if (source) params.set("source", source);
        const page = parseLogPage(await client.request(`juiced/logs?${params}`, undefined, controller.signal));
        if (controller.signal.aborted) return;
        cursor.current = page.next;
        if (page.dropped) setDropped((count) => count + page.dropped);
        if (page.data.length) setLines((current) => [...current, ...page.data].slice(-MAX_LINES));
        setError("");
      } catch (failure) {
        if (!controller.signal.aborted) setError(failure instanceof Error ? failure.message : "Logs are unavailable.");
      } finally { busy = false; }
    };
    void poll();
    const timer = setInterval(() => void poll(), 1000);
    return () => { controller.abort(); clearInterval(timer); };
  }, [client, source]);
  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return lines.filter((line) => (level === "all" || line.level === "error" || (level === "warning" && line.level === "warning")) && (!needle || line.text.toLowerCase().includes(needle)));
  }, [lines, level, query]);
  useEffect(() => { if (follow && view.current) view.current.scrollTop = view.current.scrollHeight; }, [visible, follow]);
  return <div className="logs">
    <div className="toolbar">
      <input type="search" aria-label="Search logs" placeholder="Search logs" value={query} onChange={(event) => setQuery(event.target.value)} />
      <select aria-label="Log source" value={source} onChange={(event) => setSource(event.target.value)}><option value="">All sources</option><option value="service">Service</option>{models.map((model) => <option key={model} value={model}>{model}</option>)}</select>
      <div className="segmented" role="group" aria-label="Log level">{(["all", "warning", "error"] as const).map((value) => <button key={value} aria-pressed={level === value} onClick={() => setLevel(value)}>{value === "all" ? "All" : value === "warning" ? "Warnings" : "Errors"}</button>)}</div>
      <Button variant={follow ? "outline" : "primary"} onClick={() => setFollow(!follow)}>{follow ? "Pause" : "Follow"}</Button>
    </div>
    {error && <p className="inline-error" role="alert">{error}</p>}
    {dropped > 0 && <p className="field-help">{dropped} older lines were dropped before they could be shown.</p>}
    <div ref={view} className="log-view" role="log" aria-live="off" aria-label="Log lines" onWheel={() => { if (view.current && view.current.scrollTop + view.current.clientHeight < view.current.scrollHeight - 8) setFollow(false); }}>
      {visible.length === 0 ? <p className="muted log-empty">{lines.length ? "No lines match." : "Waiting for log output…"}</p> : visible.map((line) => <div className={`log-line ${line.level ?? ""}`} key={line.seq}>
        <time>{new Date(line.timestamp_ms).toLocaleTimeString([], { hour12: false })}</time><span className="log-source">{line.source}</span><span className="log-text">{line.text}{line.truncated ? " …" : ""}</span>
      </div>)}
    </div>
  </div>;
}
