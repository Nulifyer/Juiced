import { useState } from "react";
import type { Client } from "./api";
import { bytes, policyNames, type Policy } from "./domain";
import { policyLabel } from "./AddModel";
import { notify } from "./toast";
import { Badge, Button } from "./ui";
import { useResource } from "./useResource";

type Checkpoint = { checkpoint: string; installed: boolean; footprint_gb: number | null; kind: string; registered: string[] };
type Then = { policy: Policy; state: "pending" | "done" | "failed"; model: string | null; message: string | null };
type Job = { checkpoint: string; state: string; percent: number | null; error: string | null; then: Then | null };
export type Flm = { available: boolean; problems: string[]; models: Checkpoint[]; jobs: Job[]; listError: string | null };

const policies: Policy[] = ["disabled", "on-demand", "hot"];
const policy = (value: unknown): Policy => policies.includes(value as Policy) ? value as Policy : "on-demand";

export function parseFlm(value: unknown): Flm {
  const body = value && typeof value === "object" ? value as Record<string, any> : {};
  const list = (items: unknown) => Array.isArray(items) ? items.slice(0, 512).filter((item) => item && typeof item === "object") : [];
  const then = (item: any): Then | null => item && typeof item === "object" ? { policy: policy(item.policy), state: item.state === "done" || item.state === "failed" ? item.state : "pending", model: typeof item.model === "string" ? item.model : null, message: typeof item.message === "string" ? item.message : null } : null;
  return {
    available: body.available === true,
    problems: Array.isArray(body.validation?.problems) ? body.validation.problems.filter((item: unknown): item is string => typeof item === "string").slice(0, 16) : [],
    models: list(body.models).map((item: any) => ({ checkpoint: String(item.checkpoint ?? ""), installed: item.installed === true, footprint_gb: typeof item.footprint_gb === "number" ? item.footprint_gb : null, kind: String(item.kind ?? "chat"), registered: Array.isArray(item.registered) ? item.registered.filter((id: unknown) => typeof id === "string") : [] })).filter((item) => item.checkpoint),
    jobs: list(body.jobs).map((item: any) => ({ checkpoint: String(item.checkpoint ?? ""), state: String(item.state ?? ""), percent: typeof item.percent === "number" ? item.percent : null, error: typeof item.error === "string" ? item.error : null, then: then(item.then) })),
    listError: typeof body.list_error === "string" ? body.list_error : null,
  };
}

export function useFlm(client: Client) {
  return useResource(client, "juiced/flm", parseFlm, 10000);
}

/** FastFlowLM's footprint is in GiB, the same unit the service reserves for it. */
const footprint = (gib: number | null) => gib === null ? "" : bytes(gib * 1024 ** 3);

export function FlmModels({ client, online, query, flm, onAdded }: { client: Client; online: boolean; query: string; flm: ReturnType<typeof useFlm>; onAdded: () => Promise<void> }) {
  const [choice, setChoice] = useState<Policy>("on-demand");
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const data = flm.data;
  if (!data?.available) return null;
  const needle = query.trim().toLowerCase();
  const models = data.models.filter((item) => item.kind === "chat" && (!needle || item.checkpoint.toLowerCase().includes(needle)));
  if (needle && models.length === 0 && !error && !data.listError) return null;
  async function act(checkpoint: string, path: string, body: unknown, success: string) {
    setBusy(checkpoint); setError("");
    try { await client.request(path, body); notify(success); await flm.reload(); await onAdded(); }
    catch (caught) { setError(caught instanceof Error ? caught.message : "The request failed."); }
    finally { setBusy(""); }
  }
  return <section className="panel flm-models" aria-label="NPU models">
    <header className="panel-head"><h3>NPU models</h3><span className="muted">FastFlowLM</span>
      <label className="inline-field">{policyLabel} <select aria-label={`${policyLabel} for NPU models`} value={choice} onChange={(event) => setChoice(event.target.value as Policy)}>{policies.map((item) => <option key={item} value={item}>{policyNames[item]}</option>)}</select></label></header>
    {data.problems.map((problem) => <p className="inline-warning" key={problem}>{problem}</p>)}
    {(error || data.listError) && <p className="inline-error" role="alert">{error || "FastFlowLM couldn't list its models."}</p>}
    {models.length === 0 ? <p className="muted">No NPU chat models listed.</p> : <ul className="flm-list">{models.map((item) => {
      const job = [...data.jobs].reverse().find((entry) => entry.checkpoint === item.checkpoint);
      const pulling = job?.state === "pulling";
      const adding = job?.state === "complete" && job.then?.state === "pending";
      return <li key={item.checkpoint}>
        <strong className="break-word">{item.checkpoint}</strong>
        <span className="muted numeric">{footprint(item.footprint_gb)}</span>
        {item.registered.length > 0 ? <Badge tone="good">Added</Badge>
          : pulling ? <span className="muted">Downloading{job.percent !== null ? ` ${Math.round(job.percent)}%` : "…"}{job.then ? ` · then ${policyNames[job.then.policy]}` : ""}</span>
          : adding ? <span className="muted">Adding…</span>
          : item.installed ? <Button variant="primary" disabled={!online || !!busy} onClick={() => void act(item.checkpoint, "juiced/flm/models/add", { checkpoint: item.checkpoint, policy: choice }, `Added ${item.checkpoint} on the NPU.`)}>Add</Button>
          : <Button disabled={!online || !!busy} onClick={() => void act(item.checkpoint, "juiced/flm/pull", { checkpoint: item.checkpoint, then: { policy: choice } }, `Downloading ${item.checkpoint}. It's added when the download finishes.`)}>Download</Button>}
        {job?.then?.state === "failed" && !item.registered.length && <p className="inline-warning flm-note">{job.then.message ?? "The model wasn't added."}</p>}
      </li>;
    })}</ul>}
  </section>;
}
