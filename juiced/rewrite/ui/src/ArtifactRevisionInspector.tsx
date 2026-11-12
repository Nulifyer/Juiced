import { useState } from "react";
import type { Client } from "./api";
import type { Model } from "./domain";
import type { Mutate } from "./Models";
import { parseArtifactRevision } from "./artifactSetData";
import { firstInventoryPages, inventoryPath, nextInventoryPage, parseArtifactInventory, previousInventoryPage } from "./management";
import { Button } from "./ui";
import { useResource } from "./useResource";
import { useRevisionSelection } from "./useRevisionSelection";

export function ArtifactRevisionInspector({ model, client, busy, online, otherDirty, onDirty, mutate }: { model: Model; client: Client; busy: boolean; online: boolean; otherDirty: boolean; onDirty: (dirty: boolean) => void; mutate: Mutate }) {
  const path = `models/${encodeURIComponent(model.id)}/artifacts`;
  const revision = useResource(client, path, parseArtifactRevision, 0);
  const draft = useRevisionSelection(client, model, onDirty, revision.reload);
  const { selected, draftRevision } = draft;
  const [cursors, setCursors] = useState(firstInventoryPages);
  const inventory = useResource(client, inventoryPath(cursors, { artifact_set: selected || undefined }, "artifact_sets", 128), parseArtifactInventory, 0, true);
  const view = revision.data?.model === model.id ? revision.data : null;
  const sets = inventory.data?.artifact_sets ?? [];
  const selection = inventory.data?.selection.artifact_set;
  const target = sets.find((set) => set.id === selected) ?? (selection?.id === selected ? selection : undefined);
  const removed = !!selected && !target && inventory.current;
  const setPage = inventory.data?.pages.artifact_sets;
  const setDepth = cursors.stacks.artifact_sets.length;
  const staleDraft = !!selected && draftRevision !== view?.revision;
  const blocked = !online || busy || otherDirty || !!revision.error || !view || !view.supported || view.worker_in_use || model.loaded || model.state === "loading" || model.active_requests > 0 || view.revision !== model.revision || staleDraft;
  async function change(rollback: boolean) {
    if (blocked || (!rollback && (!target || target.specification.members.length !== 1 || !target.members_present || inventory.error))) return;
    if (await mutate(path, { expected_revision: rollback ? view!.revision : draftRevision, change: rollback ? { action: "rollback" } : { action: "promote", artifact_set: selected } }, `Changed ${model.id} artifact revision. Policy preserved; worker remains unloaded.`)) {
      draft.clear(); await revision.reload(); await inventory.reload();
    }
  }
  async function refresh() {
    if (setDepth > 0 || inventory.error) setCursors(firstInventoryPages());
    const [fresh] = await Promise.all([revision.reload(), inventory.reload()]);
    if (selected && fresh?.model === model.id) draft.rebase(fresh.revision);
  }
  return <section className="inspector-section" aria-label="Model artifact revision">
    <div className="section-heading"><h3>Model revision</h3><Button disabled={busy || revision.loading || inventory.loading} onClick={() => void refresh()}>Refresh revisions</Button></div>
    <p className="field-help">Switch complete recorded weight sets while the worker is unloaded. The previous set stays protected for rollback. Engine settings and loading policy are preserved.</p>
    {revision.error && <p role="alert" className="inline-error">{revision.error}</p>}
    {inventory.error && <p role="alert" className="inline-error">{inventory.error}</p>}
    {view?.current && <p className="field-help break-word">Current set: {view.current.artifact_set}</p>}
    {view?.previous && <p className="field-help break-word">Previous set: {view.previous.artifact_set}</p>}
    {view && !view.supported && <p className="inline-warning">This profile needs its native settings or protected resource authority before weight revisions can change here.</p>}
    {(view?.worker_in_use || model.loaded) && <p className="field-help">{selected ? "Cancel this revision draft, then unload the worker before changing weights." : "Unload the worker and finish active requests before changing weights."}</p>}
    <label htmlFor="revision-set">Replacement artifact set</label><select id="revision-set" value={selected} disabled={busy || otherDirty || !view?.supported || view.worker_in_use || model.loaded || model.state === "loading" || model.active_requests > 0 || revision.loading || inventory.loading} onChange={(event) => { draft.select(event.target.value, view!.revision); }}>
      <option value="">Choose a recorded primary set</option>
      {!!selected && !target && <option value={selected}>{removed ? "Selected set is no longer recorded" : "Selected set"}</option>}
      {!!target && !sets.includes(target) && <option value={target.id}>{target.specification.origin.source_revision} · {target.id.slice(4, 16)}</option>}
      {sets.filter((set) => set.specification.members.length === 1 && set.members_present && set.id !== view?.current?.artifact_set).map((set) => <option value={set.id} key={set.id}>{set.specification.origin.source_revision} · {set.id.slice(4, 16)}</option>)}
    </select>
    {setPage && (setDepth > 0 || setPage.next_after) && <div className="action-row wrap cache-pagination" aria-label="Replacement set pages"><Button disabled={inventory.loading || setDepth === 0} onClick={() => setCursors(previousInventoryPage(cursors, "artifact_sets"))}>Previous sets</Button><span className="muted">Page {setDepth + 1} · {setPage.total} sets</span><Button disabled={inventory.loading || !setPage.next_after} onClick={() => inventory.data && setCursors(nextInventoryPage(cursors, inventory.data, "artifact_sets"))}>More sets</Button></div>}
    {selected && <p className="field-help break-word">Selected set: {selected}</p>}
    {removed && <p className="inline-warning">The selected set was removed. Choose a current set or cancel this draft.</p>}
    {(staleDraft || (view && view.revision !== model.revision)) && <p className="inline-warning">The model changed. Your selection is kept. Review the model changes, then refresh revisions or cancel this draft.</p>}
    {otherDirty && <p className="field-help">Save or cancel other model drafts before changing weights.</p>}
    <p className="field-help">Recorded content is not engine qualification. Changing sets does not load a worker. Earlier snapshots require their exact execution identity.</p>
    <div className="action-row wrap"><Button disabled={busy || !selected} onClick={draft.clear}>Cancel revision draft</Button><Button disabled={blocked || !target || !!inventory.error} onClick={() => void change(false)}>Use selected set</Button><Button disabled={blocked || !!selected || !view?.previous} onClick={() => void change(true)}>Roll back model revision</Button></div>
  </section>;
}
