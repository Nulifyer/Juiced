import { useEffect, useState } from "react";
import type { Client } from "./api";
import { bytes, type Model } from "./domain";
import { AddModel } from "./AddModel";
import { parseLocal, quantRefusal, roleNames, storageNames, type LocalFile, type LocalGroup, type LocalInventory } from "./localLibraryData";
import { notify } from "./toast";
import { Badge, Button, Dialog, Icon } from "./ui";
import { useResource } from "./useResource";

const plural = (count: number, word: string) => `${count} ${word}${count === 1 ? "" : "s"}`;
const modelName = (file: string) => (file.split(/[\\/]/).pop() ?? file).replace(/\.(gguf|bin|safetensors|onnx|ort)$/i, "").replace(/-0*1-of-\d+$/i, "").trim();

export function useLocalFiles(client: Client, enabled: boolean) {
  const [fresh, setFresh] = useState(true);
  useEffect(() => { if (!enabled) setFresh(true); }, [enabled]);
  const local = useResource(client, fresh ? "juiced/library/local?fresh=true" : "juiced/library/local", parseLocal, 5000, true, enabled);
  useEffect(() => { if (local.data) setFresh(false); }, [local.data]);
  return local;
}

export const unusedQuants = (group: LocalGroup) => group.quants.filter((quant) => quant.usedBy.length === 0);
export const notAdded = (inventory: LocalInventory | null) => (inventory?.groups ?? []).filter((group) => group.quants.length ? unusedQuants(group).length > 0 : group.usedBy.length === 0);

function UsedBy({ ids, onOpenModel }: { ids: string[]; onOpenModel: (id: string) => void }) {
  if (ids.length === 0) return <span className="muted">No model</span>;
  return <span className="used-by">{ids.map((id) => <button key={id} className="link-button" onClick={() => onOpenModel(id)}>{id}</button>)}</span>;
}

export function FileInspector({ group, client, online, onClose, onRegistered, onOpenModel, onChanged }: { group: LocalGroup; client: Client; online: boolean; onClose: () => void; onRegistered: () => Promise<void>; onOpenModel: (id: string) => void; onChanged: () => Promise<unknown> }) {
  const [picked, setPicked] = useState(() => (unusedQuants(group)[0] ?? group.quants[0])?.file ?? "");
  const [confirming, setConfirming] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState("");
  const quant = group.quants.find((item) => item.file === picked) ?? group.quants[0];
  const primary = group.files.find((file) => file.id === quant?.file);
  const [names, setNames] = useState<Record<string, string>>({});
  const name = quant ? names[quant.file] ?? (primary ? modelName(primary.name) : "") : "";
  const companions = group.files.filter((file) => file.role !== "model" && file.role !== "shard");
  const refusal = quant ? quantRefusal(group, quant) : undefined;
  const added = group.quants.length ? unusedQuants(group).length === 0 : group.usedBy.length > 0;
  async function remove() {
    if (!quant) return;
    setDeleting(true); setDeleteError("");
    try {
      const reply = await client.request<{ freed_bytes?: number }>("juiced/library/local/delete", { files: quant.files });
      setConfirming(false);
      notify(`Deleted ${group.name} ${quant.label}. Freed ${bytes(reply.freed_bytes ?? quant.bytes)}.`);
      const rest = group.quants.filter((item) => item.file !== quant.file);
      if (rest.length) setPicked((rest.find((item) => item.usedBy.length === 0) ?? rest[0]!).file); else onClose();
      await onChanged();
    } catch (failure) { setDeleteError(failure instanceof Error ? failure.message : "The files couldn't be deleted."); }
    finally { setDeleting(false); }
  }
  return <aside className="inspector" aria-label="File details">
    <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={onClose}><Icon name="back" /> Back to models</Button><Button variant="ghost" className="close-inspector" aria-label="Close file details" onClick={onClose}><Icon name="close" /></Button></div>
    <div className="inspector-body">
      <div className="inspector-title"><h2 className="break-word">{group.name}</h2>{!added && <Badge>Not added</Badge>}</div>
      <p className="field-help">{[group.publisher, ...group.storage.map((value) => storageNames[value]), group.readOnly ? "Read-only" : null].filter(Boolean).join(" · ")}</p>
      {group.quants.length > 0 && <div className="quant-list" role="listbox" aria-label="Quants on disk">{group.quants.map((entry) => <button key={entry.file} role="option" aria-selected={entry.file === quant?.file} className="quant-option" onClick={() => { setPicked(entry.file); setDeleteError(""); }}>
        <span className={`status-dot ${entry.usedBy.length ? "good" : "quiet"}`} title={entry.usedBy.length ? "In use" : "Not used"} /><strong>{entry.label}</strong>{entry.parts > 1 && <span className="muted">{entry.parts} parts</span>}<span className="numeric muted">{bytes(entry.bytes)}</span>
      </button>)}</div>}
      <dl className="facts">
        {quant && <div><dt>Used by</dt><dd><UsedBy ids={quant.usedBy} onOpenModel={onOpenModel} /></dd></div>}
        <div><dt>Total</dt><dd>{bytes(group.bytes)}</dd></div>
        {(group.location ?? primary?.location) && <div className="fact-path"><dt>Location</dt><dd><code className="break-word">{group.location ?? primary?.location}</code></dd></div>}
      </dl>
      {companions.length > 0 && <ul className="file-list" aria-label="Other files">{companions.map((file) => <li key={file.id}><span className="break-word">{file.name}</span><span className="muted">{roleNames[file.role] ?? file.role}</span><span className="muted numeric">{bytes(file.bytes)}</span></li>)}</ul>}
      {quant && <>
        {quant.add && <div className="add-name"><label htmlFor="file-model-id">Model name</label><input id="file-model-id" value={name} maxLength={128} onChange={(event) => { const value = event.target.value; setNames((old) => ({ ...old, [quant.file]: value })); }} /></div>}
        {!quant.add && <p className="field-help">{quant.parts > 1 ? "Split models can't be added yet. Merge the parts with llama-gguf-split --merge." : "This file can't be added on its own."}</p>}
        <AddModel client={client} online={online} target={quant.add} id={name.trim()} ready={!!quant.add} resetKey={`${group.id}:${quant.file}`} onRegistered={async () => { await onRegistered(); await onChanged(); }} onOpenModel={onOpenModel} />
        <div className="action-row wrap delete-row"><Button variant="danger" disabled={!online || !quant.removable || deleting} title={refusal} onClick={() => { setDeleteError(""); setConfirming(true); }}>Delete {quant.label}</Button></div>
        {refusal && <p className="field-help">{refusal}</p>}
      </>}
    </div>
    {confirming && quant && <Dialog title={`Delete ${group.name} ${quant.label}?`} onClose={() => setConfirming(false)}>
      <p>This removes {plural(quant.files.length, "file")} ({bytes(quant.bytes)}) from disk. Download it again from Discover if you need it later.</p>
      {deleteError && <p className="inline-error" role="alert">{deleteError}</p>}
      <div className="dialog-actions"><Button onClick={() => setConfirming(false)}>Cancel</Button><Button variant="danger" disabled={deleting} onClick={() => void remove()}>{deleting ? "Deleting…" : "Delete"}</Button></div>
    </Dialog>}
  </aside>;
}

export function modelFiles(model: Model, inventory: LocalInventory | null): { groups: LocalGroup[]; files: LocalFile[] } {
  const groups: LocalGroup[] = [], files: LocalFile[] = [];
  for (const group of inventory?.groups ?? []) {
    const parts = new Set(group.quants.filter((quant) => quant.usedBy.includes(model.id)).flatMap((quant) => quant.files));
    const used = group.files.filter((file) => file.usedBy.includes(model.id) || parts.has(file.id));
    if (used.length) { groups.push(group); files.push(...used); }
  }
  return { groups, files };
}

export function removalNote(model: Model, inventory: LocalInventory | null): string {
  const { files } = modelFiles(model, inventory);
  if (!files.length) return "Removes the model. Its files stay on disk.";
  if (files.some((file) => file.readOnly)) return "Removes the model. Its files are in a read-only folder and stay there.";
  if (files.some((file) => file.usedBy.some((id) => id !== model.id))) return "Removes the model. Its files stay on disk because other models use them.";
  return "Removes the model. Its files stay on disk and show as Not added, where you can delete them.";
}

export function ModelFiles({ model, inventory }: { model: Model; inventory: LocalInventory | null }) {
  const { groups, files } = modelFiles(model, inventory);
  if (!files.length) return null;
  const others = [...new Set(files.flatMap((file) => file.usedBy))].filter((id) => id !== model.id);
  const location = groups.length === 1 ? groups[0]!.location ?? files[0]!.location : null;
  return <section className="inspector-section"><h3>Files</h3>
    <ul className="file-list" aria-label="Model files">{files.map((file) => <li key={file.id}><span className="break-word">{file.name}</span><span className="muted">{file.quant ?? roleNames[file.role] ?? file.role}</span><span className="muted numeric">{bytes(file.bytes)}</span></li>)}</ul>
    <dl className="facts"><div><dt>On disk</dt><dd>{bytes(files.reduce((sum, file) => sum + file.bytes, 0))}{files.some((file) => file.readOnly) ? " · read-only" : ""}</dd></div>
      {location && <div className="fact-path"><dt>Location</dt><dd><code className="break-word">{location}</code></dd></div>}</dl>
    {others.length > 0 && <p className="field-help">Also used by {others.join(", ")}.</p>}
  </section>;
}
