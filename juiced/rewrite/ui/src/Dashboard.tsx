import type { ReactNode } from "react";
import type { Client } from "./api";
import { Meter } from "./charts";
import { ago, parseHealth, summary } from "./connectData";
import { policyNames, workerState, workerTone, type Model, type Snapshot } from "./domain";
import { gib, gpuName, percent, type SystemStats } from "./live";
import { failed } from "./performance";
import { remaining } from "./Residency";
import { lowSpace, parseStorageSummary } from "./storageData";
import { Badge, Empty } from "./ui";
import { useResource } from "./useResource";

type Props = { client: Client; snapshot: Snapshot; stats: SystemStats | null; onOpenModel: (id: string) => void; onOpenRequests: () => void; onOpenConnect: () => void; onOpenStorage: () => void };
type Issue = { key: string; title: string; detail: string; tone: "warning" | "bad"; open?: () => void };

const RECENT_FAILURE_MS = 60 * 60_000;

function attention(model: Model): string | null {
  if (model.state === "failed") return "Failed to start";
  if (model.state === "cleanup-pending") return "Needs cleanup";
  if (model.state === "missing-weights" && model.policy !== "disabled") return "Weights missing";
  if ((model.uncertain_requests ?? 0) > 0 || (model.uncertain_state_operations ?? 0) > 0) return "Unconfirmed work";
  if (model.policy === "hot" && !model.loaded && model.state !== "loading") return "Hot but not loaded";
  return null;
}

function share(used: number | null | undefined, total: number | null | undefined) {
  return used != null && total ? used / total * 100 : null;
}

function usage(model: Model, now: number): string {
  const residency = model.residency;
  if (model.policy === "on-demand" && residency?.idle_unload_at_unix_ms) return `Unloads in ${remaining(Math.max(0, residency.idle_unload_at_unix_ms - now))}`;
  return residency?.last_used_unix_ms ? `Used ${ago(residency.last_used_unix_ms, now)}` : "Not used yet";
}

function pair(used: number | null | undefined, total: number | null | undefined) {
  return `${gib(used).replace(" GiB", "")} / ${gib(total)}`;
}

function Tile({ label, value, children, foot }: { label: string; value: ReactNode; children: ReactNode; foot: ReactNode }) {
  return <article className="stat-card tile" aria-label={label}>
    <div className="stat-head"><span>{label}</span><strong>{value}</strong></div>
    <span className="stat-detail">{children}</span>
    <div className="tile-foot">{foot}</div>
  </article>;
}

export function Dashboard({ client, snapshot, stats, onOpenModel, onOpenRequests, onOpenConnect, onOpenStorage }: Props) {
  const health = useResource(client, "juiced/protocols/health", parseHealth, 5000, true);
  const storage = useResource(client, "juiced/storage?summary=1", parseStorageSummary, 30000, true);
  const now = Date.now();
  const gpu = stats?.gpus[0];
  const resources = snapshot.resources;
  const running = snapshot.models.filter((model) => model.loaded || model.state === "loading" || model.startup_waiting);
  const hot = snapshot.models.filter((model) => model.policy === "hot");
  const protocols = (health.data ?? []).filter((protocol) => protocol.enabled);
  const broken = protocols.filter((protocol) => protocol.status === "error");
  const inUse = protocols.filter((protocol) => protocol.status === "healthy");
  const lastClient = Math.max(0, ...protocols.map((protocol) => protocol.last?.at ?? 0));
  const latest = snapshot.requests[0];
  const slots = running.filter((model) => model.loaded).reduce((sum, model) => sum + model.max_inflight, 0);
  const failures = snapshot.requests.filter((request) => failed(request) && (request.completed_at_ms == null || now - request.completed_at_ms < RECENT_FAILURE_MS));
  const cache = resources.cache_control_memory;
  const issues: Issue[] = [
    ...snapshot.models.flatMap((model) => { const issue = attention(model); return issue ? [{ key: `model:${model.id}`, title: model.id, detail: issue, tone: "warning" as const, open: () => onOpenModel(model.id) }] : []; }),
    ...broken.map((protocol) => ({ key: `protocol:${protocol.id}`, title: protocol.title, detail: summary(protocol, now), tone: "bad" as const, open: onOpenConnect })),
    ...failures.length ? [{ key: "requests", title: `${failures.length} failed ${failures.length === 1 ? "request" : "requests"}`, detail: `Latest: ${failures[0]!.model} · ${failures[0]!.status ?? failures[0]!.outcome.replaceAll("_", " ")}`, tone: "bad" as const, open: onOpenRequests }] : [],
    ...lowSpace(storage.data).map((warning) => ({ key: `storage:${warning.code}:${warning.path ?? ""}`, title: warning.code === "low_space" ? "Low disk space" : warning.code === "session_cache_over_budget" ? "Session cache over budget" : "Downloads can't be recorded", detail: warning.message, tone: "warning" as const, open: onOpenStorage })),
    ...cache?.state === "unavailable" ? [{ key: "cache", title: "Snapshot cache unavailable", detail: "Not enough host memory. Free memory, then restart the service.", tone: "warning" as const }] : [],
  ];
  const active = stats?.active_requests ?? resources.active_requests;
  const queued = stats?.queued_requests ?? 0;
  const budget = resources.resident_budget_bytes;
  return <div className="dashboard">
    <section className="stat-grid" aria-label="At a glance">
      <Tile label="Protocols" value={health.data ? broken.length ? <span className="tone-bad">{broken.length} failing</span> : `${protocols.filter((protocol) => protocol.status === "healthy" || protocol.status === "idle").length} ready` : "—"}
        foot={lastClient ? <span className="stat-detail">Last client request {ago(lastClient, now)}</span> : null}>
        {health.data ? inUse.length ? `${inUse.map((protocol) => protocol.title).join(", ")} in use` : "Waiting for clients" : health.error ? "Operator key needed" : "Checking…"}
      </Tile>
      <Tile label="Requests" value={`${active} active${queued ? ` · ${queued} queued` : ""}`} foot={<Meter plain value={slots ? active / slots * 100 : 0} label="Request slots in use" />}>
        {`of ${slots} ${slots === 1 ? "slot" : "slots"}`}{latest?.completed_at_ms ? ` · last ${ago(latest.completed_at_ms, now)}` : ""}
      </Tile>
      <Tile label="Model memory" value={budget ? pair(resources.reserved_bytes, budget) : gib(resources.reserved_bytes)} foot={budget ? <Meter value={share(resources.reserved_bytes, budget)} label="Model memory reserved" /> : null}>
        {budget ? `${gib(Math.max(0, budget - resources.reserved_bytes))} free for another model` : "No memory budget set"}
      </Tile>
      <Tile label="GPU memory" value={gpu ? pair(gpu.memory_used_bytes, gpu.memory_total_bytes) : "—"} foot={gpu ? <Meter plain value={share(gpu.memory_used_bytes, gpu.memory_total_bytes)} label="GPU memory used" /> : null}>
        {gpu ? `${gpu.gpu_percent == null ? "Utilization unavailable" : `${percent(gpu.gpu_percent)} busy`} · ${gpu.shared ? `shared · ${gib(gpu.shared.dedicated_total)} reserved + ${gib(gpu.shared.shared_total)} system` : gpuName(gpu)}` : "No GPU detected"}
      </Tile>
    </section>
    <div className="dashboard-columns">
      <section className="panel" aria-label="Running models">
        <header className="panel-head"><h2>Running</h2><span className="muted">{hot.length ? `Hot ${hot.filter((model) => model.loaded).length}/${hot.length} ready` : `${running.length} loaded`}</span></header>
        {running.length === 0 ? <Empty title="Nothing loaded">On demand models load on their first request.</Empty> : <ul className="row-list">
          {running.map((model) => <li key={model.id}><button className="row-button" onClick={() => onOpenModel(model.id)}>
            <span className={`status-dot ${workerTone(model)}`} />
            <span className="row-main"><strong>{model.id}</strong><span className="muted">{workerState(model)} · {policyNames[model.policy]}{model.resident_bytes ? ` · ${gib(model.resident_bytes)}` : ""}</span></span>
            <span className="row-meta"><span>{model.active_requests}/{model.max_inflight} busy</span><span className="muted">{usage(model, now)}</span></span>
          </button></li>)}
        </ul>}
      </section>
      <section className="panel" aria-label="Needs attention">
        <header className="panel-head"><h2>Needs attention</h2>{issues.length > 0 && <Badge tone="warning">{issues.length}</Badge>}</header>
        {issues.length === 0 ? <p className="all-clear"><span className="status-dot good" /> Everything looks healthy.</p> : <ul className="row-list">
          {issues.map((issue) => {
            const body = <><span className={`status-dot ${issue.tone}`} /><span className="row-main"><strong>{issue.title}</strong><span className="muted">{issue.detail}</span></span></>;
            return <li key={issue.key}>{issue.open ? <button className="row-button" onClick={issue.open}>{body}</button> : <div className="row-button static">{body}</div>}</li>;
          })}
        </ul>}
      </section>
    </div>
  </div>;
}
