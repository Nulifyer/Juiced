import { useEffect, useRef, useState } from "react";
import type { AddTarget } from "./AddModel";
import { ApiError, type Client } from "./api";
import { bytes } from "./domain";
import { groupEntries, planUpload, splitPart } from "./localFilesData";
import { Button, Dialog, Empty, Icon } from "./ui";

export type Projector = { name: string; path: string; bytes: number };
export type LocalFile = { name: string; bytes: number; sent: number | null; partCount: number; artifact?: string; parts?: string[]; path?: string; id: string; job?: string; error?: string; projector?: { name: string; artifact?: string; path?: string } | null; projectors?: Projector[] };
type Entry = { name: string; path: string; kind: "directory" | "file"; bytes: number | null; split: boolean };
type Listing = { enabled: boolean; directory: string | null; parent: string | null; entries: Entry[]; truncated: boolean };
type Job = { state: string; downloaded_bytes: number; expected_bytes: number | null; artifact?: string | null; error?: string | null };
type Intake = { artifact?: string; bytes: number; file: string; role?: "model" | "projector"; split?: { part: number; parts: number } | null; parts?: { name: string }[]; projectors?: Projector[]; suggested_id: string | null; job?: Job & { id: string } | null; add?: { path?: string; artifact?: string; id?: string } | null };

const projectorAlone = "This is a vision projector (mmproj). Pick its model file; Juiced attaches the projector to it.";
const stem = (name: string) => splitPart(name)?.prefix ?? name.replace(/\.(gguf|bin|safetensors|onnx)$/i, "");
const failure = (error: unknown, fallback: string) => error instanceof ApiError && error.code === "split_copy_unsupported" ? "Split models can't be copied. Pick it again with Where it is." : error instanceof Error ? error.message : fallback;

export function useLocalFile(client: Client) {
  const [file, setFile] = useState<LocalFile | null>(null);
  const controller = useRef<AbortController | null>(null);
  useEffect(() => () => controller.current?.abort(), []);
  useEffect(() => {
    if (!file?.job || file.artifact || file.error) return;
    const job = file.job;
    const timer = setInterval(async () => {
      try {
        const view = await client.request<Job>(`juiced/artifacts/jobs/${encodeURIComponent(job)}`);
        setFile((current) => current?.job !== job ? current : view.state === "complete" && view.artifact ? { ...current, sent: current.bytes, artifact: view.artifact } : ["failed", "cancelled", "quarantined"].includes(view.state) ? { ...current, error: "The copy didn't finish. Pick the file again to retry." } : { ...current, sent: view.downloaded_bytes });
      } catch { /* The next poll retries. */ }
    }, 1000);
    return () => clearInterval(timer);
  }, [client, file?.job, file?.artifact, file?.error]);
  /** Uploads one model, or every part of a split model, plus an optional projector, one file at a time. */
  async function upload(picked: File[]) {
    controller.current?.abort();
    const plan = planUpload(picked);
    if ("error" in plan) {
      controller.current = null;
      setFile({ name: picked[0]?.name ?? "Model file", bytes: 0, sent: null, partCount: 1, id: "", error: plan.error });
      return;
    }
    const current = new AbortController(); controller.current = current;
    const total = plan.files.reduce((sum, item) => sum + item.size, 0);
    const name = plan.label;
    setFile({ name, bytes: total, sent: 0, partCount: plan.parts, id: stem(name) });
    let done = 0, artifact: string | undefined, suggested: string | null = null;
    const parts: string[] = [];
    let projector: LocalFile["projector"] = null;
    try {
      for (const item of plan.files) {
        const reply = await client.upload<Intake>(`juiced/library/upload?filename=${encodeURIComponent(item.name)}&size=${item.size}`, item, (sent) => setFile((value) => value && value.name === name ? { ...value, sent: done + sent } : value), current.signal);
        done += item.size;
        if (reply.role === "projector") projector = { name: reply.file, artifact: reply.artifact };
        else if (reply.split) { if (reply.artifact) parts.push(reply.artifact); suggested ??= reply.suggested_id; }
        else { artifact = reply.artifact; suggested = reply.suggested_id; }
      }
      if (!artifact && parts.length === 0) throw new Error(projectorAlone);
      setFile({ name, bytes: total, sent: total, partCount: plan.parts, ...(parts.length ? { parts } : { artifact }), projector, id: suggested ?? stem(name) });
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") return;
      const kept = done > 0 && plan.files.length > 1 ? " Files already uploaded stay in the library; pick the same files again to finish." : "";
      setFile((value) => value ? { ...value, error: failure(error, "The upload failed.") + kept } : value);
    } finally { if (controller.current === current) controller.current = null; }
  }
  async function pick(entry: Entry & { parts?: number }, mode: "link" | "copy") {
    setFile({ name: entry.name, bytes: entry.bytes ?? 0, sent: mode === "copy" ? 0 : null, partCount: entry.parts ?? 1, id: stem(entry.name) });
    try {
      const reply = await client.request<Intake>("juiced/library/import-path", { path: entry.path, mode });
      if (reply.role === "projector") throw new Error(projectorAlone);
      const id = reply.suggested_id ?? stem(reply.file);
      const partCount = Math.max(reply.parts?.length ?? 0, 1);
      const projectors = reply.projectors ?? [];
      setFile(mode === "link" ? { name: reply.file, bytes: reply.bytes, sent: null, partCount, path: reply.add?.path ?? entry.path, id, projectors, projector: projectors[0] ? { name: projectors[0].name, path: projectors[0].path } : null } : { name: reply.file, bytes: reply.bytes, sent: 0, partCount, job: reply.job?.id, id });
    } catch (error) { setFile((value) => value ? { ...value, error: failure(error, "The file couldn't be imported.") } : value); }
  }
  function clear() { controller.current?.abort(); controller.current = null; setFile(null); }
  function chooseProjector(path: string) {
    setFile((value) => {
      const chosen = value?.projectors?.find((item) => item.path === path);
      return value ? { ...value, projector: chosen ? { name: chosen.name, path: chosen.path } : null } : value;
    });
  }
  return { file, upload, pick, clear, chooseProjector, rename: (id: string) => setFile((value) => value ? { ...value, id } : value) };
}

export function ServerFolder({ client, onPick, onClose }: { client: Client; onPick: (entry: Entry & { parts: number }, mode: "link" | "copy") => void; onClose: () => void }) {
  const [dir, setDir] = useState("");
  const [listing, setListing] = useState<Listing | null>(null);
  const [error, setError] = useState("");
  const [mode, setMode] = useState<"link" | "copy">("link");
  useEffect(() => {
    const controller = new AbortController();
    setError("");
    client.request<Listing>(`juiced/library/import-path${dir ? `?dir=${encodeURIComponent(dir)}` : ""}`, undefined, controller.signal)
      .then(setListing).catch((caught) => { if (!controller.signal.aborted) setError(failure(caught, "The folder couldn't be listed.")); });
    return () => controller.abort();
  }, [client, dir]);
  const entries = groupEntries(listing?.entries ?? []);
  return <Dialog title="Add from the server" onClose={onClose}>
    {listing && !listing.enabled ? <Empty title="No import folder set">Set <code>import_roots</code> in the service config. The container uses <code>/data/import</code>.</Empty> : <>
      <div className="folder-path">{listing?.directory && <Button variant="ghost" aria-label="Up one folder" onClick={() => setDir(listing.parent ?? "")}><Icon name="back" size={14} /></Button>}<code className="break-word">{listing?.directory ?? "Import folders"}</code></div>
      {error && <p className="inline-error" role="alert">{error}</p>}
      <ul className="folder-list" aria-label="Server files">{entries.map((entry) => <li key={entry.path}>
        {entry.kind === "directory" ? <button onClick={() => setDir(entry.path)}><Icon name="library" size={14} /><span className="break-word">{entry.name}</span></button>
          : <button onClick={() => onPick(entry, mode)}><span className="break-word">{entry.name}</span><span className="muted numeric">{entry.parts > 1 ? `${entry.parts} parts · ` : ""}{bytes(entry.bytes)}</span></button>}
      </li>)}</ul>
      {listing && entries.length === 0 && <p className="muted">No model files here.</p>}
      {listing?.truncated && <p className="field-help">Showing the first 1000 entries.</p>}
      <div className="add-policy"><span className="field-label" id="import-mode-label">Keep the file</span><div className="segmented" role="radiogroup" aria-labelledby="import-mode-label">{([["link", "Where it is"], ["copy", "Copy to library"]] as const).map(([value, label]) => <button key={value} role="radio" aria-checked={mode === value} onClick={() => setMode(value)}>{label}</button>)}</div></div>
      <p className="field-help">{mode === "link" ? "Uses the file in place. Moving or deleting it breaks the model." : "Copies it into Juiced's storage and checks its digest. Split models stay in place."}</p>
    </>}
  </Dialog>;
}

export function localTarget(file: LocalFile): AddTarget | null {
  const projector = file.path ? file.projector?.path : file.projector?.artifact;
  const extra = projector ? { projector } : {};
  if (file.parts) return { parts: file.parts, ...extra };
  if (file.artifact) return { artifact: file.artifact, ...extra };
  if (file.path) return { path: file.path, ...extra };
  return null;
}

/** Picks the vision projector found beside a linked model. */
export function ProjectorChoice({ file, onChoose }: { file: LocalFile; onChoose: (path: string) => void }) {
  if (file.projectors?.length) return <div className="add-name"><label htmlFor="local-projector">Vision projector</label>
    <select id="local-projector" value={file.projector?.path ?? ""} onChange={(event) => onChoose(event.target.value)}>
      {file.projectors.map((item) => <option key={item.path} value={item.path}>{item.name} ({bytes(item.bytes)})</option>)}
      <option value="">None (text only)</option>
    </select></div>;
  return null;
}

export function LocalProgress({ file, onCancel }: { file: LocalFile; onCancel: () => void }) {
  if (file.error) return <p className="inline-error" role="alert">{file.error}</p>;
  if (file.sent === null || file.artifact || file.parts) return null;
  return <><progress aria-label={file.job ? "Copy progress" : "Upload progress"} max={file.bytes || 1} value={file.sent} />
    <div className="action-row"><span className="muted">{file.job ? "Copying" : "Uploading"}… {bytes(file.sent)} of {bytes(file.bytes)}</span>{!file.job && <Button onClick={onCancel}>Cancel</Button>}</div></>;
}
