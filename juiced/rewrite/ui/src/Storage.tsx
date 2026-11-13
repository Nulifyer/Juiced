import { useState } from "react";
import type { Client } from "./api";
import { Meter } from "./charts";
import { bytes } from "./domain";
import { kindNames, parsePreview, place, parseStorage, reclaimRows, roleNames, type ReclaimPreview, type Reclaimable, type StorageView, type Volume } from "./storageData";
import { notify } from "./toast";
import { Badge, Button, Dialog, HelpTip } from "./ui";
import { useResource } from "./useResource";
import "./storage.css";

export function Storage({ client, online }: { client: Client; online: boolean }) {
  const view = useResource(client, "juiced/storage", parseStorage, 15000, true);
  const [chosen, setChosen] = useState<string[]>([]);
  const [preview, setPreview] = useState<ReclaimPreview | null>(null);
  const [busy, setBusy] = useState(false);
  const data = view.data;
  if (!data) return view.error ? <div className="action-row"><p className="inline-error" role="alert">{view.error}</p><Button onClick={() => void view.reload()}>Retry</Button></div> : <p className="muted" role="status">Measuring storage…</p>;
  const rows = reclaimRows(data);
  const selected = chosen.filter((id) => rows.some((row) => row.id === id && row.reclaimable));
  const selectedBytes = rows.filter((row) => selected.includes(row.id)).reduce((sum, row) => sum + row.bytes, 0);
  const toggle = (id: string) => setChosen((current) => current.includes(id) ? current.filter((known) => known !== id) : [...current, id]);
  async function review() {
    setBusy(true);
    try { setPreview(parsePreview(await client.request("juiced/storage/reclaim", { items: selected, preview: true }))); }
    catch (failure) { notify(failure instanceof Error ? failure.message : "The preview failed. Retry.", "error"); }
    finally { setBusy(false); }
  }
  async function reclaim(plan: ReclaimPreview) {
    setBusy(true);
    try {
      const result = await client.request<{ freed_bytes?: number; applied?: boolean }>("juiced/storage/reclaim", { items: plan.items.map((item) => item.id), expected_revision: plan.revision });
      notify(result.applied === false ? `Freed ${bytes(result.freed_bytes ?? 0)}. Some items were kept; review them below.` : `Freed ${bytes(result.freed_bytes ?? 0)}.`, result.applied === false ? "error" : undefined);
      setChosen([]); setPreview(null);
    } catch (failure) { notify(failure instanceof Error ? failure.message : "Nothing was deleted. Review again.", "error"); setPreview(null); }
    finally { setBusy(false); await view.reload(); }
  }
  return <div className="page-body storage-page">
    <section className="volume-strip" aria-label="Volumes">{data.volumes.map((volume) => <VolumeItem key={volume.path} volume={volume} />)}</section>
    {view.error && <p className="inline-warning">Showing the last measurement. {view.error}</p>}
    {data.warnings.map((warning) => <p key={`${warning.code}:${warning.path ?? ""}`} className="inline-warning" role="status">{warning.message}</p>)}
    <div className="storage-columns">
      <Usage data={data} />
      <section className="panel" aria-label="Reclaim space">
        <header className="panel-head"><h2>Reclaim space <HelpTip label="Reclaim space">Only files no model, download, running engine or saved session uses. Read-only library folders are never touched. Juiced shows what will be deleted before anything is.</HelpTip></h2>
          {rows.length > 0 && <Button variant="danger" disabled={!online || busy || selected.length === 0} onClick={() => void review()}>{selected.length ? `Review ${bytes(selectedBytes)}` : "Review"}</Button>}</header>
        {rows.length === 0 ? <p className="all-clear"><span className="status-dot good" /> Nothing to reclaim.</p> : <ul className="row-list reclaim-list" aria-label="Reclaimable items">
          {rows.map((row) => <ReclaimRow key={row.id} row={row} checked={selected.includes(row.id)} disabled={!online || busy} onToggle={() => toggle(row.id)} />)}
        </ul>}
      </section>
    </div>
    <ModelUsageTable data={data} />
    {preview && <ReclaimDialog preview={preview} busy={busy} onCancel={() => setPreview(null)} onConfirm={() => void reclaim(preview)} />}
  </div>;
}

function VolumeItem({ volume }: { volume: Volume }) {
  const used = volume.total_bytes ? volume.used_bytes / volume.total_bytes * 100 : null;
  const roles = volume.roles.map((role) => roleNames[role] ?? role).join(", ");
  return <div className="volume-item" role="group" aria-label={`Volume ${volume.path}`}>
    <div className="volume-head"><strong className="break-word" title={volume.paths.join("\n")}>{volume.path}</strong>{volume.read_only ? <Badge>Read-only</Badge> : volume.low ? <Badge tone="warning">Low space</Badge> : null}</div>
    <Meter value={used} label={`${volume.path} used`} />
    <span className="muted">{bytes(volume.available_bytes)} free of {bytes(volume.total_bytes)} · {roles}</span>
  </div>;
}

function Usage({ data }: { data: StorageView }) {
  const cache = data.session_cache;
  const detail: Record<string, string | null> = {
    session_cache: cache.budget_bytes != null ? `${cache.sessions ?? 0} saved ${cache.sessions === 1 ? "session" : "sessions"} · ${bytes(cache.session_bytes ?? 0)} of ${bytes(cache.budget_bytes)} budget` : null,
    logs: "Kept in memory",
  };
  const shown = data.categories.filter((category) => category.bytes > 0 || category.id === "models" || category.id === "session_cache");
  return <section className="panel" aria-label="Usage">
    <header className="panel-head"><h2>Usage <HelpTip label="Usage">Space Juiced uses on each store. Free space must stay above {bytes(data.settings.reserve_bytes)} after a download, engine install or snapshot, or Juiced refuses it before starting.</HelpTip></h2>{data.truncated && <span className="muted">Lower bound</span>}</header>
    <ul className="row-list usage-list" aria-label="Usage by store">{shown.map((category) => <li key={category.id} aria-label={category.title}>
      <span className="row-main"><strong>{category.title}</strong>{detail[category.id] && <span className="muted">{detail[category.id]}</span>}</span>
      <span className="row-meta numeric"><span>{bytes(category.bytes)}</span>{category.reclaimable_bytes > 0 && <span className="muted">{bytes(category.reclaimable_bytes)} reclaimable</span>}</span>
      {category.id === "session_cache" && cache.budget_bytes ? <Meter value={(cache.session_bytes ?? 0) / cache.budget_bytes * 100} label="Session cache budget used" /> : null}
    </li>)}</ul>
  </section>;
}

function ReclaimRow({ row, checked, disabled, onToggle }: { row: Reclaimable; checked: boolean; disabled: boolean; onToggle: () => void }) {
  const id = `reclaim-${row.id}`;
  return <li className={`reclaim-row ${row.reclaimable ? "" : "kept"}`} aria-label={row.title}>
    {row.reclaimable ? <input id={id} type="checkbox" checked={checked} disabled={disabled} onChange={onToggle} aria-label={`Select ${row.title}`} /> : <span className="reclaim-spacer" aria-hidden="true" />}
    <label htmlFor={row.reclaimable ? id : undefined} className="row-main" title={row.path ?? undefined}><strong className="break-word">{row.title}</strong><span className="muted break-word">{[kindNames[row.kind] ?? row.kind, row.reclaimable ? place(row.path) : row.message].filter(Boolean).join(" · ")}</span></label>
    <span className="row-meta numeric">{bytes(row.bytes)}</span>
  </li>;
}

function ModelUsageTable({ data }: { data: StorageView }) {
  if (data.models.length === 0) return null;
  return <section className="panel" aria-label="Models on disk">
    <header className="panel-head"><h2>Models on disk <HelpTip label="Models on disk">Space each model's files take. Shared files stay until every model that uses them is removed. Remove a model from its page under Models; files in read-only library folders are never deleted.</HelpTip></h2></header>
    <div className="data-table stacked model-usage" role="table" aria-label="Models on disk">
      <div className="data-head" role="row"><span role="columnheader">Model</span><span role="columnheader" className="numeric">Size</span><span role="columnheader" className="numeric">Shared</span><span role="columnheader" className="numeric">Removal frees</span></div>
      {data.models.map((model) => <div className="data-row" role="row" key={model.model} aria-label={model.model}>
        <span role="rowheader" className="break-word" data-label="Model">{model.model}{model.shared_with.length > 0 && <span className="muted"> · shares files with {model.shared_with.join(", ")}</span>}</span>
        <span role="cell" className="numeric" data-label="Size">{bytes(model.bytes)}</span>
        <span role="cell" className="numeric" data-label="Shared">{model.shared_bytes ? bytes(model.shared_bytes) : "—"}</span>
        <span role="cell" className="numeric" data-label="Removal frees">{model.read_only_bytes === model.bytes && model.bytes > 0 ? "Library" : bytes(model.removal_frees_bytes)}</span>
      </div>)}
    </div>
  </section>;
}

function ReclaimDialog({ preview, busy, onCancel, onConfirm }: { preview: ReclaimPreview; busy: boolean; onCancel: () => void; onConfirm: () => void }) {
  const allowed = preview.items.filter((item) => item.reclaimable);
  const refused = preview.items.filter((item) => !item.reclaimable);
  return <Dialog title="Delete these files?" onClose={onCancel}>
    <p>Frees <strong>{bytes(preview.freed_bytes)}</strong>. This can't be undone.</p>
    <ul className="row-list reclaim-preview" aria-label="Items to delete">{allowed.map((item) => <li key={item.id}><span className="row-main"><strong className="break-word">{item.title}</strong><span className="muted break-word">{[kindNames[item.kind] ?? item.kind, item.references.length ? `affects ${item.references.join(", ")}` : null].filter(Boolean).join(" · ")}</span></span><span className="row-meta numeric">{bytes(item.bytes)}</span></li>)}</ul>
    {refused.length > 0 && <p className="inline-warning">{refused.map((item) => `${item.title}: ${item.message ?? "kept"}`).join(" ")} Deselect {refused.length === 1 ? "it" : "them"} to continue.</p>}
    <div className="dialog-actions"><Button onClick={onCancel}>Cancel</Button><Button variant="danger" disabled={busy || refused.length > 0 || allowed.length === 0} onClick={onConfirm}>{`Delete ${allowed.length} ${allowed.length === 1 ? "item" : "items"}`}</Button></div>
  </Dialog>;
}

