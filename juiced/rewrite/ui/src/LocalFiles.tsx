import { useEffect, useRef, useState } from "react";
import { ApiError, type Client } from "./api";
import { bytes } from "./domain";
import { Button, Dialog, Empty, Icon } from "./ui";

export type LocalFile = { name: string; bytes: number; sent: number | null; artifact?: string; path?: string; id: string; job?: string; error?: string };
type Entry = { name: string; path: string; kind: "directory" | "file"; bytes: number | null; split: boolean };
type Listing = { enabled: boolean; directory: string | null; parent: string | null; entries: Entry[]; truncated: boolean };
type Job = { state: string; downloaded_bytes: number; expected_bytes: number | null; artifact?: string | null; error?: string | null };
type Intake = { artifact?: string; bytes: number; file: string; suggested_id: string | null; job?: Job & { id: string } | null; add?: { path?: string; artifact?: string; id?: string } | null };

const stem = (name: string) => name.replace(/\.(gguf|bin|safetensors|onnx)$/i, "");
const failure = (error: unknown, fallback: string) => error instanceof ApiError && error.code === "split_model_unsupported" ? "Split GGUF files can't be added yet. Merge the parts with llama-gguf-split --merge, then add the merged file." : error instanceof ApiError && error.code === "projector_unsupported" ? "Vision projector files are added together with their model, which isn't supported for local files yet." : error instanceof Error ? error.message : fallback;

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
  async function upload(picked: File) {
    controller.current?.abort();
    const current = new AbortController(); controller.current = current;
    setFile({ name: picked.name, bytes: picked.size, sent: 0, id: stem(picked.name) });
    try {
      const reply = await client.upload<Intake>(`juiced/library/upload?filename=${encodeURIComponent(picked.name)}&size=${picked.size}`, picked, (sent) => setFile((value) => value && value.name === picked.name ? { ...value, sent } : value), current.signal);
      setFile({ name: reply.file, bytes: reply.bytes, sent: reply.bytes, artifact: reply.artifact, id: reply.suggested_id ?? stem(reply.file) });
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") return;
      setFile((value) => value ? { ...value, error: failure(error, "The upload failed.") } : value);
    } finally { if (controller.current === current) controller.current = null; }
  }
  async function pick(entry: Entry, mode: "link" | "copy") {
    setFile({ name: entry.name, bytes: entry.bytes ?? 0, sent: mode === "copy" ? 0 : null, id: stem(entry.name) });
    try {
      const reply = await client.request<Intake>("juiced/library/import-path", { path: entry.path, mode });
      const id = reply.suggested_id ?? stem(reply.file);
      setFile(mode === "link" ? { name: reply.file, bytes: reply.bytes, sent: null, path: reply.add?.path ?? entry.path, id } : { name: reply.file, bytes: reply.bytes, sent: 0, job: reply.job?.id, id });
    } catch (error) { setFile((value) => value ? { ...value, error: failure(error, "The file couldn't be imported.") } : value); }
  }
  function clear() { controller.current?.abort(); controller.current = null; setFile(null); }
  return { file, upload, pick, clear, rename: (id: string) => setFile((value) => value ? { ...value, id } : value) };
}

export function ServerFolder({ client, onPick, onClose }: { client: Client; onPick: (entry: Entry, mode: "link" | "copy") => void; onClose: () => void }) {
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
  return <Dialog title="Add from the server" onClose={onClose}>
    {listing && !listing.enabled ? <Empty title="No import folder set">Set <code>import_roots</code> in the service config. The container uses <code>/data/import</code>.</Empty> : <>
      <div className="folder-path">{listing?.directory && <Button variant="ghost" aria-label="Up one folder" onClick={() => setDir(listing.parent ?? "")}><Icon name="back" size={14} /></Button>}<code className="break-word">{listing?.directory ?? "Import folders"}</code></div>
      {error && <p className="inline-error" role="alert">{error}</p>}
      <ul className="folder-list" aria-label="Server files">{(listing?.entries ?? []).map((entry) => <li key={entry.path}>
        {entry.kind === "directory" ? <button onClick={() => setDir(entry.path)}><Icon name="library" size={14} /><span className="break-word">{entry.name}</span></button>
          : <button disabled={entry.split} title={entry.split ? "Split GGUF parts can't be added yet" : undefined} onClick={() => onPick(entry, mode)}><span className="break-word">{entry.name}</span><span className="muted numeric">{bytes(entry.bytes)}</span></button>}
      </li>)}</ul>
      {listing && listing.entries.length === 0 && <p className="muted">No model files here.</p>}
      {listing?.truncated && <p className="field-help">Showing the first 1000 entries.</p>}
      <div className="add-policy"><span className="field-label" id="import-mode-label">Keep the file</span><div className="segmented" role="radiogroup" aria-labelledby="import-mode-label">{([["link", "Where it is"], ["copy", "Copy to library"]] as const).map(([value, label]) => <button key={value} role="radio" aria-checked={mode === value} onClick={() => setMode(value)}>{label}</button>)}</div></div>
      <p className="field-help">{mode === "link" ? "Uses the file in place. Moving or deleting it breaks the model." : "Copies it into Juiced's storage and checks its digest."}</p>
    </>}
  </Dialog>;
}

export function LocalProgress({ file, onCancel }: { file: LocalFile; onCancel: () => void }) {
  if (file.error) return <p className="inline-error" role="alert">{file.error}</p>;
  if (file.sent === null || file.artifact) return null;
  return <><progress aria-label={file.job ? "Copy progress" : "Upload progress"} max={file.bytes || 1} value={file.sent} />
    <div className="action-row"><span className="muted">{file.job ? "Copying" : "Uploading"}… {bytes(file.sent)} of {bytes(file.bytes)}</span>{!file.job && <Button onClick={onCancel}>Cancel</Button>}</div></>;
}
