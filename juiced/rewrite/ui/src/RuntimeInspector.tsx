import type { Client } from "./api";
import { bytes, type Model } from "./domain";
import type { Mutate } from "./Models";
import { parseRuntimeInventory, parseRuntimeSelection, runtimeCandidate } from "./runtimePackages";
import { useResource } from "./useResource";
import { useRevisionSelection } from "./useRevisionSelection";
import { Button } from "./ui";

export function RuntimeInspector({ model, client, busy, online, otherDirty, onDirty, mutate }: { model: Model; client: Client; busy: boolean; online: boolean; otherDirty: boolean; onDirty: (dirty: boolean) => void; mutate: Mutate }) {
  const path = `models/${encodeURIComponent(model.id)}/runtime`;
  const revision = useResource(client, path, parseRuntimeSelection, 0);
  const inventory = useResource(client, "juiced/runtime-packages", parseRuntimeInventory, 0);
  const draft = useRevisionSelection(client, model, onDirty, revision.reload);
  const view = revision.data?.model === model.id ? revision.data : null;
  const packages = inventory.data?.packages ?? [];
  const target = packages.find((item) => item.id === draft.selected);
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
    if (await mutate(path, { expected_revision: rollback ? view!.revision : draft.draftRevision, change: rollback ? { action: "rollback" } : { action: "promote", package: draft.selected } }, `Changed ${model.id} runtime. Weights and policy preserved; worker remains unloaded.`)) {
      draft.clear(); await revision.reload(); await inventory.reload();
    }
  }
  return <section className="inspector-section" aria-label="Model runtime selection">
    <div className="section-heading"><h3>Installed runtime</h3><Button disabled={busy || revision.loading || inventory.loading} onClick={() => void refresh()}>Refresh runtimes</Button></div>
    <p className="field-help">Select an installed engine while the worker is unloaded. One previous managed runtime stays available for rollback. Weights, worker settings and loading policy are preserved.</p>
    {revision.error && <p role="alert" className="inline-error">{revision.error}</p>}
    {inventory.error && <p role="alert" className="inline-error">{inventory.error}</p>}
    <p className="field-help break-word">Current runtime: {view ? view.current ?? "Configured outside managed runtime storage" : "Reading selection…"}</p>
    {view?.previous && <p className="field-help break-word">Previous runtime: {view.previous}</p>}
    {view && !view.supported && <p className="inline-warning">This profile needs its external, native settings or protected resource authority before its runtime can change here.</p>}
    {(view?.worker_in_use || model.loaded) && <p className="field-help">{draft.selected ? "Cancel this runtime draft, then unload the worker before changing its engine." : "Unload the worker and finish active requests before changing its engine."}</p>}
    <label htmlFor="runtime-package">Replacement installed runtime</label><select id="runtime-package" value={draft.selected} disabled={busy || !online || otherDirty || !view?.supported || view.worker_in_use || model.loaded || model.state === "loading" || model.active_requests > 0 || revision.loading || inventory.loading} onChange={(event) => draft.select(event.target.value, view?.revision ?? null)}>
      <option value="">Choose an installed runtime</option>
      {!!draft.selected && !eligibleTarget && <option value={draft.selected}>Selected runtime needs review</option>}
      {candidates.map((item) => <option value={item.id} key={item.id}>{item.provenance.build_revision} · {item.id.slice(8, 20)}</option>)}
    </select>
    {target && <><p className="field-help break-word">Selected runtime: {target.id}</p><dl className="facts"><div><dt>Declared build</dt><dd>{target.provenance.build_revision}</dd></div><div><dt>Declared target</dt><dd>{target.declared_target.operating_system} / {target.declared_target.architecture} / {target.declared_target.abi}</dd></div><div><dt>Installed payload</dt><dd>{bytes(target.reserved_bytes)} / {target.member_count} {target.member_count === 1 ? "member" : "members"}</dd></div></dl></>}
    {(stale || (view && view.revision !== model.revision)) && <p className="inline-warning">The model changed. Your runtime selection is kept. Review the model changes, then refresh runtimes or cancel this draft.</p>}
    {draft.selected && !eligibleTarget && <p className="inline-warning">The selected runtime is unavailable, already selected or does not match this adapter and service target. Choose a different installed runtime or cancel this draft.</p>}
    {inventory.data && candidates.length === 0 && view?.supported && <p className="field-help">No replacement runtime matches this adapter and service target. Import a matching local bundle in Backends &amp; hardware, then refresh runtimes here.</p>}
    {otherDirty && <p className="field-help">Save or cancel other model drafts before changing its runtime.</p>}
    <p className="field-help">Target filtering compares declared OS and CPU labels with this service. Recorded builds and matching target labels remain unqualified. Selection does not load a worker. Saved computation snapshots require the exact selected package. The first managed selection has no managed rollback history.</p>
    <div className="action-row wrap"><Button disabled={busy || !draft.selected} onClick={draft.clear}>Cancel runtime draft</Button><Button disabled={blocked || !eligibleTarget || !!inventory.error} onClick={() => void change(false)}>Use selected runtime</Button><Button disabled={blocked || !!draft.selected || !view?.previous} onClick={() => void change(true)}>Roll back runtime</Button></div>
  </section>;
}
