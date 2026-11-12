import { useEffect, useRef, useState } from "react";
import { bytes, loadReason, matchingModels, policyNames, unloadReason, workerState, workerTone, parseSuitability, type Suitability, type Model, type Policy } from "./domain";
import { Badge, Button, Empty, Icon } from "./ui";
import type { Client } from "./api";
import { removalReason } from "./management";
import { ContextInspector } from "./ContextInspector";
import { ArtifactRevisionInspector } from "./ArtifactRevisionInspector";
import { RuntimeInspector } from "./RuntimeInspector";

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
  if (code === "resident_budget") return "The declared resident budget is full. Unload an idle On demand worker, then retry.";
  if (code === "missing_weights") return "The configured model file is unavailable. Check its path on the service host.";
  if (code === "worker_exited") return "The engine exited. Review its launch configuration, then retry loading.";
  return "The worker needs attention. Review its current state before retrying.";
}

export function ModelList({ models, selected, onSelect, mode }: { models: Model[]; selected?: string; onSelect: (id: string) => void; mode: "models" | "policy" | "library" }) {
  const [query, setQuery] = useState("");
  const visible = matchingModels(models, query);
  return <section className="inventory" aria-label="Model inventory">
    <div className="list-toolbar"><label className="search-label"><span className="sr-only">Search models</span><input type="search" placeholder="Filter models, policies, operations…" value={query} onChange={(event) => setQuery(event.target.value)} /></label><span className="muted count">{visible.length} models</span></div>
    {models.length === 0 ? <Empty title="No models registered">Add an existing model file and engine to the isolated service configuration. New entries default to Disabled.</Empty> : visible.length === 0 ? <Empty title="No matching models">Change or clear the filter to see your registered models.</Empty> : mode === "policy" ? <div className="policy-board">
      {(["hot", "on-demand", "disabled"] as Policy[]).map((policy) => <section className="policy-lane" key={policy}><h2>{policyNames[policy]} <span className="muted">{visible.filter((model) => model.policy === policy).length}</span></h2><p className="muted">{policy === "hot" ? "Protected from idle unload" : policy === "on-demand" ? "Loads when a client calls" : "Hidden from client discovery"}</p>
        {visible.filter((model) => model.policy === policy).map((model) => <button key={model.id} className={`lane-model ${selected === model.id ? "selected" : ""}`} aria-pressed={selected === model.id} onClick={() => onSelect(model.id)}><strong>{model.id}</strong><span className="muted">{workerState(model)} · {bytes(model.resident_bytes)} declared reservation</span></button>)}
      </section>)}
    </div> : <div className="model-table" role="table" aria-label="Registered models">
      <div className="table-heading" role="row"><span role="columnheader">Model</span><span role="columnheader">{mode === "library" ? "Weights" : "Policy"}</span><span role="columnheader">Reservation</span></div>
      {visible.map((model) => <div className={`model-row ${selected === model.id ? "selected" : ""}`} role="row" key={model.id}>
        <div role="cell" className="model-identity"><button className="model-name" aria-pressed={selected === model.id} onClick={() => onSelect(model.id)}>{model.id}</button><span className="model-meta"><span className={`status-dot ${workerTone(model)}`} />{workerState(model)}{model.active_requests > 0 ? ` · ${model.active_requests} active` : ""}</span></div>
        <div role="cell">{mode === "library" ? <Badge tone={model.downloaded ? "good" : "warning"}>{model.downloaded ? "Available" : "Missing"}</Badge> : <Badge tone={model.policy === "hot" ? "good" : "quiet"}>{policyNames[model.policy]}</Badge>}</div>
        <div role="cell" className="numeric muted">{bytes(model.resident_bytes)}</div>
      </div>)}
    </div>}
  </section>;
}

export function Inspector({ model, busy, online, onClose, onDirty, mutate, client, stopLoad }: { model: Model; busy: boolean; online: boolean; onClose: () => void; onDirty: (value: boolean) => void; mutate: Mutate; client: Client; stopLoad?: () => void }) {
  const [draft, setDraft] = useState(model.policy);
  const [saved, setSaved] = useState(model.policy);
  const revision = useRef(model.revision);
  const [profileDirty, setProfileDirty] = useState(false);
  const [artifactDirty, setArtifactDirty] = useState(false);
  const [runtimeDirty, setRuntimeDirty] = useState(false);
  const policyDirty = draft !== saved;
  const dirty = policyDirty || profileDirty || artifactDirty || runtimeDirty;
  useEffect(() => { onDirty(dirty); }, [dirty, onDirty]);
  useEffect(() => {
    if (!policyDirty) { setDraft(model.policy); setSaved(model.policy); revision.current = model.revision; }
  }, [model.policy, model.revision, policyDirty]);
  const loadBlocked = loadReason(model);
  const unloadBlocked = unloadReason(model);
  const cleanupRetry = model.state === "cleanup-pending" && model.cleanup_retry_available === true;
  const policyBlocked = draft === "disabled" && model.loaded;
  async function save() {
    if (await mutate("juiced/policy", { model: model.id, policy: draft, expected_revision: revision.current }, `Saved ${policyNames[draft]} policy for ${model.id}.`)) {
      revision.current += 1;
      setSaved(draft);
    }
  }
  return <aside className="inspector" aria-label="Model inspector">
    <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={onClose}><Icon name="back" /> Back to models</Button><Button variant="ghost" className="close-inspector" aria-label="Close model inspector" onClick={onClose}><Icon name="close" /></Button></div>
    <div className="inspector-body">
      <div className="inspector-title"><span className="eyebrow">Model inspector</span><h2>{model.id}</h2><Badge tone={workerTone(model)}>{workerState(model)}</Badge></div>
      {model.error && <p className={["request_termination_unconfirmed", "state_termination_unconfirmed"].includes(model.error) ? "inline-warning" : "inline-error"} role="alert">{workerFailure(model.error)}</p>}
      <section className="inspector-section"><h3>Loading policy</h3><p className="muted">Saved intent controls availability. Worker state is observed separately.</p><label htmlFor="model-policy">Policy</label><select id="model-policy" value={draft} disabled={busy || !online || profileDirty || artifactDirty || runtimeDirty} onChange={(event) => setDraft(event.target.value as Policy)}>
        <option value="disabled">Disabled</option><option value="on-demand">On demand</option><option value="hot">Hot</option>
      </select>
      {draft === "hot" && <p className="field-help">{cleanupRetry ? "Retrying this worker cleanup preserves its saved Hot policy." : "Hot workers load at service startup and cannot be unloaded until their policy changes. Use Load worker to start this worker now."}</p>}
      {policyBlocked && <p className="inline-warning">Switch to On demand and unload this worker before disabling it.</p>}
      {policyDirty && revision.current !== model.revision && <p className="inline-warning">Another client changed this policy. Your draft is retained. Cancel draft to review the latest policy.</p>}
      </section>
      <section className="inspector-section"><h3>Worker</h3><dl className="facts"><div><dt>Process</dt><dd>{model.pid ?? "Not running"}</dd></div><div><dt>Active requests</dt><dd>{model.active_requests} / {model.max_inflight} slots</dd></div><div><dt>Declared reservation</dt><dd>{bytes(model.resident_bytes)}</dd></div><div><dt>Memory measurement</dt><dd>Unavailable</dd></div><div><dt>Weights</dt><dd>{model.downloaded ? "Available on host" : "Missing on host"}</dd></div></dl>
        {(model.uncertain_state_operations ?? 0) > 0 && <p className="inline-warning">A snapshot operation ended before the engine confirmed completion. Its slot, memory and file protection remain held. Retry cleanup to confirm this worker has exited. Retained disk reservations need a separate recovery check.</p>}
        {(model.uncertain_requests ?? 0) > 0 && <p className="inline-warning">The previous request ended without confirming that engine work stopped. Its slot and memory remain reserved. Retry cleanup to confirm this worker has exited.</p>}
        {model.startup_waiting && <p className="field-help">Waiting to start this worker. It has no process or resident reservation yet. Disabled cancels the pending start.</p>}
        <div className="action-row wrap"><Button disabled={busy || !online || !!loadBlocked || dirty} onClick={() => void mutate("load", { model: model.id }, `Loaded ${model.id}.`)}>Load worker</Button><Button disabled={busy || !online || !!unloadBlocked || dirty} onClick={() => void mutate("unload", { model: model.id }, cleanupRetry ? `Confirmed cleanup for ${model.id}.` : `Unloaded ${model.id}.`)}>{cleanupRetry ? "Retry cleanup" : "Unload"}</Button>{stopLoad && <Button onClick={stopLoad}>Stop waiting</Button>}</div>
        {(model.policy === "disabled" || !model.downloaded || model.active_requests > 0 || model.policy === "hot") && <p className="field-help">{!model.loaded ? loadBlocked : unloadBlocked}</p>}
        {dirty && <p className="field-help">Save or cancel all drafts before changing worker state.</p>}
      </section>
      {model.operations.includes("audio/transcriptions") ? <section className="inspector-section"><h3>Speech worker</h3><p className="muted">The selected speech profile uses one physical slot and its existing audio model. Text context launch settings do not apply.</p></section> : <ContextInspector model={model} client={client} online={online} busy={busy} policyDirty={policyDirty || artifactDirty || runtimeDirty} onDirty={setProfileDirty} mutate={mutate} />}
      {model.artifact_set && <ArtifactRevisionInspector model={model} client={client} busy={busy} online={online} otherDirty={policyDirty || profileDirty || runtimeDirty} onDirty={setArtifactDirty} mutate={mutate} />}
      <RuntimeInspector model={model} client={client} busy={busy} online={online} otherDirty={policyDirty || profileDirty || artifactDirty} onDirty={setRuntimeDirty} mutate={mutate} />
      <HostFit model={model} client={client} dirty={dirty} online={online} />
      <section className="inspector-section"><h3>Declared operations</h3><ul className="operation-list">{model.operations.map((operation) => <li key={operation}><code>{operation}</code></li>)}</ul><p className="field-help">Declared by the service configuration. Hardware and inference qualification are not yet measured.</p></section>
      <section className="inspector-section"><h3>Remove registration</h3><p className="muted">Removes this model from the appliance catalog. Its weight files are retained.</p><Button variant="danger" disabled={busy || !online || dirty || !!removalReason(model)} onClick={() => void mutate("delete", { model: model.id, expected_revision: model.revision }, `Removed ${model.id} registration. Weight files retained.`)}>Remove registration</Button><p className="field-help">{removalReason(model) ?? "The model is Disabled and unloaded. Its registration can be removed."}</p></section>
    </div>
    <div className="inspector-footer"><span className="muted">{dirty ? "Unsaved changes" : `Policy revision ${model.revision}`}</span><div className="action-row"><Button variant="ghost" disabled={!policyDirty || busy} onClick={() => { setDraft(model.policy); setSaved(model.policy); revision.current = model.revision; }}>Cancel draft</Button><Button variant="primary" disabled={!policyDirty || profileDirty || artifactDirty || runtimeDirty || busy || !online || policyBlocked} onClick={() => void save()}>{busy && policyDirty ? "Saving…" : "Save policy"}</Button></div></div>
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
    <h3>Host fit</h3><p className="muted">Inspect saved intent without enabling, loading or reserving resources.</p>
    <Button disabled={dirty || checking || !online} onClick={() => void check()}>{checking ? "Checking…" : "Check saved settings"}</Button>
    {dirty && <p className="field-help">Save or cancel the draft before checking saved settings.</p>}
    {error && <p className="inline-error" role="alert">{error}</p>}
    {result && <div className="fit-observation" aria-live="polite"><Badge tone="warning">Physical fit unknown</Badge><p className="field-help">A budget check does not qualify physical fit or model quality.</p><dl className="facts"><div><dt>Declared budget available</dt><dd>{bytes(result.resources.declared_budget.available_bytes)}</dd></div><div><dt>Additional worker reservation</dt><dd>{bytes(result.resources.declared_budget.additional_worker_bytes)}</dd></div><div><dt>Declared budget check</dt><dd>{result.resources.declared_budget.fits_additional_reservation === null ? "Unknown" : result.resources.declared_budget.fits_additional_reservation ? "Within ceiling" : "Blocked"}</dd></div></dl>{result.resources.blockers.map((code) => <p className="inline-warning" key={code}>{blockers[code] ?? "The service reported an admission blocker."}</p>)}<p className="field-help">This observation is not hardware qualification. Loads recheck current policy and resources.</p></div>}
  </section>;
}
