import { useEffect, useId, useState, type ReactNode } from "react";
import { ApiError, type Client } from "./api";
import { bytes, policyNames, type Policy } from "./domain";
import { installEngine, parseEngines, type EngineJob } from "./engines";
import { backendNames } from "./hardware";
import { plainError } from "./modelErrors";
import { Button, HelpTip } from "./ui";

/** `projector` is an artifact for stored models and a path for models used in place. */
export type AddTarget = ({ path: string } | { artifact: string } | { artifact_set: string } | { parts: string[] }) & { projector?: string };
type Install = { engine: string; backend: string; size: number | null };

export const supportedFormats = "GGUF (llama.cpp, sd.cpp, parakeet.cpp, FUTO asr4all), whisper.cpp ggml, diffusion safetensors, Kokoro ONNX and Moonshine ORT.";

/** An add or import failure; the format list for an unknown file stays behind a tip. */
export function AddError({ error, code }: { error: string; code?: string }) {
  if (!error) return null;
  return <p className="inline-error" role="alert">{error}{code === "unsupported_model_format" && <> <HelpTip label="Supported formats">{supportedFormats}</HelpTip></>}</p>;
}

export const policyLabel = "Loading policy";

/** Starts a download that the service adds with `policy` once it finishes. */
export type DownloadThenAdd = { disabled: boolean; start: (policy: Policy) => Promise<void> };

/** Policy choice, engine install and the Add model action for one ready source, or the download that adds it. */
export function AddModel({ client, online, target, id, engine, ready, blocked, fallback, download, downloadLabel = "Download", onRegistered, onOpenModel, resetKey }: { client: Client; online: boolean; target: AddTarget | null; id: string; engine?: string; ready: boolean; blocked?: string; fallback?: ReactNode; download?: DownloadThenAdd; downloadLabel?: string; onRegistered: () => Promise<void>; onOpenModel: (id: string) => void; resetKey: string }) {
  const [needs, setNeeds] = useState<Install | null>(null);
  const [installing, setInstalling] = useState<EngineJob | null>(null);
  const [added, setAdded] = useState<{ model: string; backend: string; policy: Policy } | null>(null);
  const [policy, setPolicy] = useState<Policy>("on-demand");
  const [adding, setAdding] = useState(false);
  const [error, setError] = useState("");
  const [code, setCode] = useState<string | undefined>();
  const label = useId();
  useEffect(() => { setNeeds(null); setAdded(null); setError(""); setCode(undefined); }, [resetKey]);
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
      else {
        setCode(failure instanceof ApiError ? failure.code : undefined);
        setError(failure instanceof ApiError && failure.code === "model_exists" ? "A model already has this name. Choose another name." : plainError(failure, "The model couldn't be added."));
      }
    } finally { setAdding(false); }
  }
  if (added) return <div className="added"><p className="inline-success" role="status">Added {added.model}. {policyNames[added.policy]} on {backendNames[added.backend] ?? added.backend}.</p><div className="action-row"><Button variant="primary" onClick={() => onOpenModel(added.model)}>Open model</Button></div></div>;
  return <>
    {(ready || download) && <div className="add-policy"><span className="field-label" id={label}>{policyLabel}</span><div className="segmented" role="radiogroup" aria-labelledby={label}>{(["disabled", "on-demand", "hot"] as Policy[]).map((value) => <button key={value} role="radio" aria-checked={policy === value} disabled={adding} onClick={() => setPolicy(value)}>{policyNames[value]}</button>)}</div></div>}
    {needs ? <div className="needs-engine"><p className="field-help">Needs the {needs.engine} {backendNames[needs.backend] ?? needs.backend} engine{needs.size ? ` (${bytes(needs.size)})` : ""}.</p>
      {installing ? <><progress aria-label="Engine install progress" max={installing.total_bytes ?? 1} value={installing.total_bytes ? installing.downloaded_bytes : undefined} /><p className="field-help">Installing engine… {installing.state}</p></> : <Button variant="primary" disabled={!online || adding || !target} onClick={() => void add(needs)}>Install engine and add</Button>}</div>
      : <div className="action-row wrap">{ready ? <Button variant="primary" disabled={!online || adding || !target || !id || !!blocked} title={blocked} onClick={() => void add()}>{adding ? "Adding…" : "Add model"}</Button> : download ? <Button variant="primary" disabled={!online || adding || download.disabled} onClick={() => { setAdding(true); void download.start(policy).finally(() => setAdding(false)); }}>{downloadLabel}</Button> : fallback}</div>}
    <AddError error={error} code={code} />
  </>;
}
