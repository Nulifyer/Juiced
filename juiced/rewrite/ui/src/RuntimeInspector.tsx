import type { Client } from "./api";
import { bytes, type Model } from "./domain";
import type { Mutate } from "./Models";
import { packageLabel, parseRuntimeInventory, parseRuntimeSelection, runtimeCandidate } from "./runtimePackages";
import { useResource } from "./useResource";
import { useRevisionSelection } from "./useRevisionSelection";
import { Button, HelpTip, Icon } from "./ui";

export function RuntimeInspector({ model, client, busy, online, otherDirty, onDirty, mutate }: { model: Model; client: Client; busy: boolean; online: boolean; otherDirty: boolean; onDirty: (dirty: boolean) => void; mutate: Mutate }) {
  const path = `models/${encodeURIComponent(model.id)}/runtime`;
  const revision = useResource(client, path, parseRuntimeSelection, 5000);
  const inventory = useResource(client, "juiced/runtime-packages", parseRuntimeInventory, 5000);
  const draft = useRevisionSelection(client, model, onDirty, revision.reload);
  const view = revision.data?.model === model.id ? revision.data : null;
  const packages = inventory.data?.packages ?? [];
  const target = packages.find((item) => item.id === draft.selected);
  const label = (id: string) => { const item = packages.find((entry) => entry.id === id); return item ? packageLabel(item) : id; };
  const candidates = packages.filter((item) => runtimeCandidate(item, view));
  const eligibleTarget = !!target && runtimeCandidate(target, view);
  const stale = !!draft.selected && draft.draftRevision !== view?.revision;
  const blocked = !online || busy || otherDirty || !!revision.error || !!inventory.error || !inventory.data || revision.loading || inventory.loading || !view?.supported || view.worker_in_use || model.loaded || model.state === "loading" || model.active_requests > 0 || view.revision !== model.revision || stale;
  async function refresh() {
    const [fresh] = await Promise.all([revision.reload(), inventory.reload()]);
    if (draft.selected && fresh?.model === model.id) draft.rebase(fresh.revision);
  }
  async function change(rollback: boolean) {
    if (blocked || (!rollback && (!eligibleTarget || inventory.error))) return;
    if (await mutate(path, { expected_revision: rollback ? view!.revision : draft.draftRevision, change: rollback ? { action: "rollback" } : { action: "promote", package: draft.selected } }, `Changed the engine for ${model.id}.`)) {
      draft.clear(); await revision.reload(); await inventory.reload();
    }
  }
  return <section className="inspector-section" aria-label="Model runtime selection">
    <div className="section-heading"><div className="page-title"><h3>Engine</h3><HelpTip label="Engine">Switch to another installed engine package while the model is unloaded. The previous one stays available for rollback. Weights, settings and policy don't change. Saved snapshots only restore with the engine that made them.</HelpTip></div></div>
    {revision.error && <p role="alert" className="inline-error">{revision.error}</p>}
    {inventory.error && <p role="alert" className="inline-error">{inventory.error}</p>}
    <dl className="facts"><div><dt>Current</dt><dd className="break-word" title={view?.current ?? undefined}>{view ? view.current ? label(view.current) : "Set in config" : "Loading…"}</dd></div>{view?.previous && <div><dt>Previous</dt><dd className="break-word" title={view.previous}>{label(view.previous)}</dd></div>}</dl>
    {view && !view.supported && <p className="field-help">This model's engine can't be switched here.</p>}
    {(view?.worker_in_use || model.loaded) && <p className="field-help">Unload the model to switch engines.</p>}
    <label htmlFor="runtime-package">Switch engine to</label><select id="runtime-package" value={draft.selected} disabled={busy || !online || otherDirty || !view?.supported || view.worker_in_use || model.loaded || model.state === "loading" || model.active_requests > 0 || revision.loading || inventory.loading} onChange={(event) => draft.select(event.target.value, view?.revision ?? null)}>
      <option value="">Choose an installed engine</option>
      {!!draft.selected && !eligibleTarget && <option value={draft.selected}>Selected runtime needs review</option>}
      {candidates.map((item) => <option value={item.id} key={item.id}>{packageLabel(item)}</option>)}
    </select>
    {target && <dl className="facts"><div><dt>Build</dt><dd>{target.provenance.build_revision}</dd></div><div><dt>Target</dt><dd>{target.declared_target.operating_system} / {target.declared_target.architecture}</dd></div><div><dt>Size</dt><dd>{bytes(target.reserved_bytes)}</dd></div></dl>}
    {(stale || (view && view.revision !== model.revision)) && <div className="action-row wrap"><p className="inline-warning">The model changed. Your selection is kept.</p><Button onClick={() => void refresh()}>Use latest</Button></div>}
    {draft.selected && !eligibleTarget && <p className="inline-warning">That engine can't be used for this model. Pick another or cancel.</p>}
    {inventory.data && candidates.length === 0 && view?.supported && <p className="field-help">No other installed engine fits this model.</p>}
    {otherDirty && <p className="field-help">Save or cancel your other changes first.</p>}
    <div className="action-row wrap"><Button disabled={busy || !draft.selected} onClick={draft.clear}>Cancel runtime draft</Button><Button disabled={blocked || !eligibleTarget || !!inventory.error} onClick={() => void change(false)}>Use selected runtime</Button><Button disabled={blocked || !!draft.selected || !view?.previous} onClick={() => void change(true)}>Roll back runtime</Button></div>
  </section>;
}
