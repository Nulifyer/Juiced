import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import type { Model } from "./domain";
import type { Mutate } from "./Models";
import { launchDraft, editedLaunchOptions, parseModelContext, parseModelOptions, sameDraft, servingContext, type LaunchDraft, type ModelOptions } from "./profiles";
import { Button, HelpTip, Icon } from "./ui";
import { useResource } from "./useResource";

const emptyDraft: LaunchDraft = { context: "", threads: "", gpu: "" };
function tokens(value: number | null | undefined) { return value == null ? "Unknown" : `${value.toLocaleString()} tokens`; }

export function ContextInspector({ model, client, online, busy, policyDirty, onDirty, mutate }: { model: Model; client: Client; online: boolean; busy: boolean; policyDirty: boolean; onDirty: (dirty: boolean) => void; mutate: Mutate }) {
  const options = useResource(client, `models/${encodeURIComponent(model.id)}/options`, parseModelOptions, 0);
  const context = useResource(client, `juiced/context?model=${encodeURIComponent(model.id)}`, parseModelContext, model.loaded ? 5000 : 0);
  const [draft, setDraft] = useState(emptyDraft); const [saved, setSaved] = useState(emptyDraft);
  const [validationError, setValidationError] = useState("");
  const [pendingSave, setPendingSave] = useState<number | null>(null);
  const revision = useRef<number | null>(null);
  const last = useRef({ revision: model.revision, state: model.state });
  const dirty = !sameDraft(draft, saved);
  const view = options.data?.model === model.id ? options.data : null;
  const observation = context.data?.model === model.id ? context.data : null;
  const imported = view?.options_authority === "native-settings";
  const reload = model.loaded && !imported;
  const editable = view?.provider === "llama-cpp" && (!model.loaded || reload) && model.state !== "loading" && model.active_requests === 0;
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
      if (await mutate(`models/${encodeURIComponent(model.id)}/options`, { expected_revision: revision.current, options: settings, ...(reload ? { apply: "reload" } : {}) }, reload ? `Saved launch settings and restarted ${model.id}.` : `Saved launch settings for ${model.id}.`)) {
        setPendingSave(revision.current);
      }
    } catch (error) { setValidationError((error as Error).message); }
  }
  async function cancel() {
    const fresh = await options.reload();
    if (fresh) { accept(fresh); setPendingSave(null); setValidationError(""); }
  }
  return <section className="inspector-section" aria-label="Context and launch profile">
    <div className="section-heading"><div className="page-title"><h3>Launch settings</h3><HelpTip label="Launch settings">Engine: {view?.provider ?? "loading"}. Context is the total budget the engine starts with, shared by its slots. Empty fields keep the engine's own default. GPU layers 0 runs on CPU only. Saving keeps the loading policy. A loaded model restarts with the new settings.</HelpTip></div></div>
    {options.error && <p role="alert" className="inline-error">{options.error}</p>}
    {context.error && <p role="alert" className="inline-error">{context.error}</p>}
    {view && <form aria-label="Launch profile" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <label htmlFor="profile-context">Total context budget in tokens</label><input id="profile-context" inputMode="numeric" value={draft.context} disabled={blocked} placeholder={imported ? "Registered model setting" : "Engine arguments or default"} onChange={(event) => setDraft({ ...draft, context: event.target.value })} />
      {imported ? <p className="field-help">Imported model: empty uses its registered context.</p> : <><div className="profile-option-grid"><div><label htmlFor="profile-threads">Threads</label><input id="profile-threads" inputMode="numeric" value={draft.threads} disabled={blocked} placeholder="Engine default" onChange={(event) => setDraft({ ...draft, threads: event.target.value })} /></div><div><label htmlFor="profile-gpu">GPU layers</label><input id="profile-gpu" inputMode="numeric" value={draft.gpu} disabled={blocked} placeholder="Engine default" onChange={(event) => setDraft({ ...draft, gpu: event.target.value })} /></div></div>
</>}
      <dl className="facts"><div><dt>Slots</dt><dd>{view.physical_slots}</dd></div><div><dt>Effective context</dt><dd>{tokens(view.effective_launch?.context_tokens)}</dd></div></dl>
      {view.provider === "external-http" ? <p className="field-help">Only llama.cpp models have editable launch settings.</p>
        : model.active_requests > 0 || model.state === "loading" ? <p className="field-help">Wait for running requests and loading to finish to edit these.</p>
        : model.loaded && imported ? <p className="field-help">Unload the model to edit these.</p>
        : reload && dirty ? <p className="field-help">Saving restarts the model with these settings.</p> : null}
      {policyDirty && <p className="inline-warning">Save or cancel the policy change first.</p>}
      {dirty && revision.current !== model.revision && <p className="inline-warning">Another client changed this model. Your draft is kept; cancel it to see the latest.</p>}
      {validationError && <p role="alert" className="inline-error">{validationError}</p>}
      {pendingSave !== null && <p className="field-help">Saved. Reading the new settings…</p>}
      {view.provider === "llama-cpp" && <div className="action-row wrap"><Button disabled={!dirty || busy || !online || options.loading} onClick={() => void cancel()}>{imported ? "Cancel context draft" : "Cancel launch draft"}</Button><Button type="submit" variant="primary" disabled={!dirty || blocked}>{imported ? "Save context override" : reload ? "Save and reload" : "Save launch profile"}</Button></div>}
    </form>}
    <div className="context-observation" aria-label="Context observation">
      <div className="page-title"><h4>Effective context</h4><HelpTip label="Effective context">Trained comes from the model file. Saved is your setting. Launched is what the engine started with. Per request is what the running engine reports for each slot.</HelpTip></div>
      <dl className="facts"><div><dt>Trained</dt><dd>{tokens(observation?.declared?.training_context_tokens)}</dd></div><div><dt>Saved</dt><dd>{tokens(observation?.configured?.context_tokens)}</dd></div><div><dt>Launched</dt><dd>{tokens(observation?.applied?.launch?.context_tokens)}</dd></div><div><dt>Per request</dt><dd>{tokens(observation ? servingContext(observation) : null)}</dd></div></dl>
      {observation?.observed?.slots.map((slot) => <div className="slot-observation" key={slot.id}><strong>Slot {slot.id}</strong><span>{tokens(slot.context_tokens)} · {slot.is_processing === null ? "Processing unknown" : slot.is_processing ? "Processing" : "Idle"}</span></div>)}
      {!observation?.observed && <p className="field-help">{observation?.observation_error === "worker_unloaded" ? "Load the model to see per-request context." : "This engine doesn't report per-request context."}</p>}
    </div>
  </section>;
}
