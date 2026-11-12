import { useEffect, useRef, useState, type ReactNode } from "react";
import { bytes, loadReason, matchingModels, offers, policyNames, services, unloadReason, workerState, workerTone, parseSuitability, type Suitability, type Model, type Policy, type Service } from "./domain";
import { Badge, Button, Empty, HelpTip, Icon } from "./ui";
import type { Client } from "./api";
import { removalReason } from "./management";
import { ContextInspector } from "./ContextInspector";
import { ArtifactRevisionInspector } from "./ArtifactRevisionInspector";
import { RuntimeInspector } from "./RuntimeInspector";
import { ModelIdle, residencyNote } from "./Residency";

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

type ListProps = { models: Model[]; selected?: string; onSelect: (id: string) => void; mode: "models" | "policy"; mutate: Mutate; busy: boolean; online: boolean; idleControl?: ReactNode };

function setPolicy(mutate: Mutate, model: Model, policy: Policy) {
  if (policy === model.policy) return;
  void mutate("juiced/policy", { model: model.id, policy, expected_revision: model.revision }, `${model.id} is now ${policyNames[policy]}.`);
}

function PolicySelect({ model, mutate, disabled }: { model: Model; mutate: Mutate; disabled: boolean }) {
  return <select className="policy-select" aria-label={`Loading policy for ${model.id}`} data-policy={model.policy} value={model.policy} disabled={disabled} onClick={(event) => event.stopPropagation()} onChange={(event) => setPolicy(mutate, model, event.target.value as Policy)}>
    <option value="hot">Hot</option><option value="on-demand">On demand</option><option value="disabled" disabled={model.loaded}>Disabled</option>
  </select>;
}

function RowAction({ model, mutate, disabled }: { model: Model; mutate: Mutate; disabled: boolean }) {
  const unload = model.loaded || model.state === "cleanup-pending";
  const reason = unload ? unloadReason(model) : loadReason(model);
  return <Button className="row-action" disabled={disabled || !!reason} title={reason ?? undefined} onClick={(event) => { event.stopPropagation(); void mutate(unload ? "unload" : "load", { model: model.id }, unload ? `Unloaded ${model.id}.` : `Loaded ${model.id}.`); }}>{unload ? "Unload" : "Load"}</Button>;
}

export function ModelList({ models, selected, onSelect, mode, mutate, busy, online, idleControl }: ListProps) {
  const [query, setQuery] = useState("");
  const [service, setService] = useState<Service | null>(null);
  const [dragging, setDragging] = useState("");
  const visible = matchingModels(service ? models.filter((model) => offers(model, service)) : models, query);
  const chips = services.map(([id, label]) => [id, label, models.filter((model) => offers(model, id)).length] as const).filter(([, , count]) => count > 0);
  const locked = busy || !online;
  return <section className="inventory" aria-label="Model inventory">
    <div className="toolbar">
      <input type="search" aria-label="Search models" placeholder="Search models" value={query} onChange={(event) => setQuery(event.target.value)} />
      {chips.length > 1 && <div className="chip-filter" role="group" aria-label="Service filter"><button aria-pressed={service === null} onClick={() => setService(null)}>All <span>{models.length}</span></button>{chips.map(([id, label, count]) => <button key={id} aria-pressed={service === id} onClick={() => setService(service === id ? null : id)}>{label} <span>{count}</span></button>)}</div>}
    </div>
    {models.length === 0 ? <Empty title="No models yet">Find a model in Library to get started.</Empty> : visible.length === 0 ? <Empty title="No matches">Try another search or filter.</Empty> : mode === "policy" ? <div className="policy-board">
      {(["hot", "on-demand", "disabled"] as Policy[]).map((policy) => <section className={`policy-lane ${dragging ? "droppable" : ""}`} key={policy} aria-label={`${policyNames[policy]} models`}
        onDragOver={(event) => { if (dragging) event.preventDefault(); }}
        onDrop={(event) => { event.preventDefault(); const model = models.find((item) => item.id === dragging); setDragging(""); if (model && !(policy === "disabled" && model.loaded)) setPolicy(mutate, model, policy); }}>
        <header><h2>{policyNames[policy]}</h2><span className="muted">{visible.filter((model) => model.policy === policy).length}</span></header>
        <p className="muted">{policy === "hot" ? "Loaded at startup and kept ready" : policy === "on-demand" ? "Loads on the first request" : "Hidden from apps"}</p>
        {policy === "on-demand" && idleControl}
        {visible.filter((model) => model.policy === policy).map((model) => <div key={model.id} className={`lane-model ${selected === model.id ? "selected" : ""}`} draggable={!locked} onDragStart={(event) => { setDragging(model.id); event.dataTransfer.effectAllowed = "move"; }} onDragEnd={() => setDragging("")}>
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
    </div>}
  </section>;
}

export function Inspector({ model, busy, online, onClose, onDirty, mutate, client, stopLoad }: { model: Model; busy: boolean; online: boolean; onClose: () => void; onDirty: (value: boolean) => void; mutate: Mutate; client: Client; stopLoad?: () => void }) {
  const [profileDirty, setProfileDirty] = useState(false);
  const [artifactDirty, setArtifactDirty] = useState(false);
  const [runtimeDirty, setRuntimeDirty] = useState(false);
  const dirty = profileDirty || artifactDirty || runtimeDirty;
  useEffect(() => { onDirty(dirty); }, [dirty, onDirty]);
  const loadBlocked = loadReason(model);
  const unloadBlocked = unloadReason(model);
  const cleanupRetry = model.state === "cleanup-pending" && model.cleanup_retry_available === true;
  const [tab, setTab] = useState<"overview" | "settings" | "manage">("overview");
  const tabs = [["overview", "Overview", false], ["settings", "Settings", dirty], ["manage", "Manage", false]] as const;
  return <aside className="inspector" aria-label="Model inspector">
    <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={onClose}><Icon name="back" /> Back to models</Button><Button variant="ghost" className="close-inspector" aria-label="Close model inspector" onClick={onClose}><Icon name="close" /></Button></div>
    <div className="inspector-body">
      <div className="inspector-title"><h2>{model.id}</h2><Badge tone={workerTone(model)}>{workerState(model)}</Badge></div>
      <div className="tabs" role="tablist" aria-label="Model sections">{tabs.map(([id, label, changed]) => <button key={id} role="tab" id={`tab-${id}`} aria-controls={`panel-${id}`} aria-selected={tab === id} onClick={() => setTab(id)}>{label}{changed && <span className="tab-dot" aria-label="unsaved" />}</button>)}</div>
      {model.error && <p className={["request_termination_unconfirmed", "state_termination_unconfirmed"].includes(model.error) ? "inline-warning" : "inline-error"} role="alert">{workerFailure(model.error)}</p>}
      <div role="tabpanel" id="panel-overview" aria-labelledby="tab-overview" hidden={tab !== "overview"}>
      <section className="inspector-section"><span className="field-label" id="policy-label">Loading policy</span>
        <div className="segmented wide" role="radiogroup" aria-labelledby="policy-label">{(["hot", "on-demand", "disabled"] as Policy[]).map((policy) => <button key={policy} role="radio" aria-checked={model.policy === policy} disabled={busy || !online || dirty || (policy === "disabled" && model.loaded)} title={policy === "disabled" && model.loaded ? "Unload the model before disabling it" : undefined} onClick={() => setPolicy(mutate, model, policy)}>{policyNames[policy]}</button>)}</div>
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
      {model.operations.includes("audio/transcriptions") ? <section className="inspector-section"><p className="muted">Speech models use one slot. Text context settings don't apply.</p></section> : <ContextInspector model={model} client={client} online={online} busy={busy} policyDirty={artifactDirty || runtimeDirty} onDirty={setProfileDirty} mutate={mutate} />}
      {model.artifact_set && <ArtifactRevisionInspector model={model} client={client} busy={busy} online={online} otherDirty={profileDirty || runtimeDirty} onDirty={setArtifactDirty} mutate={mutate} />}
      <RuntimeInspector model={model} client={client} busy={busy} online={online} otherDirty={profileDirty || artifactDirty} onDirty={setRuntimeDirty} mutate={mutate} />
      </div>
      <div role="tabpanel" id="panel-manage" aria-labelledby="tab-manage" hidden={tab !== "manage"}>
      <HostFit model={model} client={client} dirty={dirty} online={online} />
      <section className="inspector-section"><h3>Operations</h3><div className="chip-row">{model.operations.map((operation) => <code className="chip" key={operation}>{operation}</code>)}</div></section>
      <section className="inspector-section"><h3>Remove registration</h3><p className="muted">Removes the model. Its weight files are kept.</p><Button variant="danger" disabled={busy || !online || dirty || !!removalReason(model)} onClick={() => void mutate("delete", { model: model.id, expected_revision: model.revision }, `Removed ${model.id}. Weight files kept.`)}>Remove registration</Button>{removalReason(model) && <p className="field-help">{removalReason(model)}</p>}</section>
      </div>
    </div>
  </aside>;
}


function HostFit({ model, client, dirty, online }: { model: Model; client: Client; dirty: boolean; online: boolean }) {
  const [result, setResult] = useState<Suitability | null>(null);
  const [error, setError] = useState("");
  const [checking, setChecking] = useState(false);
  const request = useRef<AbortController | null>(null);
  useEffect(() => {
    request.current?.abort();
    setResult(null); setError(""); setChecking(false);
    return () => { request.current?.abort(); };
  }, [model.id, model.revision, model.state, client, dirty]);
  async function check() {
    request.current?.abort();
    const controller = new AbortController(); request.current = controller;
    setChecking(true); setError(""); setResult(null);
    try {
      const observation = parseSuitability(await client.request(`juiced/suitability?model=${encodeURIComponent(model.id)}`, undefined, controller.signal));
      if (!controller.signal.aborted) setResult(observation);
    } catch (error) {
      if (!controller.signal.aborted) setError(error instanceof Error ? error.message : "The suitability check failed. Retry the check.");
    } finally { if (!controller.signal.aborted) setChecking(false); }
  }
  const blockers: Record<string, string> = {
    model_disabled: "The model is Disabled.", missing_weights: "The model file is unavailable.",
    worker_transition: "The worker is starting.", shutting_down: "The service is shutting down.",
    resident_budget: "The declared budget has no room for an additional worker.",
    unknown_host_capacity: "A complete, fresh host-memory observation is unavailable.",
    host_memory_exhausted: "The host pool cannot admit this load while preserving its headroom.",
    resource_profile_required: "This model needs a resource profile before protected execution.",
    protected_profile_unqualified: "This execution profile has no selected protected resource contract.",
    state_termination_unconfirmed: "Confirm cleanup of the prior snapshot operation before using this worker.",
    request_termination_unconfirmed: "Confirm cleanup of the prior request before using this worker.",
  };
  return <section className="inspector-section" aria-label="Host fit">
    <div className="section-heading"><h3>Host fit</h3><HelpTip label="Host fit">Checks the saved settings against the memory budget. Nothing loads or gets reserved. Physical fit on this hardware isn't measured yet.</HelpTip></div>
    <Button disabled={dirty || checking || !online} onClick={() => void check()}>{checking ? "Checking…" : "Check saved settings"}</Button>
    {dirty && <p className="field-help">Save or cancel your changes first.</p>}
    {error && <p className="inline-error" role="alert">{error}</p>}
    {result && (model.loaded ? <div className="fit-observation" aria-live="polite"><dl className="facts"><div><dt>Holding now</dt><dd>{bytes(model.resident_bytes)}</dd></div><div><dt>Budget left for others</dt><dd>{bytes(result.resources.declared_budget.available_bytes)}</dd></div></dl><p className="field-help">Loaded and running with these settings.</p></div>
      : <div className="fit-observation" aria-live="polite"><dl className="facts"><div><dt>Budget free</dt><dd>{bytes(result.resources.declared_budget.available_bytes)}</dd></div><div><dt>This model needs</dt><dd>{bytes(result.resources.declared_budget.additional_worker_bytes)}</dd></div><div><dt>Fits budget</dt><dd>{result.resources.declared_budget.fits_additional_reservation === null ? "Unknown" : result.resources.declared_budget.fits_additional_reservation ? "Yes" : "No"}</dd></div></dl>{result.resources.blockers.map((code) => <p className="inline-warning" key={code}>{blockers[code] ?? "The service reported a blocker."}</p>)}</div>)}
  </section>;
}
