import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { bytes, loadReason, matchingModels, offers, policyNames, services, unloadReason, workerState, workerTone, type Model, type Policy, type Service } from "./domain";
import { Badge, Button, Empty, HelpTip, Icon, Switch } from "./ui";
import { Client } from "./api";
import { removalReason } from "./management";
import { ContextInspector } from "./ContextInspector";
import { QuantSync } from "./QuantSync";
import { ArtifactRevisionInspector } from "./ArtifactRevisionInspector";
import { RuntimeInspector } from "./RuntimeInspector";
import { ModelIdle, residencyNote } from "./Residency";
import { ModelFiles, modelFiles, removalNote, unusedQuants } from "./LocalModels";
import { notify } from "./toast";
import { useResource } from "./useResource";
import type { LocalGroup, LocalInventory } from "./localLibraryData";

export type Mutate = (path: string, body: unknown, success: string) => Promise<boolean>;

function workerFailure(code: string): string {
  if (code === "storage_root_unavailable") return "A claimed storage folder is unavailable. Restore its original location before retrying. On demand workers can still be unloaded.";
  if (code === "state_termination_unconfirmed") return "The last snapshot operation did not confirm engine completion.";
  if (code === "request_termination_unconfirmed") return "The last request did not confirm engine completion.";
  if (code === "speech_transfer_uncertain") return "The speech transfer did not confirm completion. Its worker was stopped; retrying starts a fresh worker.";
  if (code === "startup_queue_full") return "The startup queue is full. Wait for another model to finish starting, then retry.";
  if (code === "startup_queue_timeout") return "This worker waited too long to start. Retry when startup capacity is available.";
  if (code === "startup_completion_timeout") return "The startup attempt has not settled. Refresh its state before retrying.";
  if (code === "startup_paused" || code === "admission_paused") return "New work is paused. Resume requests before retrying.";
  if (code === "model_changed") return "The saved policy changed before startup. Review it before retrying.";
  if (code === "shutting_down") return "The service is shutting down.";
  if (code === "worker_cleanup_pending") return "The previous worker has not confirmed exit. Its reservation is retained. Retry cleanup with Unload before loading another generation.";
  if (code === "resident_budget") return "Memory is full and every loaded model is Hot or busy. Unload one, then retry.";
  if (code === "missing_weights") return "The configured model file is unavailable. Check its path on the service host.";
  if (code === "worker_exited") return "The engine exited. Review its launch configuration, then retry loading.";
  return "The worker needs attention. Review its current state before retrying.";
}

type ListProps = { models: Model[]; selected?: string; onSelect: (id: string) => void; mode: "models" | "policy"; mutate: Mutate; busy: boolean; online: boolean; idleControl?: ReactNode; files?: LocalGroup[]; inventory?: LocalInventory | null; selectedFile?: string; onSelectFile?: (id: string) => void; onDiscover?: () => void; client?: Client };
type SortKey = "name" | "state" | "memory";
const placeholderClient = new Client("", "");

export function parseUpdates(value: unknown): string[] {
  const data = value && typeof value === "object" ? (value as { data?: unknown }).data : undefined;
  return (Array.isArray(data) ? data : []).filter((item) => item && typeof item === "object" && item.update_available === true && typeof item.id === "string").map((item) => item.id as string);
}

export function sortModels(models: Model[], sort: { key: SortKey; descending: boolean } | null): Model[] {
  if (!sort) return models;
  const rank = (model: Model) => sort.key === "memory" ? model.resident_bytes ?? -1 : sort.key === "state" ? (model.loaded ? 0 : model.state === "loading" ? 1 : model.policy === "disabled" ? 3 : 2) : 0;
  const sorted = [...models].sort((left, right) => (sort.key === "name" ? left.id.localeCompare(right.id) : rank(left) - rank(right)) || left.id.localeCompare(right.id));
  return sort.descending ? sorted.reverse() : sorted;
}

function BulkBar({ models, chosen, mutate, disabled, onClear }: { models: Model[]; chosen: Set<string>; mutate: Mutate; disabled: boolean; onClear: () => void }) {
  const picked = models.filter((model) => chosen.has(model.id));
  const loadable = picked.filter((model) => !model.loaded && !loadReason(model));
  const unloadable = picked.filter((model) => model.loaded && !unloadReason(model));
  const run = async (targets: Model[], action: (model: Model) => Promise<boolean>) => { for (const model of targets) await action(model); onClear(); };
  return <div className="bulk-bar" role="toolbar" aria-label="Selected models">
    <strong>{picked.length} selected</strong>
    <Button disabled={disabled || loadable.length === 0} onClick={() => void run(loadable, (model) => mutate("load", { model: model.id }, `Loaded ${model.id}.`))}>Load{loadable.length && loadable.length !== picked.length ? ` ${loadable.length}` : ""}</Button>
    <Button disabled={disabled || unloadable.length === 0} onClick={() => void run(unloadable, (model) => mutate("unload", { model: model.id }, `Unloaded ${model.id}.`))}>Unload{unloadable.length && unloadable.length !== picked.length ? ` ${unloadable.length}` : ""}</Button>
    <select aria-label="Set loading policy for selected models" value="" disabled={disabled} onChange={(event) => { const policy = event.target.value as Policy; if (policy) void run(picked.filter((model) => model.policy !== policy && !(policy === "disabled" && model.active_requests > 0)), (model) => mutate("juiced/policy", { model: model.id, policy, expected_revision: model.revision }, `${model.id} is now ${policyNames[policy]}.`)); }}>
      <option value="">Set policy…</option><option value="hot">Hot</option><option value="on-demand">On demand</option><option value="disabled">Disabled</option>
    </select>
    <span className="spacer" />
    <Button variant="ghost" onClick={onClear}>Clear</Button>
  </div>;
}

function SortHeader({ label, id, sort, onSort, numeric, pick }: { label: string; id: SortKey; sort: { key: SortKey; descending: boolean } | null; onSort: (key: SortKey) => void; numeric?: boolean; pick?: ReactNode }) {
  const active = sort?.key === id;
  return <span role="columnheader" className={numeric ? "numeric" : pick ? "picked" : undefined} aria-sort={active ? (sort!.descending ? "descending" : "ascending") : "none"}>{pick}<button type="button" className="sort-header" onClick={() => onSort(id)}>{label}{active && <span aria-hidden="true">{sort!.descending ? " ↓" : " ↑"}</span>}</button></span>;
}

function setPolicy(mutate: Mutate, model: Model, policy: Policy) {
  if (policy === model.policy) return;
  void mutate("juiced/policy", { model: model.id, policy, expected_revision: model.revision }, policy === "disabled" && model.loaded ? `Disabled and unloaded ${model.id}.` : `${model.id} is now ${policyNames[policy]}.`);
}

function PolicySelect({ model, mutate, disabled }: { model: Model; mutate: Mutate; disabled: boolean }) {
  return <select className="policy-select" aria-label={`Loading policy for ${model.id}`} data-policy={model.policy} value={model.policy} disabled={disabled} onClick={(event) => event.stopPropagation()} onChange={(event) => setPolicy(mutate, model, event.target.value as Policy)}>
    <option value="hot">Hot</option><option value="on-demand">On demand</option><option value="disabled" disabled={model.active_requests > 0}>Disabled</option>
  </select>;
}

function RowAction({ model, mutate, disabled }: { model: Model; mutate: Mutate; disabled: boolean }) {
  const unload = model.loaded || model.state === "cleanup-pending";
  const reason = unload ? unloadReason(model) : loadReason(model);
  return <Button className="row-action" disabled={disabled || !!reason} title={reason ?? undefined} onClick={(event) => { event.stopPropagation(); void mutate(unload ? "unload" : "load", { model: model.id }, unload ? `Unloaded ${model.id}.` : `Loaded ${model.id}.`); }}>{unload ? "Unload" : "Load"}</Button>;
}

function fileMatches(group: LocalGroup, query: string) {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const text = `${group.name} ${group.publisher ?? ""} ${group.quants.map((quant) => quant.label).join(" ")}`.toLowerCase();
  return terms.every((term) => text.includes(term));
}

function FileRow({ group, selected, onSelect }: { group: LocalGroup; selected: boolean; onSelect: () => void }) {
  const unused = unusedQuants(group);
  const size = unused.length ? unused.reduce((sum, quant) => sum + quant.bytes, 0) : group.bytes;
  return <div className={`data-row model-row file-row ${selected ? "selected" : ""}`} role="row" onClick={onSelect}>
    <div role="cell" className="model-identity picked"><span className="pick pick-space" aria-hidden="true" /><div className="identity-text"><button className="model-name" aria-pressed={selected} onClick={(event) => { event.stopPropagation(); onSelect(); }}>{group.name}</button><span className="model-meta">{[group.publisher, unused.map((quant) => quant.label).join(", ") || null, bytes(size), group.readOnly ? "read-only" : null].filter(Boolean).join(" · ")}</span></div></div>
    <div role="cell" className="model-state"><Badge>Not added</Badge></div>
    <div role="cell" />
    <div role="cell" />
    <div role="cell" className="row-end"><Button className="row-action" aria-label={`Add ${group.name}`} onClick={(event) => { event.stopPropagation(); onSelect(); }}>Add</Button></div>
  </div>;
}

export function ModelList({ models, selected, onSelect, mode, mutate, busy, online, idleControl, files = [], inventory, selectedFile, onSelectFile, onDiscover, client }: ListProps) {
  const [query, setQuery] = useState("");
  const [chosen, setChosen] = useState<Set<string>>(() => new Set());
  const [sort, setSort] = useState<{ key: SortKey; descending: boolean } | null>(null);
  const [checking, setChecking] = useState(false);
  const listed = useResource(client ?? placeholderClient, "models", parseUpdates, 0, true, !!client && mode === "models");
  const [checked, setChecked] = useState<string[] | null>(null);
  const updates = useMemo(() => new Set(checked ?? listed.data ?? []), [checked, listed.data]);
  useEffect(() => { setChosen((current) => { const ids = new Set(models.map((model) => model.id)); const next = new Set([...current].filter((id) => ids.has(id))); return next.size === current.size ? current : next; }); }, [models]);
  async function checkUpdates() {
    if (!client || checking) return;
    setChecking(true);
    try {
      const reply = await client.request<{ models?: unknown; failed_models?: Record<string, string>; checked?: number }>("models/check-updates", {});
      const found = Array.isArray(reply.models) ? reply.models.filter((id): id is string => typeof id === "string") : [];
      setChecked(found);
      const failed = Object.keys(reply.failed_models ?? {}).length;
      notify(found.length ? `Updates for ${found.join(", ")}.` : `No updates${typeof reply.checked === "number" ? ` for ${reply.checked} Hugging Face model${reply.checked === 1 ? "" : "s"}` : ""}${failed ? `; ${failed} couldn't be checked` : ""}.`);
    } catch (error) { notify(error instanceof Error ? error.message : "The update check failed.", "error"); }
    finally { setChecking(false); }
  }
  const [service, setService] = useState<Service | "files" | null>(null);
  const [dragging, setDragging] = useState("");
  const dragged = useRef("");
  const visible = sortModels(service === "files" ? [] : matchingModels(service ? models.filter((model) => offers(model, service)) : models, query), sort);
  const onSort = (key: SortKey) => setSort((current) => current?.key === key ? (current.descending ? null : { key, descending: true }) : { key, descending: false });
  const allChosen = visible.length > 0 && visible.every((model) => chosen.has(model.id));
  const toggle = (id: string) => setChosen((current) => { const next = new Set(current); if (next.has(id)) next.delete(id); else next.add(id); return next; });
  const visibleFiles = service === null || service === "files" ? files.filter((group) => fileMatches(group, query)) : [];
  const chips = services.map(([id, label]) => [id, label, models.filter((model) => offers(model, id)).length] as const).filter(([, , count]) => count > 0);
  const filtered = service !== null && service !== "files" && !chips.some(([id]) => id === service);
  useEffect(() => { if (filtered || (service === "files" && files.length === 0)) setService(null); }, [filtered, service, files.length]);
  const locked = busy || !online;
  const updateAction = client && mode === "models" && models.length > 0 ? <span className="update-action"><Button variant="ghost" disabled={!online || checking} onClick={() => void checkUpdates()}>{checking ? "Checking…" : "Check for updates"}</Button><HelpTip label="Check for updates">Compares each downloaded Hugging Face model with the file upstream. Nothing is downloaded.</HelpTip></span> : null;
  const disk = inventory && <span className="muted library-total">{bytes(inventory.totalBytes)} on disk<HelpTip label="disk use">Model files on this machine, including read-only libraries.{inventory.removableBytes > 0 ? ` ${bytes(inventory.removableBytes)} isn't used by a model and can be deleted.` : ""}{inventory.truncated ? " Large folders are counted up to a limit." : ""}</HelpTip></span>;
  return <section className="inventory" aria-label="Model inventory">
    <div className="toolbar">
      <input type="search" aria-label="Search models" placeholder="Search models" value={query} onChange={(event) => setQuery(event.target.value)} />
      {(chips.length > 1 || files.length > 0) && <div className="chip-filter" role="group" aria-label="Service filter"><button aria-pressed={service === null} onClick={() => setService(null)}>All <span>{models.length + files.length}</span></button>{chips.map(([id, label, count]) => <button key={id} aria-pressed={service === id} onClick={() => setService(service === id ? null : id)}>{label} <span>{count}</span></button>)}{files.length > 0 && <button aria-pressed={service === "files"} onClick={() => setService(service === "files" ? null : "files")}>Not added <span>{files.length}</span></button>}{disk}{updateAction}</div>}
      {!(chips.length > 1 || files.length > 0) && disk}
      {!(chips.length > 1 || files.length > 0) && updateAction}
    </div>
    {mode === "models" && chosen.size > 0 && <BulkBar models={models} chosen={chosen} mutate={mutate} disabled={locked} onClear={() => setChosen(new Set())} />}
    {models.length === 0 && files.length === 0 ? <Empty title="No models yet">{onDiscover ? <><button className="link-button" onClick={onDiscover}>Find a model</button> to get started.</> : "Find a model in Discover to get started."}</Empty> : visible.length === 0 && visibleFiles.length === 0 ? <Empty title="No matches">Try another search or filter.</Empty> : mode === "policy" ? <div className="policy-board">
      {(["hot", "on-demand", "disabled"] as Policy[]).map((policy) => <section className={`policy-lane ${dragging ? "droppable" : ""}`} key={policy} aria-label={`${policyNames[policy]} models`}
        onDragOver={(event) => { if (dragged.current) event.preventDefault(); }}
        onDrop={(event) => { event.preventDefault(); const model = models.find((item) => item.id === dragged.current); dragged.current = ""; setDragging(""); if (model && !(policy === "disabled" && model.active_requests > 0)) setPolicy(mutate, model, policy); }}>
        <header><h2>{policyNames[policy]}</h2><span className="muted">{visible.filter((model) => model.policy === policy).length}</span></header>
        <p className="muted">{policy === "hot" ? "Loaded at startup and kept ready" : policy === "on-demand" ? "Loads on the first request" : "Hidden from apps"}</p>
        {policy === "on-demand" && idleControl}
        {visible.filter((model) => model.policy === policy).map((model) => <div key={model.id} className={`lane-model ${selected === model.id ? "selected" : ""}`} draggable={!locked} onDragStart={(event) => { dragged.current = model.id; setDragging(model.id); event.dataTransfer.effectAllowed = "move"; }} onDragEnd={() => { dragged.current = ""; setDragging(""); }}>
          <button className="lane-open" aria-pressed={selected === model.id} onClick={() => onSelect(model.id)}><strong>{model.id}</strong><span className="muted"><span className={`status-dot ${workerTone(model)}`} /> {workerState(model)} · {bytes(model.resident_bytes)}</span></button>
          <PolicySelect model={model} mutate={mutate} disabled={locked} />
        </div>)}
      </section>)}
    </div> : <div className="data-table model-table" role="table" aria-label="Registered models">
      <div className="data-head" role="row"><SortHeader label="Model" id="name" sort={sort} onSort={onSort} pick={<input type="checkbox" className="pick" aria-label="Select all shown models" checked={allChosen} onChange={() => setChosen(allChosen ? new Set() : new Set(visible.map((model) => model.id)))} />} /><SortHeader label="State" id="state" sort={sort} onSort={onSort} /><span role="columnheader">Policy</span><SortHeader label="Memory" id="memory" sort={sort} onSort={onSort} numeric /><span role="columnheader"><span className="sr-only">Action</span></span></div>
      {visible.map((model) => <div className={`data-row model-row ${selected === model.id ? "selected" : ""} ${chosen.has(model.id) ? "chosen" : ""}`} role="row" key={model.id} onClick={() => onSelect(model.id)}>
        <div role="cell" className="model-identity picked"><input type="checkbox" className="pick" aria-label={`Select ${model.id}`} checked={chosen.has(model.id)} onClick={(event) => event.stopPropagation()} onChange={() => toggle(model.id)} /><div className="identity-text"><button className="model-name" aria-pressed={selected === model.id} onClick={(event) => { event.stopPropagation(); onSelect(model.id); }}>{model.id}</button><span className="model-meta">{model.operations.slice(0, 3).join(" · ")}{updates.has(model.id) && <> · <span className="update-note">update available</span></>}</span></div></div>
        <div role="cell" className="model-state" title={residencyNote(model) ?? undefined}><span className={`status-dot ${workerTone(model)}`} />{workerState(model)}{model.loaded ? <span className="muted"> · {model.active_requests}/{model.max_inflight}</span> : null}</div>
        <div role="cell"><PolicySelect model={model} mutate={mutate} disabled={locked} /></div>
        <div role="cell" className="numeric muted">{bytes(model.resident_bytes)}</div>
        <div role="cell" className="row-end"><RowAction model={model} mutate={mutate} disabled={locked} /></div>
      </div>)}
      {visibleFiles.map((group) => <FileRow key={group.id} group={group} selected={selectedFile === group.id} onSelect={() => onSelectFile?.(group.id)} />)}
    </div>}
  </section>;
}

export function Inspector({ model, busy, online, onClose, onDirty, mutate, client, stopLoad, inventory = null, onRemoved }: { model: Model; busy: boolean; online: boolean; onClose: () => void; onDirty: (value: boolean) => void; mutate: Mutate; client: Client; stopLoad?: () => void; inventory?: LocalInventory | null; onRemoved?: () => void }) {
  const [profileDirty, setProfileDirty] = useState(false);
  const [artifactDirty, setArtifactDirty] = useState(false);
  const [runtimeDirty, setRuntimeDirty] = useState(false);
  const dirty = profileDirty || artifactDirty || runtimeDirty;
  useEffect(() => { onDirty(dirty); }, [dirty, onDirty]);
  const loadBlocked = loadReason(model);
  const unloadBlocked = unloadReason(model);
  const cleanupRetry = model.state === "cleanup-pending" && model.cleanup_retry_available === true;
  const [tab, setTab] = useState<"overview" | "settings" | "manage">("overview");
  const repo = modelFiles(model, inventory).groups.find((group) => group.repo)?.repo ?? null;
  const [advanced, setAdvanced] = useState(() => readLocal("juiced.rewrite.inspectorAdvanced") === "1");
  const tabs = [["overview", "Overview", false], ["settings", "Settings", dirty], ["manage", "Manage", false]] as const;
  return <aside className="inspector" aria-label="Model inspector">
    <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={onClose}><Icon name="back" /> Back to models</Button><Button variant="ghost" className="close-inspector" aria-label="Close model inspector" onClick={onClose}><Icon name="close" /></Button></div>
    <div className="inspector-body">
      <div className="inspector-title"><h2>{model.id}</h2><Badge tone={workerTone(model)}>{workerState(model)}</Badge></div>
      {repo && <a className="model-card-link" href={`https://huggingface.co/${repo}`} target="_blank" rel="noreferrer noopener">{repo} <span aria-hidden="true">↗</span><span className="sr-only">: model card and README on Hugging Face</span></a>}
      <div className="tab-row"><div className="tabs" role="tablist" aria-label="Model sections">{tabs.map(([id, label, changed]) => <button key={id} role="tab" id={`tab-${id}`} aria-controls={`panel-${id}`} aria-selected={tab === id} onClick={() => setTab(id)}>{label}{changed && <span className="tab-dot" aria-label="unsaved" />}</button>)}</div>
        {tab === "settings" && <Switch checked={advanced} label="Show advanced settings" onChange={(next) => { setAdvanced(next); writeLocal("juiced.rewrite.inspectorAdvanced", next ? "1" : ""); }}>Advanced</Switch>}</div>
      {model.error && <p className={["request_termination_unconfirmed", "state_termination_unconfirmed"].includes(model.error) ? "inline-warning" : "inline-error"} role="alert">{workerFailure(model.error)}</p>}
      <div role="tabpanel" id="panel-overview" aria-labelledby="tab-overview" hidden={tab !== "overview"}>
      <section className="inspector-section"><span className="field-label" id="policy-label">Loading policy</span>
        <div className="segmented wide" role="radiogroup" aria-labelledby="policy-label">{(["hot", "on-demand", "disabled"] as Policy[]).map((policy) => <button key={policy} role="radio" aria-checked={model.policy === policy} disabled={busy || !online || dirty || (policy === "disabled" && model.active_requests > 0)} title={policy === "disabled" && model.active_requests > 0 ? "Requests are still running on this model" : policy === "disabled" && model.loaded ? "Disabling also unloads the model" : undefined} onClick={() => setPolicy(mutate, model, policy)}>{policyNames[policy]}</button>)}</div>
        <p className="field-help">{model.policy === "hot" ? (cleanupRetry ? "Retrying cleanup keeps the Hot policy." : "Loads at startup and stays loaded.") : model.policy === "on-demand" ? "Loads on the first request." : "Hidden from apps."}</p>
        <ModelIdle model={model} client={client} online={online} disabled={busy || dirty} />
      </section>
      <section className="inspector-section"><h3>Worker</h3><dl className="facts"><div><dt>Process</dt><dd>{model.pid ?? "Not running"}</dd></div><div><dt>Requests</dt><dd>{model.active_requests} / {model.max_inflight}</dd></div><div><dt>Reserved</dt><dd>{bytes(model.resident_bytes)}</dd></div><div><dt>Weights</dt><dd>{model.downloaded ? "Present" : "Missing"}</dd></div></dl>
        {(model.uncertain_state_operations ?? 0) > 0 && <p className="inline-warning">A snapshot didn't confirm it finished. Its slot and memory stay held until you retry cleanup.</p>}
        {(model.uncertain_requests ?? 0) > 0 && <p className="inline-warning">The last request didn't confirm it stopped. Retry cleanup to release its slot.</p>}
        <div className="action-row wrap">{model.loaded || cleanupRetry
          ? <Button disabled={busy || !online || !!unloadBlocked || dirty} onClick={() => void mutate("unload", { model: model.id }, cleanupRetry ? `Confirmed cleanup for ${model.id}.` : `Unloaded ${model.id}.`)}>{cleanupRetry ? "Retry cleanup" : "Unload"}</Button>
          : <Button variant="primary" disabled={busy || !online || !!loadBlocked || dirty} onClick={() => void mutate("load", { model: model.id }, `Loaded ${model.id}.`)}>Load worker</Button>}
          {stopLoad && <Button onClick={stopLoad}>Stop waiting</Button>}</div>
        {(model.policy === "disabled" || !model.downloaded || model.active_requests > 0 || model.policy === "hot") && <p className="field-help">{!model.loaded ? loadBlocked : unloadBlocked}</p>}
        {dirty && <p className="field-help">Save or cancel your changes first.</p>}
      </section>
      </div>
      <div role="tabpanel" id="panel-settings" aria-labelledby="tab-settings" hidden={tab !== "settings"}>
      <ContextInspector model={model} client={client} online={online} busy={busy} policyDirty={artifactDirty || runtimeDirty} onDirty={setProfileDirty} mutate={mutate} advanced={advanced} />
      <QuantSync model={model} client={client} online={online} busy={busy} blocked={dirty} />
      {(advanced || artifactDirty) && model.artifact_set && <ArtifactRevisionInspector model={model} client={client} busy={busy} online={online} otherDirty={profileDirty || runtimeDirty} onDirty={setArtifactDirty} mutate={mutate} />}
      {(advanced || runtimeDirty) && <RuntimeInspector model={model} client={client} busy={busy} online={online} otherDirty={profileDirty || artifactDirty} onDirty={setRuntimeDirty} mutate={mutate} />}
      </div>
      <div role="tabpanel" id="panel-manage" aria-labelledby="tab-manage" hidden={tab !== "manage"}>
      <section className="inspector-section"><h3>Operations</h3><div className="chip-row">{model.operations.map((operation) => <code className="chip" key={operation}>{operation}</code>)}</div></section>
      <ModelFiles model={model} inventory={inventory} />
      <section className="inspector-section"><h3>Remove registration</h3><p className="muted">{removalNote(model, inventory)}</p><Button variant="danger" disabled={busy || !online || dirty || !!removalReason(model)} onClick={() => void mutate("delete", { model: model.id, expected_revision: model.revision }, `Removed ${model.id}. Weight files kept.`).then((removed) => { if (removed) onRemoved?.(); })}>Remove registration</Button>{removalReason(model) && <p className="field-help">{removalReason(model)}</p>}</section>
      </div>
    </div>
  </aside>;
}

function readLocal(key: string) { try { return localStorage.getItem(key) ?? ""; } catch { return ""; } }
function writeLocal(key: string, value: string) { try { if (value) localStorage.setItem(key, value); else localStorage.removeItem(key); } catch { /* Storage can be unavailable in private sessions. */ } }
