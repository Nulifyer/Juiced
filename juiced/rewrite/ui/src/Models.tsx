import { useEffect, useRef, useState, type ReactNode } from "react";
import { bytes, loadReason, matchingModels, offers, policyNames, services, unloadReason, workerState, workerTone, type Model, type Policy, type Service } from "./domain";
import { Badge, Button, Empty, HelpTip, Icon, Switch } from "./ui";
import type { Client } from "./api";
import { removalReason } from "./management";
import { ContextInspector } from "./ContextInspector";
import { QuantSync } from "./QuantSync";
import { ArtifactRevisionInspector } from "./ArtifactRevisionInspector";
import { RuntimeInspector } from "./RuntimeInspector";
import { ModelIdle, residencyNote } from "./Residency";
import { ModelFiles, removalNote, unusedQuants } from "./LocalModels";
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

type ListProps = { models: Model[]; selected?: string; onSelect: (id: string) => void; mode: "models" | "policy"; mutate: Mutate; busy: boolean; online: boolean; idleControl?: ReactNode; files?: LocalGroup[]; inventory?: LocalInventory | null; selectedFile?: string; onSelectFile?: (id: string) => void; onDiscover?: () => void };

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
    <div role="cell" className="model-identity"><button className="model-name" aria-pressed={selected} onClick={(event) => { event.stopPropagation(); onSelect(); }}>{group.name}</button><span className="model-meta">{[group.publisher, unused.map((quant) => quant.label).join(", ") || null, bytes(size), group.readOnly ? "read-only" : null].filter(Boolean).join(" · ")}</span></div>
    <div role="cell" className="model-state"><Badge>Not added</Badge></div>
    <div role="cell" />
    <div role="cell" />
    <div role="cell" className="row-end"><Button className="row-action" aria-label={`Add ${group.name}`} onClick={(event) => { event.stopPropagation(); onSelect(); }}>Add</Button></div>
  </div>;
}

export function ModelList({ models, selected, onSelect, mode, mutate, busy, online, idleControl, files = [], inventory, selectedFile, onSelectFile, onDiscover }: ListProps) {
  const [query, setQuery] = useState("");
  const [service, setService] = useState<Service | "files" | null>(null);
  const [dragging, setDragging] = useState("");
  const dragged = useRef("");
  const visible = service === "files" ? [] : matchingModels(service ? models.filter((model) => offers(model, service)) : models, query);
  const visibleFiles = service === null || service === "files" ? files.filter((group) => fileMatches(group, query)) : [];
  const chips = services.map(([id, label]) => [id, label, models.filter((model) => offers(model, id)).length] as const).filter(([, , count]) => count > 0);
  const filtered = service !== null && service !== "files" && !chips.some(([id]) => id === service);
  useEffect(() => { if (filtered || (service === "files" && files.length === 0)) setService(null); }, [filtered, service, files.length]);
  const locked = busy || !online;
  const disk = inventory && <span className="muted library-total">{bytes(inventory.totalBytes)} on disk<HelpTip label="disk use">Model files on this machine, including read-only libraries.{inventory.removableBytes > 0 ? ` ${bytes(inventory.removableBytes)} isn't used by a model and can be deleted.` : ""}{inventory.truncated ? " Large folders are counted up to a limit." : ""}</HelpTip></span>;
  return <section className="inventory" aria-label="Model inventory">
    <div className="toolbar">
      <input type="search" aria-label="Search models" placeholder="Search models" value={query} onChange={(event) => setQuery(event.target.value)} />
      {(chips.length > 1 || files.length > 0) && <div className="chip-filter" role="group" aria-label="Service filter"><button aria-pressed={service === null} onClick={() => setService(null)}>All <span>{models.length + files.length}</span></button>{chips.map(([id, label, count]) => <button key={id} aria-pressed={service === id} onClick={() => setService(service === id ? null : id)}>{label} <span>{count}</span></button>)}{files.length > 0 && <button aria-pressed={service === "files"} onClick={() => setService(service === "files" ? null : "files")}>Not added <span>{files.length}</span></button>}{disk}</div>}
      {!(chips.length > 1 || files.length > 0) && disk}
    </div>
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
      <div className="data-head" role="row"><span role="columnheader">Model</span><span role="columnheader">State</span><span role="columnheader">Policy</span><span role="columnheader" className="numeric">Memory</span><span role="columnheader"><span className="sr-only">Action</span></span></div>
      {visible.map((model) => <div className={`data-row model-row ${selected === model.id ? "selected" : ""}`} role="row" key={model.id} onClick={() => onSelect(model.id)}>
        <div role="cell" className="model-identity"><button className="model-name" aria-pressed={selected === model.id} onClick={(event) => { event.stopPropagation(); onSelect(model.id); }}>{model.id}</button><span className="model-meta">{model.operations.slice(0, 3).join(" · ")}</span></div>
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
  const [advanced, setAdvanced] = useState(() => readLocal("juiced.rewrite.inspectorAdvanced") === "1");
  const tabs = [["overview", "Overview", false], ["settings", "Settings", dirty], ["manage", "Manage", false]] as const;
  return <aside className="inspector" aria-label="Model inspector">
    <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={onClose}><Icon name="back" /> Back to models</Button><Button variant="ghost" className="close-inspector" aria-label="Close model inspector" onClick={onClose}><Icon name="close" /></Button></div>
    <div className="inspector-body">
      <div className="inspector-title"><h2>{model.id}</h2><Badge tone={workerTone(model)}>{workerState(model)}</Badge></div>
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
      {model.operations.includes("audio/transcriptions") ? <section className="inspector-section"><p className="muted">Speech models use one slot. Text context settings don't apply.</p></section> : <ContextInspector model={model} client={client} online={online} busy={busy} policyDirty={artifactDirty || runtimeDirty} onDirty={setProfileDirty} mutate={mutate} advanced={advanced} />}
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
