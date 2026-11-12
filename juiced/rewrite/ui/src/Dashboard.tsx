import { Sparkline } from "./charts";
import { policyNames, workerState, workerTone, type Model, type Snapshot } from "./domain";
import { gib, percent, rate, type StatsPoint, type SystemStats } from "./live";
import { Badge, Button, Empty } from "./ui";

type Props = { snapshot: Snapshot; stats: SystemStats | null; history: StatsPoint[]; onOpenModel: (id: string) => void; onTest: () => void };

function attention(model: Model): string | null {
  if (model.state === "failed") return "Failed to start";
  if (model.state === "cleanup-pending") return "Needs cleanup";
  if (model.state === "missing-weights" && model.policy !== "disabled") return "Weights missing";
  if ((model.uncertain_requests ?? 0) > 0 || (model.uncertain_state_operations ?? 0) > 0) return "Unconfirmed work";
  if (model.policy === "hot" && !model.loaded && model.state !== "loading") return "Hot but not loaded";
  return null;
}

export function Dashboard({ snapshot, stats, history, onOpenModel, onTest }: Props) {
  const gpu = stats?.gpus[0];
  const running = snapshot.models.filter((model) => model.loaded || model.state === "loading" || model.startup_waiting);
  const issues = snapshot.models.map((model) => [model, attention(model)] as const).filter((entry): entry is readonly [Model, string] => entry[1] !== null);
  const hot = snapshot.models.filter((model) => model.policy === "hot");
  const lastRate = (id: string) => snapshot.requests.find((request) => request.model === id && request.decode_tokens_per_second != null)?.decode_tokens_per_second ?? null;
  const cards = [
    { label: "CPU", value: percent(stats?.cpu_percent), detail: stats?.supported === false ? "Not available on this OS" : "Utilization", series: history.map((point) => point.cpu) },
    { label: "Memory", value: gib(stats?.memory?.used_bytes), detail: `of ${gib(stats?.memory?.total_bytes)}`, series: history.map((point) => point.memory) },
    { label: "GPU", value: gpu ? percent(gpu.gpu_percent) : "—", detail: gpu?.name ?? "No GPU detected", series: history.map((point) => point.gpu) },
    { label: "VRAM", value: gpu ? gib(gpu.memory_used_bytes) : "—", detail: gpu ? `of ${gib(gpu.memory_total_bytes)}` : "No GPU detected", series: history.map((point) => point.vram) },
  ];
  return <div className="dashboard">
    <section className="stat-grid" aria-label="Live resources">
      {cards.map((card) => <article className="stat-card" key={card.label} aria-label={card.label}>
        <div className="stat-head"><span>{card.label}</span><strong>{card.value}</strong></div>
        <Sparkline values={card.series} label={`${card.label} over the last two minutes`} />
        <span className="stat-detail">{card.detail}</span>
      </article>)}
    </section>
    <div className="dashboard-columns">
      <section className="panel" aria-label="Running models">
        <header className="panel-head"><h2>Running</h2><span className="muted">{hot.length ? `Hot ${hot.filter((model) => model.loaded).length}/${hot.length} ready` : `${running.length} loaded`}</span></header>
        {running.length === 0 ? <Empty title="Nothing loaded">On demand models load on their first request.</Empty> : <ul className="row-list">
          {running.map((model) => <li key={model.id}><button className="row-button" onClick={() => onOpenModel(model.id)}>
            <span className={`status-dot ${workerTone(model)}`} />
            <span className="row-main"><strong>{model.id}</strong><span className="muted">{workerState(model)} · {policyNames[model.policy]}</span></span>
            <span className="row-meta"><span>{model.active_requests}/{model.max_inflight} busy</span><span className="muted">{rate(lastRate(model.id))}</span></span>
          </button></li>)}
        </ul>}
        {stats?.last_inference && <p className="panel-foot">Last reply: {stats.last_inference.model} · {rate(stats.last_inference.decode_tokens_per_second)} · {stats.last_inference.output_tokens ?? "—"} tokens</p>}
      </section>
      <div className="dashboard-side">
        <section className="panel" aria-label="Needs attention">
          <header className="panel-head"><h2>Needs attention</h2>{issues.length > 0 && <Badge tone="warning">{issues.length}</Badge>}</header>
          {issues.length === 0 ? <p className="all-clear"><span className="status-dot good" /> Everything looks healthy.</p> : <ul className="row-list">
            {issues.map(([model, issue]) => <li key={model.id}><button className="row-button" onClick={() => onOpenModel(model.id)}><span className="status-dot warning" /><span className="row-main"><strong>{model.id}</strong><span className="muted">{issue}</span></span></button></li>)}
          </ul>}
        </section>
        <section className="panel" aria-label="Recent requests">
          <header className="panel-head"><h2>Recent requests</h2><Button variant="ghost" onClick={onTest}>Try a model</Button></header>
          {snapshot.requests.length === 0 ? <p className="muted panel-empty">No requests yet.</p> : <ul className="row-list compact">
            {snapshot.requests.slice(0, 6).map((request) => <li key={request.id} className="request-line">
              <span className={`status-dot ${request.outcome === "completed" ? "good" : request.outcome === "cancelled" ? "" : "bad"}`} />
              <span className="row-main"><strong>{request.model}</strong><span className="muted">{request.operation}</span></span>
              <span className="row-meta"><span>{rate(request.decode_tokens_per_second)}</span><span className="muted">{request.elapsed_ms} ms</span></span>
            </li>)}
          </ul>}
        </section>
      </div>
    </div>
  </div>;
}
