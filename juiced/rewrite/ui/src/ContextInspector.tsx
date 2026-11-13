import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import { bytes, type Model } from "./domain";
import type { Mutate } from "./Models";
import { launchDraft, editedLaunchOptions, parseModelContext, parseModelOptions, sameDraft, servingContext, type LaunchDraft, type ModelOptions } from "./profiles";
import { asrFields, engineDraft, enginePatch, fallback, imageFields, sameTuned, splitArgs, tunedDraft, tunedFields, tunedOptions, type EngineField, type Field as SettingField, type TunedDraft } from "./modelSettings";
import { Button, HelpTip } from "./ui";
import { useResource } from "./useResource";

const emptyDraft: LaunchDraft = { context: "", threads: "", gpu: "" };
const emptyTuned = tunedDraft({}, []);
const tokens = (value: number | null | undefined) => value == null ? "—" : value.toLocaleString();

type Breakdown = { weights_bytes: number; context_bytes: number; compute_bytes: number; projector_bytes: number; draft_bytes: number; other_bytes: number };
export type Fit = { source: string; total_bytes: number; budget_bytes: number; available_bytes: number; fits: boolean; loaded: boolean; largest_context_that_fits: number | null; trained_context: number | null; breakdown: Breakdown; warnings: string[] };
type Field = { saved: unknown; launched: unknown; effective: unknown; source: string; restart_needed: boolean };
type Extras = { fields: Record<string, Field> | null; mtp: { acceptance_rate: number | null; accepted_per_draft: number | null } | null };

const number = (value: unknown) => typeof value === "number" && Number.isFinite(value) ? value : null;
const record = (value: unknown): Record<string, unknown> => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
export function parseFit(value: unknown): Fit {
  const body = record(value), parts = record(body.breakdown);
  if (number(body.total_bytes) === null || number(body.budget_bytes) === null) throw new Error("The service returned an unexpected memory estimate.");
  const part = (key: string) => number(parts[key]) ?? 0;
  return {
    source: typeof body.source === "string" ? body.source : "estimate", total_bytes: number(body.total_bytes)!, budget_bytes: number(body.budget_bytes)!, available_bytes: number(body.available_bytes) ?? 0,
    fits: body.fits === true, loaded: body.loaded === true, largest_context_that_fits: number(body.largest_context_that_fits), trained_context: number(body.trained_context),
    breakdown: { weights_bytes: part("weights_bytes"), context_bytes: part("context_bytes"), compute_bytes: part("compute_bytes"), projector_bytes: part("projector_bytes"), draft_bytes: part("draft_bytes"), other_bytes: part("other_bytes") },
    warnings: (Array.isArray(body.warnings) ? body.warnings : []).map((item) => typeof item === "string" ? item : typeof record(item).message === "string" ? record(item).message as string : "").filter(Boolean).slice(0, 8),
  };
}
function parseOptionsView(value: unknown) { return { view: parseModelOptions(value), extras: parseExtras(value) }; }
function parseExtras(value: unknown): Extras {
  const body = record(value), fields = record(body.fields), metrics = record(body.mtp_metrics);
  return {
    fields: body.fields ? Object.fromEntries(Object.entries(fields).map(([key, item]) => { const field = record(item); return [key, { saved: field.saved, launched: field.launched, effective: field.effective, source: typeof field.source === "string" ? field.source : "", restart_needed: field.restart_needed === true }]; })) : null,
    mtp: body.mtp_metrics ? { acceptance_rate: number(metrics.acceptance_rate), accepted_per_draft: number(metrics.accepted_per_draft) } : null,
  };
}

const sourceNames: Record<string, string> = { auto: "Auto", model: "Model", saved: "Saved", engine: "Engine", reported: "Engine" };
const fieldNames: [string, string][] = [["context", "Context"], ["gpu_layers", "GPU layers"], ["slots", "Slots"], ["threads", "Threads"], ["kv_cache_type", "KV cache"], ["flash_attention", "Flash attention"], ["mtp", "Multi-token prediction"], ["thinking", "Thinking"]];
const show = (value: unknown) => value === null || value === undefined || value === "" ? "—" : typeof value === "boolean" ? (value ? "On" : "Off") : typeof value === "number" ? value.toLocaleString() : String(value);

export function MemoryBar({ fit }: { fit: Fit }) {
  const parts: [string, number, string][] = [["Weights", fit.breakdown.weights_bytes + fit.breakdown.projector_bytes, "weights"], ["Context", fit.breakdown.context_bytes, "context"], ["Runtime", fit.breakdown.compute_bytes + fit.breakdown.draft_bytes + fit.breakdown.other_bytes, "runtime"]];
  const room = Math.max(fit.available_bytes, fit.total_bytes, 1);
  return <div className="memory-fit" role="group" aria-label="Memory estimate">
    <div className="memory-bar" role="img" aria-label={`${bytes(fit.total_bytes)} of ${bytes(fit.available_bytes)} available`}>{parts.filter(([, value]) => value > 0).map(([label, value, tone]) => <span key={label} className={`memory-part ${tone}`} style={{ width: `${Math.min(100, value / room * 100)}%` }} title={`${label} ${bytes(value)}`} />)}</div>
    <div className="memory-legend">
      {parts.filter(([, value]) => value > 0).map(([label, value, tone]) => <span key={label}><i className={`memory-dot ${tone}`} />{label} {bytes(value)}</span>)}
      <span className={`memory-total ${fit.fits ? "" : "over"}`}>{bytes(fit.total_bytes)} of {bytes(fit.available_bytes)} available{fit.fits ? "" : " · doesn't fit"}</span>
    </div>
  </div>;
}

export function ContextInspector({ model, client, online, busy, policyDirty, onDirty, mutate, advanced }: { model: Model; client: Client; online: boolean; busy: boolean; policyDirty: boolean; onDirty: (dirty: boolean) => void; mutate: Mutate; advanced: boolean }) {
  const loaded = useResource(client, `models/${encodeURIComponent(model.id)}/options`, parseOptionsView, model.loaded ? 10000 : 0);
  const options = { ...loaded, data: loaded.data?.view ?? null, reload: async () => (await loaded.reload())?.view ?? null };
  const extras = { data: loaded.data?.extras ?? null, reload: loaded.reload };
  const context = useResource(client, `juiced/context?model=${encodeURIComponent(model.id)}`, parseModelContext, model.loaded ? 5000 : 0);
  const [draft, setDraft] = useState(emptyDraft); const [saved, setSaved] = useState(emptyDraft);
  const [tuned, setTuned] = useState(emptyTuned); const [tunedSaved, setTunedSaved] = useState(emptyTuned);
  const [fit, setFit] = useState<Fit | null>(null);
  const [validationError, setValidationError] = useState("");
  const [pendingSave, setPendingSave] = useState<number | null>(null);
  const revision = useRef<number | null>(null);
  const last = useRef({ revision: model.revision, state: model.state });
  const dirty = !sameDraft(draft, saved) || !sameTuned(tuned, tunedSaved);
  const view = options.data?.model === model.id ? options.data : null;
  const observation = context.data?.model === model.id ? context.data : null;
  const imported = view?.options_authority === "native-settings";
  const reload = model.loaded && !imported;
  const llama = view?.provider === "llama-cpp";
  const editable = llama && (!model.loaded || reload) && model.state !== "loading" && model.active_requests === 0;
  const blocked = !editable || !online || busy || policyDirty || !!options.error || !view || pendingSave !== null;
  const accept = (fresh: ModelOptions) => { const value = launchDraft(fresh.saved); setDraft(value); setSaved(value); const values = tunedDraft(fresh.saved, fresh.extra_args ?? []); setTuned(values); setTunedSaved(values); revision.current = fresh.revision; };
  useEffect(() => { onDirty(dirty); }, [dirty, onDirty]);
  useEffect(() => {
    if (view && (!dirty || (pendingSave !== null && view.revision > pendingSave))) { accept(view); if (pendingSave !== null) setPendingSave(null); }
  }, [view, dirty, pendingSave]);
  useEffect(() => {
    if (last.current.revision !== model.revision) { void options.reload(); void extras.reload(); void context.reload(); }
    else if (last.current.state !== model.state) { void context.reload(); void extras.reload(); }
    last.current = { revision: model.revision, state: model.state };
  }, [model.revision, model.state, options.reload, extras.reload, context.reload]);
  useEffect(() => {
    const controller = new AbortController();
    const query = new URLSearchParams();
    if (/^\d+$/.test(draft.context.trim())) query.set("context_tokens", draft.context.trim());
    if (/^\d+$/.test(draft.gpu.trim())) query.set("gpu_layers", draft.gpu.trim());
    if (tuned.cache_type_k && tuned.cache_type_k === tuned.cache_type_v) query.set("kv_cache_type", tuned.cache_type_k);
    const timer = setTimeout(() => {
      client.request(`models/${encodeURIComponent(model.id)}/fit${query.size ? `?${query}` : ""}`, undefined, controller.signal).then((value) => setFit(parseFit(value))).catch(() => { if (!controller.signal.aborted) setFit(null); });
    }, 250);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [client, model.id, model.revision, model.loaded, draft.context, draft.gpu, tuned.cache_type_k, tuned.cache_type_v]);
  async function save() {
    if (!view || revision.current === null) return;
    setValidationError("");
    try {
      const settings = { ...editedLaunchOptions(view, draft), ...(imported ? {} : tunedOptions(tuned)) };
      const extras = !imported && tuned.extra !== tunedSaved.extra ? { extra_args: splitArgs(tuned.extra) } : {};
      if (await mutate(`models/${encodeURIComponent(model.id)}/options`, { expected_revision: revision.current, options: settings, ...extras, ...(reload ? { apply: "reload" } : {}) }, reload ? `Saved launch settings and restarted ${model.id}.` : `Saved launch settings for ${model.id}.`)) setPendingSave(revision.current);
    } catch (error) { setValidationError((error as Error).message); }
  }
  async function cancel() {
    const fresh = await options.reload();
    if (fresh) { accept(fresh); setPendingSave(null); setValidationError(""); }
  }
  const trained = fit?.trained_context ?? observation?.declared?.training_context_tokens ?? null;
  const running = observation?.applied?.launch?.context_tokens ?? null;
  const fields = extras.data?.fields ?? null;
  const mtp = fields?.mtp?.effective === true;
  const gpuAll = draft.gpu.trim() === "" || Number(draft.gpu) >= 99;
  const restart = fields ? Object.values(fields).some((field) => field.restart_needed) : false;
  return <section className="inspector-section launch-settings" aria-label="Context and launch profile">
    <div className="section-heading"><div className="page-title"><h3>Launch settings</h3><HelpTip label="Launch settings">Context is the total token budget the engine starts with, shared by its slots. Auto picks the largest context that fits the memory budget; Max uses the size the model was trained for. A loaded model restarts when you save.</HelpTip></div></div>
    {options.error && <p role="alert" className="inline-error">{options.error}</p>}
    {fit && <MemoryBar fit={fit} />}
    {llama && fit?.warnings.map((warning) => <p className="field-help" key={warning}>{warning}</p>)}
    {view && !llama && (view.provider === "stable-diffusion-cpp" || view.provider === "asr4all") && <EngineDefaults view={view} model={model} online={online} busy={busy} blockedByOther={policyDirty} onDirty={onDirty} mutate={mutate} />}
    {view && !llama && view.provider !== "stable-diffusion-cpp" && view.provider !== "asr4all" && <p className="field-help">{model.operations.includes("audio/transcriptions") ? "Speech models use one slot. Text context settings don't apply." : "This engine has no adjustable launch settings."}</p>}
    {view && llama && <form aria-label="Launch profile" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <label htmlFor="profile-context">Context</label>
      <div className="context-row">
        <input id="profile-context" inputMode="numeric" value={draft.context} disabled={blocked} placeholder={imported ? "Registered setting" : fit?.largest_context_that_fits ? `Auto · ${tokens(fit.largest_context_that_fits)}` : "Engine default"} onChange={(event) => setDraft({ ...draft, context: event.target.value })} />
        {fit?.largest_context_that_fits && <Button disabled={blocked} title="Largest context that fits the memory budget" onClick={() => setDraft({ ...draft, context: String(fit.largest_context_that_fits) })}>Auto</Button>}
        {trained && <Button disabled={blocked} title="The context the model was trained for" onClick={() => setDraft({ ...draft, context: String(trained) })}>Max</Button>}
      </div>
      <p className="field-help">{[trained ? `Trained ${tokens(trained)}` : null, running ? `running ${tokens(running)}` : null, view.physical_slots > 1 ? `${view.physical_slots} slots share it` : null].filter(Boolean).join(" · ") || "Empty uses the engine default."}</p>
      {llama && !imported && <div className="setting-line"><span className="field-label" id="gpu-label">Run on</span>
        <div className="segmented" role="radiogroup" aria-labelledby="gpu-label">{([["gpu", "GPU"], ["cpu", "CPU only"]] as const).map(([value, label]) => <button key={value} type="button" role="radio" aria-checked={value === "gpu" ? gpuAll || (Number(draft.gpu) > 0 && Number(draft.gpu) < 99) : draft.gpu.trim() === "0"} disabled={blocked} onClick={() => setDraft({ ...draft, gpu: value === "gpu" ? "" : "0" })}>{label}</button>)}</div></div>}
      {fields?.mtp && <p className="setting-status"><strong>Multi-token prediction</strong> {mtp ? "on" : "off"}{mtp && extras.data?.mtp?.acceptance_rate != null ? ` · ${Math.round(extras.data.mtp.acceptance_rate * 100)}% of drafts accepted` : ""}</p>}
      {fields?.thinking && fields.thinking.effective !== null && fields.thinking.effective !== undefined && <p className="setting-status"><strong>Thinking</strong> {show(fields.thinking.effective)}</p>}
      {llama && !imported && <TunedGrid title="Generation defaults" help="Used when a request doesn't set its own value. Empty keeps the profile's argument or llama.cpp's default." fields={tunedFields.filter((field) => !field.advanced)} draft={tuned} onChange={setTuned} view={view} disabled={blocked} />}
      {advanced && !imported && <div className="setting-group"><div className="group-title"><span>Runtime</span><HelpTip label="Runtime">CPU threads, offload and batching. Changes apply when the engine next starts.</HelpTip></div><div className="profile-option-grid"><div><label htmlFor="profile-threads">Threads</label><input id="profile-threads" inputMode="numeric" value={draft.threads} disabled={blocked} placeholder="Engine default" onChange={(event) => setDraft({ ...draft, threads: event.target.value })} /></div><div><label htmlFor="profile-gpu">GPU layers</label><input id="profile-gpu" inputMode="numeric" value={draft.gpu} disabled={blocked} placeholder="All" onChange={(event) => setDraft({ ...draft, gpu: event.target.value })} /></div>
        {tunedFields.filter((field) => field.advanced).map((field) => <TunedInput key={field.key} field={field} draft={tuned} onChange={setTuned} view={view} disabled={blocked} />)}</div></div>}
      {advanced && !imported && llama && <div className="setting-group"><div className="field-head"><label htmlFor="profile-extra">Extra arguments</label><HelpTip label="Extra arguments">Passed to llama-server after Juiced's own arguments, for example --spec-type draft-mtp. Quote values that contain spaces. Settings above have their own fields; the host, port, model and cache paths stay with Juiced.</HelpTip></div>
        <textarea id="profile-extra" className="code-input" rows={Math.min(6, Math.max(2, Math.ceil(tuned.extra.length / 36) + 1))} spellCheck={false} value={tuned.extra} disabled={blocked} placeholder="None" onChange={(event) => setTuned({ ...tuned, extra: event.target.value })} /></div>}
      {!llama ? null
        : model.active_requests > 0 || model.state === "loading" ? <p className="field-help">Wait for running requests and loading to finish to edit these.</p>
        : model.loaded && imported ? <p className="field-help">Unload the model to edit these.</p> : null}
      {restart && !dirty && <p className="inline-warning">Saved settings differ from the running engine. Reload the model to apply them.</p>}
      {policyDirty && <p className="inline-warning">Save or cancel the other change first.</p>}
      {dirty && revision.current !== model.revision && <p className="inline-warning">Another client changed this model. Your draft is kept; cancel it to see the latest.</p>}
      {validationError && <p role="alert" className="inline-error">{validationError}</p>}
      {pendingSave !== null && <p className="field-help">Saved. Reading the new settings…</p>}
      {llama && dirty && <div className="action-row wrap"><Button disabled={busy || !online || options.loading} onClick={() => void cancel()}>Cancel</Button><Button type="submit" variant="primary" disabled={blocked || (fit !== null && !fit.fits)}>{reload ? "Save and reload" : "Save"}</Button></div>}
    </form>}
    {advanced && fields && <div className="data-table provenance" role="table" aria-label="Setting sources">
      <div className="data-head" role="row"><span role="columnheader">Setting</span><span role="columnheader">Saved</span><span role="columnheader">Running</span><span role="columnheader">From</span></div>
      {fieldNames.filter(([key]) => fields[key]).map(([key, label]) => { const field = fields[key]!; return <div className="data-row" role="row" key={key}><span role="cell">{label}</span><span role="cell">{show(field.saved)}</span><span role="cell">{show(field.launched ?? field.effective)}{field.restart_needed && <span className="muted"> · restart</span>}</span><span role="cell" className="muted">{sourceNames[field.source] ?? field.source}</span></div>; })}
    </div>}
    {advanced && observation?.observed && <div className="slot-lines">{observation.observed.slots.map((slot) => <span key={slot.id} className="muted">Slot {slot.id}: {tokens(slot.context_tokens)} · {slot.is_processing ? "busy" : "idle"}</span>)}{servingContext(observation) && <span className="muted">Per request {tokens(servingContext(observation))}</span>}</div>}
  </section>;
}

function TunedInput({ field, draft, onChange, view, disabled }: { field: SettingField; draft: TunedDraft; onChange: (draft: TunedDraft) => void; view: ModelOptions; disabled: boolean }) {
  const id = `profile-${field.key}`;
  const empty = fallback(field.key, view.effective_launch, view.defaults);
  return <div>
    <div className="field-head"><label htmlFor={id}>{field.label}</label><HelpTip label={field.label}>{field.help}</HelpTip></div>
    {field.kind === "choice"
      ? <select id={id} value={draft[field.key]} disabled={disabled} onChange={(event) => onChange({ ...draft, [field.key]: event.target.value })}><option value="">{empty}</option>{field.choices!.map((choice) => <option key={choice} value={choice}>{choice}</option>)}</select>
      : <input id={id} inputMode={field.kind === "decimal" ? "decimal" : "numeric"} value={draft[field.key]} disabled={disabled} placeholder={empty} onChange={(event) => onChange({ ...draft, [field.key]: event.target.value })} />}
  </div>;
}

function TunedGrid({ title, help, fields, draft, onChange, view, disabled }: { title: string; help: string; fields: SettingField[]; draft: TunedDraft; onChange: (draft: TunedDraft) => void; view: ModelOptions; disabled: boolean }) {
  return <div className="setting-group"><div className="group-title"><span>{title}</span><HelpTip label={title}>{help}</HelpTip></div>
    <div className="profile-option-grid">{fields.map((field) => <TunedInput key={field.key} field={field} draft={draft} onChange={onChange} view={view} disabled={disabled} />)}</div></div>;
}

function EngineDefaults({ view, model, online, busy, blockedByOther, onDirty, mutate }: { view: ModelOptions; model: Model; online: boolean; busy: boolean; blockedByOther: boolean; onDirty: (dirty: boolean) => void; mutate: Mutate }) {
  const image = view.provider === "stable-diffusion-cpp";
  const fields: EngineField[] = image ? imageFields : asrFields;
  const values = image ? view.image_defaults : view.asr_options;
  const saved = engineDraft(fields, values);
  const [draft, setDraft] = useState(saved);
  const [error, setError] = useState("");
  const base = useRef({ revision: view.revision, saved });
  if (base.current.revision !== view.revision) base.current = { revision: view.revision, saved };
  const dirty = fields.some((field) => (draft[field.key] ?? "") !== (base.current.saved[field.key] ?? ""));
  useEffect(() => { onDirty(dirty); }, [dirty, onDirty]);
  useEffect(() => { if (!dirty) setDraft(base.current.saved); }, [view.revision]);
  const reload = model.loaded;
  const blocked = !online || busy || blockedByOther || model.state === "loading" || model.active_requests > 0;
  const title = image ? "Image defaults" : "Speech options";
  async function save() {
    setError("");
    try {
      const patch = enginePatch(fields, base.current.saved, draft);
      if (await mutate(`models/${encodeURIComponent(model.id)}/options`, { expected_revision: view.revision, [image ? "image_defaults" : "asr_options"]: patch, ...(reload ? { apply: "reload" } : {}) }, reload ? `Saved ${title.toLowerCase()} and restarted ${model.id}.` : `Saved ${title.toLowerCase()} for ${model.id}.`)) base.current = { revision: -1, saved: draft };
    } catch (failure) { setError((failure as Error).message); }
  }
  return <form className="engine-defaults" aria-label={title} onSubmit={(event) => { event.preventDefault(); void save(); }}>
    <div className="group-title"><span>{title}</span><HelpTip label={title}>{image ? "The engine starts with these and uses them for every image request that doesn't set its own. Empty uses the engine default." : "Passed to the transcription engine when it starts. Empty uses the engine default."}</HelpTip></div>
    <div className="profile-option-grid">{fields.map((field) => <div key={field.key}>
      <div className="field-head"><label htmlFor={`engine-${field.key}`}>{field.label}</label><HelpTip label={field.label}>{field.help}</HelpTip></div>
      <input id={`engine-${field.key}`} inputMode={field.kind === "name" ? "text" : field.kind === "decimal" ? "decimal" : "numeric"} value={draft[field.key] ?? ""} disabled={blocked} placeholder="Engine default" onChange={(event) => setDraft({ ...draft, [field.key]: event.target.value })} />
    </div>)}</div>
    {error && <p role="alert" className="inline-error">{error}</p>}
    {dirty && <div className="action-row wrap"><Button disabled={busy} onClick={() => { setDraft(base.current.saved); setError(""); }}>Cancel</Button><Button type="submit" variant="primary" disabled={blocked}>{reload ? "Save and reload" : "Save"}</Button></div>}
  </form>;
}
