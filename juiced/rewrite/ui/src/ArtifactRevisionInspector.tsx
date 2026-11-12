import { useState } from "react";
import type { Client } from "./api";
import type { Model } from "./domain";
import type { Mutate } from "./Models";
import { parseArtifactRevision } from "./artifactSetData";
import { firstInventoryPages, inventoryPath, nextInventoryPage, parseArtifactInventory, previousInventoryPage } from "./management";
import { Button, HelpTip } from "./ui";
import { useResource } from "./useResource";
import { useRevisionSelection } from "./useRevisionSelection";

export function ArtifactRevisionInspector({ model, client, busy, online, otherDirty, onDirty, mutate }: { model: Model; client: Client; busy: boolean; online: boolean; otherDirty: boolean; onDirty: (dirty: boolean) => void; mutate: Mutate }) {
  const path = `models/${encodeURIComponent(model.id)}/artifacts`;
  const revision = useResource(client, path, parseArtifactRevision, 5000);
  const draft = useRevisionSelection(client, model, onDirty, revision.reload);
  const { selected, draftRevision } = draft;
  const [cursors, setCursors] = useState(firstInventoryPages);
  const inventory = useResource(client, inventoryPath(cursors, { artifact_set: selected || undefined }, "artifact_sets", 128), parseArtifactInventory, 5000, true);
  const view = revision.data?.model === model.id ? revision.data : null;
  const sets = inventory.data?.artifact_sets ?? [];
  const selection = inventory.data?.selection.artifact_set;
  const target = sets.find((set) => set.id === selected) ?? (selection?.id === selected ? selection : undefined);
  const removed = !!selected && !target && inventory.current;
  const setPage = inventory.data?.pages.artifact_sets;
  const setDepth = cursors.stacks.artifact_sets.length;
  const choices = sets.filter((set) => set.specification.members.length === 1 && set.members_present && set.id !== view?.current?.artifact_set);
  const staleDraft = !!selected && draftRevision !== view?.revision;
  const blocked = !online || busy || otherDirty || !!revision.error || !view || !view.supported || view.worker_in_use || model.loaded || model.state === "loading" || model.active_requests > 0 || view.revision !== model.revision || staleDraft;
  async function change(rollback: boolean) {
    if (blocked || (!rollback && (!target || target.specification.members.length !== 1 || !target.members_present || inventory.error))) return;
    if (await mutate(path, { expected_revision: rollback ? view!.revision : draftRevision, change: rollback ? { action: "rollback" } : { action: "promote", artifact_set: selected } }, `Changed the weights for ${model.id}.`)) {
      draft.clear(); await revision.reload(); await inventory.reload();
    }
  }
  async function refresh() {
    if (setDepth > 0 || inventory.error) setCursors(firstInventoryPages());
    const [fresh] = await Promise.all([revision.reload(), inventory.reload()]);
    if (selected && fresh?.model === model.id) draft.rebase(fresh.revision);
  }
  return <section className="inspector-section" aria-label="Model artifact revision">
    <div className="section-heading"><div className="page-title"><h3>Weights</h3><HelpTip label="Weights">Switch to another weight set while the model is unloaded. The previous set is kept for rollback. Engine settings and policy don't change. Saved snapshots only restore with the weights that made them.</HelpTip></div></div>
    {revision.error && <p role="alert" className="inline-error">{revision.error}</p>}
    {inventory.error && <p role="alert" className="inline-error">{inventory.error}</p>}
    {(view?.current || view?.previous) && <dl className="facts">{view?.current && <div><dt>Current</dt><dd className="break-word" title={view.current.artifact_set}>{view.current.artifact_set.slice(4, 16)}</dd></div>}{view?.previous && <div><dt>Previous</dt><dd className="break-word" title={view.previous.artifact_set}>{view.previous.artifact_set.slice(4, 16)}</dd></div>}</dl>}
    {!view ? null
      : !view.supported ? <p className="field-help">This model's weights are set by its registration.</p>
      : view.worker_in_use || model.loaded || model.state === "loading" || model.active_requests > 0 ? <p className="field-help">Unload the model to switch weights.</p>
      : <>
        {(choices.length > 0 || selected || (setPage && (setDepth > 0 || setPage.next_after))) && <><label htmlFor="revision-set">Switch to</label><select id="revision-set" value={selected} disabled={busy || otherDirty || revision.loading || inventory.loading} onChange={(event) => { draft.select(event.target.value, view.revision); }}>
          <option value="">Choose a weight set</option>
          {!!selected && !target && <option value={selected}>{removed ? "Selected set is no longer recorded" : "Selected set"}</option>}
          {!!target && !choices.includes(target) && <option value={target.id}>{target.specification.origin.source_revision} · {target.id.slice(4, 16)}</option>}
          {choices.map((set) => <option value={set.id} key={set.id}>{set.specification.origin.source_revision} · {set.id.slice(4, 16)}</option>)}
        </select></>}
        {setPage && (setDepth > 0 || setPage.next_after) && <div className="action-row wrap cache-pagination" aria-label="Replacement set pages"><Button disabled={inventory.loading || setDepth === 0} onClick={() => setCursors(previousInventoryPage(cursors, "artifact_sets"))}>Previous sets</Button><span className="muted">Page {setDepth + 1} · {setPage.total} sets</span><Button disabled={inventory.loading || !setPage.next_after} onClick={() => inventory.data && setCursors(nextInventoryPage(cursors, inventory.data, "artifact_sets"))}>More sets</Button></div>}
        {inventory.data && choices.length === 0 && !selected && !view.previous && <p className="field-help">No other weight set is recorded for this model.</p>}
        {removed && <p className="inline-warning">The selected set was removed. Pick another or cancel.</p>}
        {(staleDraft || view.revision !== model.revision) && <div className="action-row wrap"><p className="inline-warning">The model changed. Your selection is kept.</p><Button onClick={() => void refresh()}>Use latest</Button></div>}
        {otherDirty && <p className="field-help">Save or cancel your other changes first.</p>}
        <div className="action-row wrap">{selected
          ? <><Button disabled={busy} onClick={draft.clear}>Cancel</Button><Button disabled={blocked || !target || !!inventory.error} onClick={() => void change(false)}>Switch weights</Button></>
          : view.previous && <Button disabled={blocked} onClick={() => void change(true)}>Roll back to previous</Button>}</div>
      </>}
  </section>;
}
