import { useEffect, useMemo, useRef, useState } from "react";
import type { Client } from "./api";
import { Sparkline } from "./charts";
import { bytes, type RequestRecord, type Snapshot } from "./domain";
import { gib, parseLogPage, percent, rate, type LogLine, type StatsPoint, type SystemStats } from "./live";
import { count, failed, formatMs, modelPerformance, pathNames, timing } from "./performance";
import { Badge, Button, Empty, HelpTip } from "./ui";

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
  const models = modelPerformance(snapshot.requests);
  const charts = [
    ["CPU", percent(stats?.cpu_percent), history.map((point) => point.cpu)],
    ["Memory", `${gib(stats?.memory?.used_bytes)} / ${gib(stats?.memory?.total_bytes)}`, history.map((point) => point.memory)],
    ["GPU", gpu ? percent(gpu.gpu_percent) : "No GPU", history.map((point) => point.gpu)],
    ["VRAM", gpu ? `${gib(gpu.memory_used_bytes)} / ${gib(gpu.memory_total_bytes)}` : "No GPU", history.map((point) => point.vram)],
  ] as const;
  const cache = resources.cache_control_memory, runtime = resources.runtime_control_memory;
  return <div className="performance">
    <section className="panel" aria-label="Model performance">
      <header className="panel-head"><h2>By model</h2><span className="muted">{snapshot.requests.length ? `Last ${snapshot.requests.length} ${snapshot.requests.length === 1 ? "request" : "requests"}` : ""}</span></header>
      {models.length === 0 ? <p className="muted panel-empty">Send a request to see timings.</p> : <div className="data-table stacked perf-table" role="table" aria-label="Model performance">
        <div role="row" className="data-head">
          <span role="columnheader">Model</span><span role="columnheader" className="numeric">Requests</span><span role="columnheader" className="numeric">Typical time</span>
          <span role="columnheader" className="numeric">Prompt time <HelpTip label="Prompt time">Median time spent reading the prompt. Reused: the engine still had the conversation, or restored it, and read only the new part. Full: it read the whole prompt.</HelpTip></span>
          <span role="columnheader" className="numeric">Cached</span><span role="columnheader" className="numeric">Speed</span><span role="columnheader">Generate trend</span>
        </div>
        {models.map((model) => <div role="row" className="data-row" key={model.model}>
          <span role="cell" className="break-word">{model.model}</span>
          <span role="cell" className="numeric" data-label="Requests">{model.count}{model.failed > 0 && <span className="cell-note tone-bad">{model.failed} failed</span>}</span>
          <span role="cell" className="numeric" data-label="Typical time">{formatMs(model.medianMs)}</span>
          <span role="cell" className={`numeric ${model.reusedPromptMs == null && model.fullPromptMs == null ? "no-data" : ""}`} data-label="Prompt time">{model.reusedPromptMs == null && model.fullPromptMs == null ? "—" : <>{model.reusedPromptMs != null && <span>{formatMs(model.reusedPromptMs)} reused</span>}{model.fullPromptMs != null && <span className="cell-note">{formatMs(model.fullPromptMs)} full</span>}</>}</span>
          <span role="cell" className={`numeric ${model.cachedShare == null ? "no-data" : ""}`} data-label="Cached">{model.cachedShare == null ? "—" : `${Math.round(model.cachedShare * 100)}%`}</span>
          <span role="cell" className={`numeric ${model.decodeRate == null ? "no-data" : ""}`} data-label="Speed">{model.decodeRate == null ? "—" : <>{rate(model.decodeRate)}{model.promptRate != null && <span className="cell-note">prompt {rate(model.promptRate)}</span>}</>}</span>
          <span role="cell" className={`trend-cell ${model.decodeTrend.length > 1 ? "" : "no-data"}`} data-label="Generate trend">{model.decodeTrend.length > 1 ? <Sparkline values={model.decodeTrend} max={0} height={28} label={`Generate speed over recent requests for ${model.model}`} /> : null}</span>
        </div>)}
      </div>}
    </section>
    <div className="stat-grid" aria-label="Resource history">{charts.map(([label, value, series]) => <article className="chart-card" key={label} aria-label={label}><div className="stat-head"><span>{label}</span><strong>{value}</strong></div><Sparkline values={[...series]} height={72} label={`${label} over the last two minutes`} /></article>)}</div>
    <section className="panel" aria-label="Reservations">
      <header className="panel-head"><h2>Reservations</h2><span className="muted">Declared limits used to admit work</span></header>
      <dl className="fact-grid">
        <div><dt>Budget</dt><dd>{bytes(resources.resident_budget_bytes ?? null)}</dd></div>
        <div><dt>Reserved</dt><dd>{bytes(resources.reserved_bytes)}</dd></div>
        {cache?.state === "accounted" && <><div><dt>Cache metadata</dt><dd>{bytes(cache.metadata_bytes)}</dd></div><div><dt>Cache work</dt><dd>{bytes(cache.scratch_bytes)}</dd></div></>}
        {runtime?.state === "accounted" && <><div><dt>Runtime metadata</dt><dd>{bytes(runtime.metadata_bytes)}</dd></div><div><dt>Runtime bindings</dt><dd>{bytes(runtime.binding_bytes)}</dd></div><div><dt>Runtime work</dt><dd>{bytes(runtime.scratch_bytes)}</dd></div></>}
        <div><dt>Active requests</dt><dd>{resources.active_requests}</dd></div>
      </dl>
    </section>
  </div>;
}

const segments = [["queue", "Queue"], ["prompt", "Prompt"], ["generate", "Generate"], ["other", "Other"]] as const;

function promptNote(request: RequestRecord): string | null {
  const session = request.session_cache;
  const cached = request.cached_tokens ? `${count(request.cached_tokens)} cached` : null;
  if (session?.path === "restored") return [session.restore_ms != null ? `restored in ${formatMs(session.restore_ms)}` : "restored from disk", cached].filter(Boolean).join(" · ");
  if (session?.path === "slot-hit") return cached ?? pathNames["slot-hit"]!.toLowerCase();
  if (session) return pathNames[session.path]?.toLowerCase() ?? session.path;
  return cached;
}

function TimingBar({ request, longest }: { request: RequestRecord; longest: number }) {
  const parts = timing(request);
  const detail = segments.filter(([key]) => parts[key] >= 1).map(([key, label]) => `${label} ${formatMs(parts[key])}`).join(" · ");
  return <span className="timing" title={detail}>
    <span className="timing-track"><span className="timing-bar" role="img" aria-label={detail || formatMs(parts.total)} style={{ width: `${Math.max(2, parts.total / longest * 100)}%` }}>
      {segments.map(([key]) => parts[key] / Math.max(1, parts.total) >= 0.005 && <span key={key} className={`segment ${key}`} style={{ flexGrow: parts[key], flexBasis: 0 }} />)}
    </span></span>
    <span className="timing-total">{formatMs(parts.total)}</span>
  </span>;
}

function Requests({ snapshot }: { snapshot: Snapshot }) {
  if (snapshot.requests.length === 0) return <Empty title="No requests yet">Requests from apps and tests show here. Prompts and replies are never stored.</Empty>;
  const longest = Math.max(1, ...snapshot.requests.map((request) => request.elapsed_ms));
  return <div className="requests">
    <div className="timing-legend" aria-label="Time breakdown">
      {segments.map(([key, label]) => <span key={key}><span className={`swatch ${key}`} />{label}</span>)}
      <HelpTip label="Time breakdown">Prompt and Generate come from the engine's own token rates. Other covers restoring a saved conversation, audio work and network time. Prompts and replies are never stored.</HelpTip>
    </div>
    <div className="data-table stacked requests-table" role="table" aria-label="Recent requests">
      <div role="row" className="data-head"><span role="columnheader">Time</span><span role="columnheader">Model</span><span role="columnheader">Outcome</span><span role="columnheader" className="numeric">Prompt</span><span role="columnheader" className="numeric">Output</span><span role="columnheader" className="numeric">Speed</span><span role="columnheader">Duration</span></div>
      {snapshot.requests.map((request) => {
        const note = promptNote(request);
        return <div role="row" className="data-row" key={request.id}>
          <span role="cell" className="muted request-time">{request.completed_at_ms ? new Date(request.completed_at_ms).toLocaleTimeString([], { hour12: false }) : "—"}</span>
          <span role="cell" className="request-model"><span className="break-word">{request.model}</span><code className="cell-note">{request.operation}</code></span>
          <span role="cell"><Badge tone={failed(request) ? "bad" : request.outcome === "cancelled" ? "quiet" : "good"}>{request.outcome.replaceAll("_", " ")}{request.status ? ` · ${request.status}` : ""}</Badge></span>
          <span role="cell" className={`numeric ${request.input_tokens == null ? "no-data" : ""}`} data-label="Prompt">{count(request.input_tokens)}{note && <span className="cell-note">{note}</span>}</span>
          <span role="cell" className={`numeric ${request.output_tokens == null ? "no-data" : ""}`} data-label="Output">{count(request.output_tokens)}</span>
          <span role="cell" className={`numeric ${request.decode_tokens_per_second == null && request.prompt_tokens_per_second == null ? "no-data" : ""}`} data-label="Speed">{rate(request.decode_tokens_per_second)}{request.prompt_tokens_per_second != null && <span className="cell-note">prompt {rate(request.prompt_tokens_per_second)}</span>}</span>
          <span role="cell" className="timing-cell"><TimingBar request={request} longest={longest} /></span>
        </div>;
      })}
    </div>
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
