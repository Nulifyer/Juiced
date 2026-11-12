import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import type { Model } from "./domain";
import type { Mutate } from "./Models";
import { launchDraft, editedLaunchOptions, parseModelContext, parseModelOptions, sameDraft, servingContext, type LaunchDraft, type ModelOptions } from "./profiles";
import { Badge, Button } from "./ui";
import { useResource } from "./useResource";

const emptyDraft: LaunchDraft = { context: "", threads: "", gpu: "" };
function tokens(value: number | null | undefined) { return value == null ? "Unknown" : `${value.toLocaleString()} tokens`; }

export function ContextInspector({ model, client, online, busy, policyDirty, onDirty, mutate }: { model: Model; client: Client; online: boolean; busy: boolean; policyDirty: boolean; onDirty: (dirty: boolean) => void; mutate: Mutate }) {
  const options = useResource(client, `models/${encodeURIComponent(model.id)}/options`, parseModelOptions, 0);
  const context = useResource(client, `juiced/context?model=${encodeURIComponent(model.id)}`, parseModelContext, 0);
  const [draft, setDraft] = useState(emptyDraft); const [saved, setSaved] = useState(emptyDraft);
  const [validationError, setValidationError] = useState("");
  const [pendingSave, setPendingSave] = useState<number | null>(null);
  const revision = useRef<number | null>(null);
  const last = useRef({ revision: model.revision, state: model.state });
  const dirty = !sameDraft(draft, saved);
  const view = options.data?.model === model.id ? options.data : null;
  const observation = context.data?.model === model.id ? context.data : null;
  const imported = view?.options_authority === "native-settings";
  const editable = view?.provider === "llama-cpp" && !model.loaded && model.state !== "loading" && model.active_requests === 0;
  const blocked = !editable || !online || busy || policyDirty || !!options.error || !view || pendingSave !== null;
  const accept = (fresh: ModelOptions) => {
    const value = launchDraft(fresh.saved);
    setDraft(value); setSaved(value); revision.current = fresh.revision;
  };
  useEffect(() => { onDirty(dirty); }, [dirty, onDirty]);
  useEffect(() => {
    if (view && (!dirty || (pendingSave !== null && view.revision > pendingSave))) {
      accept(view);
      if (pendingSave !== null) setPendingSave(null);
    }
  }, [view, dirty, pendingSave]);
  useEffect(() => {
    if (last.current.revision !== model.revision) { void options.reload(); void context.reload(); }
    else if (last.current.state !== model.state) void context.reload();
    last.current = { revision: model.revision, state: model.state };
  }, [model.revision, model.state, options.reload, context.reload]);
  async function save() {
    if (!view || revision.current === null) return;
    setValidationError("");
    try {
      const settings = editedLaunchOptions(view, draft);
      if (await mutate(`models/${encodeURIComponent(model.id)}/options`, { expected_revision: revision.current, options: settings }, `Saved launch profile for ${model.id}. Policy preserved; worker remains unloaded.`)) {
        setPendingSave(revision.current);
      }
    } catch (error) { setValidationError((error as Error).message); }
  }
  async function cancel() {
    const fresh = await options.reload();
    if (fresh) { accept(fresh); setPendingSave(null); setValidationError(""); }
  }
  return <section className="inspector-section" aria-label="Context and launch profile">
    <div className="section-heading"><h3>Context and launch</h3><Button disabled={options.loading || context.loading || busy || !online} onClick={() => { void options.reload(); void context.reload(); }}>Refresh context</Button></div>
    <p className="muted">Provider: {view?.provider ?? "Reading…"}. The engine provider is selected at registration, never inferred from a filename.</p>
    {options.error && <p role="alert" className="inline-error">{options.error}</p>}
    {context.error && <p role="alert" className="inline-error">{context.error}</p>}
    {view && <form aria-label="Launch profile" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <label htmlFor="profile-context">Total context budget in tokens</label><input id="profile-context" inputMode="numeric" value={draft.context} disabled={blocked} placeholder={imported ? "Registered model setting" : "Engine arguments or default"} onChange={(event) => setDraft({ ...draft, context: event.target.value })} />
      <p className="field-help">A total launch budget, not an advertised limit for each request. Juiced does not shrink it automatically.</p>
      {imported ? <p className="field-help">Imported settings own this context override. Empty removes the override and uses the registered model setting. Backend placement and physical slots stay with the imported profile.</p> : <><div className="profile-option-grid"><div><label htmlFor="profile-threads">Threads</label><input id="profile-threads" inputMode="numeric" value={draft.threads} disabled={blocked} placeholder="Engine default" onChange={(event) => setDraft({ ...draft, threads: event.target.value })} /></div><div><label htmlFor="profile-gpu">GPU layers</label><input id="profile-gpu" inputMode="numeric" value={draft.gpu} disabled={blocked} placeholder="Engine default" onChange={(event) => setDraft({ ...draft, gpu: event.target.value })} /></div></div>
      <p className="field-help">Empty fields keep the engine arguments or default. GPU layers 0 explicitly disables GPU layers.</p></>}
      <dl className="facts"><div><dt>Physical slots</dt><dd>{view.physical_slots} · fixed registration</dd></div><div><dt>Effective launch budget</dt><dd>{tokens(view.effective_launch?.context_tokens)}</dd></div></dl>
      {view.provider === "external-http" ? <p className="field-help">Typed launch changes and engine context observations require an explicit llama.cpp provider. Raw external engine settings remain unchanged.</p> : (model.loaded || model.state === "loading" || model.active_requests > 0) ? <p className="field-help">Unload this worker and finish active requests before editing its launch settings.</p> : <p className="field-help">Saving preserves the loading policy and leaves the worker unloaded. Changing physical slot count requires a new registration.</p>}
      {policyDirty && <p className="inline-warning">Save or cancel the policy draft before editing launch settings.</p>}
      {dirty && revision.current !== model.revision && <p className="inline-warning">Another client changed this model. Your launch draft is retained. Cancel it to read the latest settings.</p>}
      {validationError && <p role="alert" className="inline-error">{validationError}</p>}
      {pendingSave !== null && <p className="field-help">Settings were saved. Reading their committed revision before further edits. Refresh context if this read is unavailable.</p>}
      {view.provider === "llama-cpp" && <div className="action-row wrap"><Button disabled={!dirty || busy || !online || options.loading} onClick={() => void cancel()}>{imported ? "Cancel context draft" : "Cancel launch draft"}</Button><Button type="submit" variant="primary" disabled={!dirty || blocked}>{imported ? "Save context override" : "Save launch profile"}</Button></div>}
    </form>}
    <div className="context-observation" aria-label="Context observation">
      <h4>Context evidence</h4><p className="field-help">The last observation describes saved configuration and the engine at that moment. Refresh to read it again.</p>
      <dl className="facts"><div><dt>Training metadata</dt><dd>{tokens(observation?.declared?.training_context_tokens)}</dd></div><div><dt>Configured launch budget</dt><dd>{tokens(observation?.configured?.context_tokens)}</dd></div><div><dt>Applied spawn budget</dt><dd>{tokens(observation?.applied?.launch?.context_tokens)}</dd></div><div><dt>Observed request context</dt><dd>{tokens(observation ? servingContext(observation) : null)}</dd></div></dl>
      <p className="field-help">Training context comes from GGUF metadata. Applied values come from spawn arguments. Serving limits come only from engine observations, never by dividing the total budget.</p>
      {observation?.observed?.slots.map((slot) => <div className="slot-observation" key={slot.id}><strong>Slot {slot.id}</strong><span>{tokens(slot.context_tokens)} · {slot.is_processing === null ? "Processing unknown" : slot.is_processing ? "Processing" : "Idle"}</span></div>)}
      {!observation?.observed && <p className="field-help">{observation?.observation_error === "worker_unloaded" ? "Worker unloaded. Serving context is unknown." : observation?.observation_error === "provider_observation_unsupported" ? "This provider does not expose supported serving observations." : "Serving context observations are unavailable."}</p>}
      {observation && <Badge tone="warning">Engine compatibility {observation.qualification}</Badge>}
    </div>
  </section>;
}
