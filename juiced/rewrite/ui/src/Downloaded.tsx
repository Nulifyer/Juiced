import { useEffect, useMemo, useState } from "react";
import type { Client } from "./api";
import { bytes } from "./domain";
import { AddModel } from "./AddModel";
import { parseLocal, quantRefusal, roleNames, storageNames, type LocalGroup, type LocalQuant } from "./localLibraryData";
import { notify } from "./toast";
import { Badge, Button, Dialog, Empty, Icon } from "./ui";
import { useResource } from "./useResource";

const modelName = (file: string) => (file.split(/[\\/]/).pop() ?? file).replace(/\.(gguf|bin|safetensors|onnx|ort)$/i, "").replace(/-0*1-of-\d+$/i, "").trim();
const plural = (count: number, word: string) => `${count} ${word}${count === 1 ? "" : "s"}`;

function UsedBy({ ids, onOpenModel }: { ids: string[]; onOpenModel: (id: string) => void }) {
  if (ids.length === 0) return <span className="muted">No model</span>;
  return <span className="used-by">{ids.map((id) => <button key={id} className="link-button" onClick={() => onOpenModel(id)}>{id}</button>)}</span>;
}

export function Downloaded({ client, online, onRegistered, onOpenModel, onSearch }: { client: Client; online: boolean; onRegistered: () => Promise<void>; onOpenModel: (id: string) => void; onSearch: () => void }) {
  const [fresh, setFresh] = useState(true);
  const local = useResource(client, fresh ? "juiced/library/local?fresh=true" : "juiced/library/local", parseLocal, 5000, true);
  useEffect(() => { if (local.data) setFresh(false); }, [local.data]);
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<{ group: string; quant: string } | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState("");
  const [name, setName] = useState("");
  const data = local.data;
  const groups = useMemo(() => {
    const terms = filter.trim().toLowerCase().split(/\s+/).filter(Boolean);
    return (data?.groups ?? []).filter((group) => terms.every((term) => `${group.name} ${group.publisher ?? ""} ${group.quants.map((quant) => quant.label).join(" ")} ${group.usedBy.join(" ")}`.toLowerCase().includes(term)));
  }, [data, filter]);
  const group = data?.groups.find((item) => item.id === selected?.group);
  const quant = group?.quants.find((item) => item.file === selected?.quant) ?? (group && !selected?.quant ? group.quants[0] : undefined);
  const primary = group?.files.find((file) => file.id === quant?.file);
  useEffect(() => { setName(primary ? modelName(primary.name) : ""); setDeleteError(""); }, [primary?.id]);
  useEffect(() => { if (selected && data && !group) setSelected(null); }, [data, group, selected]);
  const choose = (next: LocalGroup, item?: LocalQuant) => setSelected({ group: next.id, quant: item?.file ?? next.quants[0]?.file ?? "" });
  async function remove() {
    if (!group || !quant) return;
    setDeleting(true); setDeleteError("");
    try {
      const reply = await client.request<{ freed_bytes?: number }>("juiced/library/local/delete", { files: quant.files });
      setConfirming(false);
      notify(`Deleted ${group.name} ${quant.label}. Freed ${bytes(reply.freed_bytes ?? quant.bytes)}.`);
      setSelected(group.quants.length > 1 ? { group: group.id, quant: "" } : null);
      await local.reload();
    } catch (failure) { setDeleteError(failure instanceof Error ? failure.message : "The files couldn't be deleted."); }
    finally { setDeleting(false); }
  }
  const companions = group?.files.filter((file) => file.role !== "model" && file.role !== "shard") ?? [];
  const refusal = group && quant ? quantRefusal(group, quant) : undefined;
  return <div className={`find-models ${group ? "has-detail" : ""}`}>
    <div className="find-main">
      <div className="toolbar">
        <input type="search" aria-label="Filter downloaded models" placeholder="Filter by name, quant or model" value={filter} onChange={(event) => setFilter(event.target.value)} />
        {data && <span className="muted library-total" role="status">{plural(data.groups.length, "model")} · {bytes(data.totalBytes)}{data.removableBytes > 0 ? ` · ${bytes(data.removableBytes)} can be deleted` : ""}</span>}
      </div>
      {local.error && <div className="action-row"><p className="inline-error" role="alert">{data ? "Showing the last observation. " : ""}{local.error}</p><Button onClick={() => void local.reload()}>Retry</Button></div>}
      {data?.truncated && <p className="field-help">Showing the first files only. Large folders are listed up to a limit.</p>}
      {!data ? <p className="muted" role="status">{local.loading ? "Looking for model files…" : ""}</p>
        : data.groups.length === 0 ? <Empty title="Nothing downloaded yet">Model files on this machine show here. <button className="link-button" onClick={onSearch}>Search for a model</button></Empty>
        : groups.length === 0 ? <Empty title="No matches">Try a shorter filter.</Empty>
        : <ul className="model-results downloaded-list" aria-label="Downloaded models">
          {groups.map((item) => <li key={item.id} className={`model-result ${item.id === group?.id ? "selected" : ""}`}>
            <div className="result-head">
              <button className="result-title" onClick={() => choose(item)}><strong className="break-word">{item.name}</strong></button>
              {item.readOnly && <Badge>Read-only</Badge>}
              <span className="muted numeric result-size">{bytes(item.bytes)}</span>
              <span className="muted result-meta">{[item.publisher, item.usedBy.length ? `used by ${plural(item.usedBy.length, "model")}` : "not used"].filter(Boolean).join(" · ")}</span>
            </div>
            <div className="quant-chips" role="group" aria-label={`${item.name} quants`}>
              {item.quants.length === 0 ? <button className="quant-chip more" onClick={() => choose(item)}>{plural(item.files.length, "file")}</button>
                : item.quants.map((entry) => <button key={entry.file} className={`quant-chip ${entry.usedBy.length ? "present" : ""}`} aria-pressed={item.id === group?.id && entry.file === quant?.file} onClick={() => choose(item, entry)} title={entry.usedBy.length ? `Used by ${entry.usedBy.join(", ")}` : "Not used by a model"}>
                  {entry.usedBy.length > 0 && <span aria-label="In use">● </span>}{entry.label}<span className="muted"> {bytes(entry.bytes)}</span>
                </button>)}
            </div>
          </li>)}
        </ul>}
    </div>
    {group && <aside className="inspector find-detail" aria-label="Downloaded model detail">
      <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={() => setSelected(null)}><Icon name="back" /> Back to downloaded</Button><Button variant="ghost" className="close-inspector" aria-label="Close detail" onClick={() => setSelected(null)}><Icon name="close" /></Button></div>
      <div className="inspector-body">
        <div className="inspector-title"><h2 className="break-word">{group.name}</h2>{group.readOnly && <Badge>Read-only</Badge>}</div>
        <p className="field-help">{[group.publisher, ...group.storage.map((value) => storageNames[value])].filter(Boolean).join(" · ")}</p>
        {group.quants.length > 0 && <div className="quant-list" role="listbox" aria-label="Quants on disk">{group.quants.map((entry) => <button key={entry.file} role="option" aria-selected={entry.file === quant?.file} className="quant-option" onClick={() => choose(group, entry)}>
          <span className={`status-dot ${entry.usedBy.length ? "good" : "quiet"}`} title={entry.usedBy.length ? "In use" : "Not used"} /><strong>{entry.label}</strong>{entry.parts > 1 && <span className="muted">{entry.parts} parts</span>}<span className="numeric muted">{bytes(entry.bytes)}</span>
        </button>)}</div>}
        <dl className="facts">
          {quant && <div><dt>Used by</dt><dd><UsedBy ids={quant.usedBy} onOpenModel={onOpenModel} /></dd></div>}
          <div><dt>Total</dt><dd>{bytes(group.bytes)}</dd></div>
          {(group.location ?? primary?.location) && <div className="fact-path"><dt>Location</dt><dd><code className="break-word">{group.location ?? primary?.location}</code></dd></div>}
        </dl>
        {companions.length > 0 && <ul className="file-list" aria-label="Other files">{companions.map((file) => <li key={file.id}><span className="break-word">{file.name}</span><span className="muted">{roleNames[file.role] ?? file.role}</span><span className="muted numeric">{bytes(file.bytes)}</span></li>)}</ul>}
        {quant && <>
          {quant.add && <div className="add-name"><label htmlFor="downloaded-model-id">Model name</label><input id="downloaded-model-id" value={name} maxLength={128} onChange={(event) => setName(event.target.value)} /></div>}
          {!quant.add && <p className="field-help">{quant.parts > 1 ? "Split models can't be added yet. Merge the parts with llama-gguf-split --merge." : "This file can't be added on its own."}</p>}
          <AddModel client={client} online={online} target={quant.add} id={name.trim()} ready={!!quant.add} resetKey={`${group.id}:${quant.file}`} onRegistered={async () => { await onRegistered(); await local.reload(); }} onOpenModel={onOpenModel} />
          <div className="action-row wrap delete-row"><Button variant="danger" disabled={!online || !quant.removable || deleting} title={refusal} onClick={() => { setDeleteError(""); setConfirming(true); }}>Delete {quant.label}</Button></div>
          {refusal && <p className="field-help">{refusal}</p>}
        </>}
      </div>
    </aside>}
    {confirming && group && quant && <Dialog title={`Delete ${group.name} ${quant.label}?`} onClose={() => setConfirming(false)}>
      <p>This removes {plural(quant.files.length, "file")} ({bytes(quant.bytes)}) from disk. Download it again from Search if you need it later.</p>
      {deleteError && <p className="inline-error" role="alert">{deleteError}</p>}
      <div className="dialog-actions"><Button onClick={() => setConfirming(false)}>Cancel</Button><Button variant="danger" disabled={deleting} onClick={() => void remove()}>{deleting ? "Deleting…" : "Delete"}</Button></div>
    </Dialog>}
  </div>;
}
