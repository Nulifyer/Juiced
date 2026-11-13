import { useEffect, useId, useState, type ReactNode } from "react";
import { ApiError, type Client } from "./api";
import { bytes, policyNames, type Policy } from "./domain";
import { installEngine, parseEngines, type EngineJob } from "./engines";
import { backendNames } from "./hardware";
import { Button } from "./ui";

export type AddTarget = { path: string } | { artifact: string } | { artifact_set: string };
type Install = { engine: string; backend: string; size: number | null };

/** Policy choice, engine install and the Add model action for one ready source. */
export function AddModel({ client, online, target, id, engine, ready, blocked, fallback, onRegistered, onOpenModel, resetKey }: { client: Client; online: boolean; target: AddTarget | null; id: string; engine?: string; ready: boolean; blocked?: string; fallback?: ReactNode; onRegistered: () => Promise<void>; onOpenModel: (id: string) => void; resetKey: string }) {
  const [needs, setNeeds] = useState<Install | null>(null);
  const [installing, setInstalling] = useState<EngineJob | null>(null);
  const [added, setAdded] = useState<{ model: string; backend: string; policy: Policy } | null>(null);
  const [policy, setPolicy] = useState<Policy>("on-demand");
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState("");
  const label = useId();
  useEffect(() => { setNeeds(null); setAdded(null); setError(""); }, [resetKey]);
  async function add(install?: Install) {
    if (!target) return;
    setAdding(true); setError("");
    try {
      if (install) await installEngine(client, install.engine, install.backend, setInstalling);
      setInstalling(null);
      const response = await client.request<{ model: string | { id?: string }; backend: string }>("juiced/models/add", { ...target, id, policy, ...(engine ? { engine } : {}) });
      const created = typeof response.model === "string" ? response.model : response.model?.id ?? "the model";
      setNeeds(null); setAdded({ model: created, backend: response.backend, policy });
      await onRegistered();
    } catch (failure) {
      setInstalling(null);
      const body = failure instanceof ApiError ? failure.body as { install?: { engine?: string; backend?: string } } | undefined : undefined;
      if (failure instanceof ApiError && failure.code === "engine_not_installed" && body?.install?.engine && body.install.backend) {
        const { engine, backend } = body.install;
        const listed = await client.request("juiced/engines").then(parseEngines).catch(() => null);
        setNeeds({ engine, backend, size: listed?.installable.find((row) => row.engine === engine && row.backend === backend)?.archive_bytes ?? null });
      }
      else setError(failure instanceof ApiError && failure.code === "model_exists" ? "A model with this name is already added. Rename or remove it under Models first." : failure instanceof Error ? failure.message : "The model couldn't be added.");
    } finally { setAdding(false); }
  }
  if (added) return <div className="added"><p className="inline-success" role="status">Added {added.model}. It runs on {backendNames[added.backend] ?? added.backend} and {added.policy === "hot" ? "loads now and stays loaded" : added.policy === "on-demand" ? "loads on its first request" : "stays Disabled until you enable it in Models"}.</p><div className="action-row"><Button variant="primary" onClick={() => onOpenModel(added.model)}>Open model</Button></div></div>;
  return <>
    {ready && <div className="add-policy"><span className="field-label" id={label}>Make available as</span><div className="segmented" role="radiogroup" aria-labelledby={label}>{(["disabled", "on-demand", "hot"] as Policy[]).map((value) => <button key={value} role="radio" aria-checked={policy === value} disabled={adding} onClick={() => setPolicy(value)}>{policyNames[value]}</button>)}</div></div>}
    {needs ? <div className="needs-engine"><p className="field-help">This model needs the {needs.engine} {backendNames[needs.backend] ?? needs.backend} engine{needs.size ? ` (${bytes(needs.size)} download)` : ""}. Juiced picked it for this machine.</p>
      {installing ? <><progress aria-label="Engine install progress" max={installing.total_bytes ?? 1} value={installing.total_bytes ? installing.downloaded_bytes : undefined} /><p className="field-help">Installing engine… {installing.state}</p></> : <Button variant="primary" disabled={!online || adding || !target} onClick={() => void add(needs)}>Install engine and add</Button>}</div>
      : <div className="action-row wrap">{ready ? <Button variant="primary" disabled={!online || adding || !target || !id || !!blocked} title={blocked} onClick={() => void add()}>{adding ? "Adding…" : "Add model"}</Button> : fallback}</div>}
    {error && <p className="inline-error" role="alert">{error}</p>}
  </>;
}
