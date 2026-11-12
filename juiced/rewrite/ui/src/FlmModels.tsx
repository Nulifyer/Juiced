import { useState } from "react";
import type { Client } from "./api";
import { policyNames, type Policy } from "./domain";
import { notify } from "./toast";
import { Badge, Button } from "./ui";
import { useResource } from "./useResource";

type Checkpoint = { checkpoint: string; installed: boolean; footprint_gb: number | null; kind: string; registered: string[] };
type Job = { checkpoint: string; state: string; percent: number | null; error: string | null };
type Flm = { available: boolean; problems: string[]; models: Checkpoint[]; jobs: Job[]; listError: string | null };

export function parseFlm(value: unknown): Flm {
  const body = value && typeof value === "object" ? value as Record<string, any> : {};
  const list = (items: unknown) => Array.isArray(items) ? items.slice(0, 512).filter((item) => item && typeof item === "object") : [];
  return {
    available: body.available === true,
    problems: Array.isArray(body.validation?.problems) ? body.validation.problems.filter((item: unknown): item is string => typeof item === "string").slice(0, 16) : [],
    models: list(body.models).map((item: any) => ({ checkpoint: String(item.checkpoint ?? ""), installed: item.installed === true, footprint_gb: typeof item.footprint_gb === "number" ? item.footprint_gb : null, kind: String(item.kind ?? "chat"), registered: Array.isArray(item.registered) ? item.registered.filter((id: unknown) => typeof id === "string") : [] })).filter((item) => item.checkpoint),
    jobs: list(body.jobs).map((item: any) => ({ checkpoint: String(item.checkpoint ?? ""), state: String(item.state ?? ""), percent: typeof item.percent === "number" ? item.percent : null, error: typeof item.error === "string" ? item.error : null })),
    listError: typeof body.list_error === "string" ? body.list_error : null,
  };
}

export function FlmModels({ client, online, query, onAdded }: { client: Client; online: boolean; query: string; onAdded: () => Promise<void> }) {
  const flm = useResource(client, "juiced/flm", parseFlm, 10000);
  const [policy, setPolicy] = useState<Policy>("on-demand");
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const data = flm.data;
  if (!data?.available) return null;
  const needle = query.trim().toLowerCase();
  const models = data.models.filter((item) => item.kind === "chat" && (!needle || item.checkpoint.toLowerCase().includes(needle)));
  async function act(checkpoint: string, path: string, body: unknown, success: string) {
    setBusy(checkpoint); setError("");
    try { await client.request(path, body); notify(success); await flm.reload(); await onAdded(); }
    catch (caught) { setError(caught instanceof Error ? caught.message : "The request failed."); }
    finally { setBusy(""); }
  }
  return <section className="panel flm-models" aria-label="NPU models">
    <header className="panel-head"><h3>NPU models</h3><span className="muted">FastFlowLM</span>
      <label className="inline-field">Add as <select aria-label="Add NPU models as" value={policy} onChange={(event) => setPolicy(event.target.value as Policy)}>{(["disabled", "on-demand", "hot"] as Policy[]).map((item) => <option key={item} value={item}>{policyNames[item]}</option>)}</select></label></header>
    {data.problems.map((problem) => <p className="inline-warning" key={problem}>{problem}</p>)}
    {(error || data.listError) && <p className="inline-error" role="alert">{error || "FastFlowLM couldn't list its models."}</p>}
    {models.length === 0 ? <p className="muted">{needle ? "No NPU models match." : "No NPU chat models listed."}</p> : <ul className="flm-list">{models.map((item) => {
      const job = data.jobs.find((entry) => entry.checkpoint === item.checkpoint && entry.state === "pulling");
      return <li key={item.checkpoint}>
        <strong className="break-word">{item.checkpoint}</strong>
        <span className="muted">{item.footprint_gb !== null ? `${item.footprint_gb.toFixed(1)} GB` : ""}</span>
        {item.registered.length > 0 ? <Badge tone="good">Added</Badge>
          : job ? <span className="muted">Downloading{job.percent !== null ? ` ${Math.round(job.percent)}%` : "…"}</span>
          : item.installed ? <Button variant="primary" disabled={!online || !!busy} onClick={() => void act(item.checkpoint, "juiced/flm/models/add", { checkpoint: item.checkpoint, policy }, `Added ${item.checkpoint} on the NPU.`)}>Add</Button>
          : <Button disabled={!online || !!busy} onClick={() => void act(item.checkpoint, "juiced/flm/pull", { checkpoint: item.checkpoint }, `Downloading ${item.checkpoint}.`)}>Download</Button>}
      </li>;
    })}</ul>}
  </section>;
}
