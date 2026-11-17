import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { matchingModels } from "./modelPicker";
import { bytes, bytesPair, hotFit, hotFitNote, hotOption, loadReason, offers, policyNames, services, stateLabel, taskNames, unloadReason, workerTone, type Model, type Policy, type Resources, type Service } from "./domain";
import { Badge, Button, Dialog, Empty, HelpTip, Icon, Switch } from "./ui";
import { Client } from "./api";
import { removalReason } from "./management";
import { ContextInspector } from "./ContextInspector";
import { QuantSync } from "./QuantSync";
import { ArtifactRevisionInspector } from "./ArtifactRevisionInspector";
import { RuntimeInspector } from "./RuntimeInspector";
import { ModelCheck } from "./ModelCheck";
import { ModelUpdate } from "./ModelUpdate";
import { ModelIdle, residencyNote } from "./Residency";
import { ModelFiles, localQuantState, removalNote, unusedQuants, weightsGroup } from "./LocalModels";
import { ModelQuants } from "./ModelQuants";
import type { DiscoverTarget } from "./FindModels";
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
  if (code === "missing_weights") return "The model file is missing on the server.";
  if (code === "worker_exited") return "The engine exited. Review its launch configuration, then retry loading.";
  return "The worker needs attention. Review its current state before retrying.";
}

type ListProps = { models: Model[]; selected?: string; onSelect: (id: string) => void; mode: "models" | "policy"; mutate: Mutate; busy: boolean; online: boolean; idleControl?: ReactNode; budgetControl?: ReactNode; files?: LocalGroup[]; inventory?: LocalInventory | null; selectedFile?: string; onSelectFile?: (id: string) => void; onDiscover?: () => void; client?: Client; resources?: Resources };
type SortKey = "name" | "state" | "memory";
const placeholderClient = new Client("", "");

export function parseUpdates(value: unknown): string[] {
  const data = value && typeof value === "object" ? (value as { data?: unknown }).data : undefined;
  return (Array.isArray(data) ? data : []).filter((item) => item && typeof item === "object" && item.update_available === true && typeof item.id === "string").map((item) => item.id as string);
}

/** Without a chosen column, loaded models come first, then loading, enabled and disabled ones. */
export function sortModels(models: Model[], chosen: { key: SortKey; descending: boolean } | null): Model[] {
  const sort = chosen ?? { key: "state" as const, descending: false };
  const rank = (model: Model) => sort.key === "memory" ? model.resident_bytes ?? -1 : sort.key === "state" ? (model.loaded ? 0 : model.state === "loading" ? 1 : model.policy === "disabled" ? 3 : 2) : 0;
  const sorted = [...models].sort((left, right) => (sort.key === "name" ? left.id.localeCompare(right.id) : rank(left) - rank(right)) || left.id.localeCompare(right.id));
  return sort.descending ? sorted.reverse() : sorted;
}

/** Splits a name after - _ . or / into pieces of at least 8 characters, so it wraps between pieces. */
export function namePieces(name: string): string[] {
  const pieces: string[] = [];
  for (const fragment of name.match(/[^-_./]*[-_./]*/g)?.filter(Boolean) ?? []) {
    const last = pieces.length - 1;
    if (last >= 0 && (pieces[last]!.length < 8 || fragment.length < 4)) pieces[last] += fragment; else pieces.push(fragment);
  }
  return pieces.length ? pieces : [name];
}

export function ModelName({ name }: { name: string }) {
  // Inline-block pieces read as separate words to assistive tech, so they get one unbroken name instead.
  return <><span className="sr-only name-sr">{name}</span><span className="name-pieces" aria-hidden="true">{namePieces(name).map((piece, index) => <span key={index} className="name-piece">{piece}</span>)}</span></>;
}

export type LoadProgress = { elapsed: number; left: number | null; share: number | null };

/** Elapsed load time, and the time left when the same model and engine loaded before. */
export function loadProgress(model: Model, now: number): LoadProgress | null {
  if (model.state !== "loading" || model.startup_waiting || typeof model.loading_started_unix_ms !== "number") return null;
  const elapsed = Math.max(0, now - model.loading_started_unix_ms);
  const last = typeof model.last_load_ms === "number" && model.last_load_ms > 0 ? model.last_load_ms : null;
  return { elapsed, left: last !== null && last > elapsed ? last - elapsed : null, share: last === null ? null : Math.min(1, elapsed / last) };
}

const seconds = (ms: number) => ms < 60_000 ? `${Math.max(1, Math.round(ms / 1000))} s` : `${Math.round(ms / 60_000)} min`;
export const loadProgressText = (progress: LoadProgress) => [seconds(progress.elapsed), progress.left !== null ? `about ${seconds(progress.left)} left` : null].filter(Boolean).join(" · ");

function useNow(active: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [active]);
  return now;
}

function LoadingNote({ model, bar = false }: { model: Model; bar?: boolean }) {
  const now = useNow(model.state === "loading");
  const progress = loadProgress(model, now);
  if (!progress) return null;
  return <>{bar && progress.share !== null && <progress className="load-progress" max={1} value={progress.share} aria-label={`Loading ${model.id}`} />}<span className="muted load-time"> · {loadProgressText(progress)}</span></>;
}

function openPopover() { try { return !!document.querySelector(":popover-open"); } catch { return false; } }

function focusRow(id: string) {
  requestAnimationFrame(() => {
    if (document.querySelector(".inspector")) return;
    document.querySelector<HTMLElement>(`.model-name[data-model="${CSS.escape(id)}"], .lane-open[data-model="${CSS.escape(id)}"]`)?.focus();
  });
}

function BulkBar({ models, chosen, mutate, disabled, onClear, onKeep }: { models: Model[]; chosen: Set<string>; mutate: Mutate; disabled: boolean; onClear: () => void; onKeep: (ids: string[]) => void }) {
  const picked = models.filter((model) => chosen.has(model.id));
  const loadable = picked.filter((model) => !model.loaded && !loadReason(model));
  const unloadable = picked.filter((model) => model.loaded && !unloadReason(model));
  const run = async (targets: Model[], action: (model: Model) => Promise<boolean>) => { for (const model of targets) await action(model); onClear(); };
  return <div className="bulk-bar" role="toolbar" aria-label="Selected models">
    <strong>{picked.length} selected</strong>
    <Button disabled={disabled || loadable.length === 0} onClick={() => void run(loadable, (model) => mutate("load", { model: model.id }, `Loaded ${model.id}.`))}>Load{loadable.length && loadable.length !== picked.length ? ` ${loadable.length}` : ""}</Button>
    <Button disabled={disabled || unloadable.length === 0} onClick={() => void run(unloadable, (model) => mutate("unload", { model: model.id }, `Unloaded ${model.id}.`))}>Unload{unloadable.length && unloadable.length !== picked.length ? ` ${unloadable.length}` : ""}</Button>
    <select aria-label="Set loading policy for selected models" value="" disabled={disabled} onChange={(event) => { const policy = event.target.value as Policy; if (policy) onKeep(picked.map((model) => model.id)); if (policy) void run(picked.filter((model) => model.policy !== policy && !(policy === "disabled" && model.active_requests > 0)), (model) => mutate("juiced/policy", { model: model.id, policy, expected_revision: model.revision }, `${model.id} is now ${policyNames[policy]}.`)); }}>
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

async function currentModel(client: Client, id: string): Promise<Model | undefined> {
  const catalog = await client.request<{ data?: Model[] }>("juiced/catalog");
  return catalog.data?.find((entry) => entry.id === id);
}

/** Disabling a loaded model unloads it, so the toast offers to put the old policy back. */
function setPolicy(mutate: Mutate, model: Model, policy: Policy, client?: Client) {
  if (policy === model.policy) return;
  const unloads = policy === "disabled" && model.loaded;
  if (!unloads || !client) { void mutate("juiced/policy", { model: model.id, policy, expected_revision: model.revision }, unloads ? `Disabled and unloaded ${model.id}.` : `${model.id} is now ${policyNames[policy]}.`); return; }
  const previous = model.policy;
  void mutate("juiced/policy", { model: model.id, policy, expected_revision: model.revision }, "").then(async (done) => {
    if (!done) return;
    const after = await currentModel(client, model.id).catch(() => undefined);
    const text = `Disabled and unloaded ${model.id}.`;
    if (!after || after.policy !== "disabled") { notify(text); return; }
    notify(text, "success", { label: "Undo", run: () => void mutate("juiced/policy", { model: model.id, policy: previous, expected_revision: after.revision }, `${model.id} is ${policyNames[previous]} again.`) });
  });
}

function PolicySelect({ model, models, resources, mutate, disabled, client }: { model: Model; models: Model[]; resources?: Resources; mutate: Mutate; disabled: boolean; client?: Client }) {
  return <select className="policy-select" aria-label={`Loading policy for ${model.id}`} data-policy={model.policy} value={model.policy} disabled={disabled} onClick={(event) => event.stopPropagation()} onChange={(event) => setPolicy(mutate, model, event.target.value as Policy, client)}>
    <option value="hot">{hotOption(model, hotFit(model, models, resources))}</option><option value="on-demand">On demand</option><option value="disabled" disabled={model.active_requests > 0}>Disabled</option>
  </select>;
}

function RowAction({ model, mutate, disabled }: { model: Model; mutate: Mutate; disabled: boolean }) {
  const unload = model.loaded || model.state === "cleanup-pending";
  if (!unload && model.policy === "disabled") return null;
  const reason = unload ? unloadReason(model) : loadReason(model);
  return <Button className="row-action" disabled={disabled || !!reason} title={reason ?? undefined} onClick={(event) => { event.stopPropagation(); void mutate(unload ? "unload" : "load", { model: model.id }, unload ? `Unloaded ${model.id}.` : `Loaded ${model.id}.`); }}>{unload ? "Unload" : "Load"}</Button>;
}

function fileMatches(group: LocalGroup, query: string) {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const text = `${group.name} ${group.publisher ?? ""} ${group.quants.map((quant) => quant.label).join(" ")}`.toLowerCase();
  return terms.every((term) => text.includes(term));
}

export const notAddedHelp = "Model files on disk that no model uses.";

const genericLabels = new Set(["gguf", "bin", "onnx", "ort", "safetensors", "file"]);
/** The quant a model runs, when its name doesn't already say so. */
export function quantLabel(model: Pick<Model, "id">, inventory: LocalInventory | null | undefined): string | null {
  const label = weightsGroup(model, inventory ?? null)?.quants.find((quant) => quant.usedBy.includes(model.id))?.label;
  return !label || genericLabels.has(label.toLowerCase()) || model.id.toLowerCase().includes(label.toLowerCase()) ? null : label;
}

function FileRow({ group, models, selected, listed, onSelect }: { group: LocalGroup; models: Model[]; selected: boolean; listed: boolean; onSelect: () => void }) {
  const unused = unusedQuants(group);
  const size = unused.length ? unused.reduce((sum, quant) => sum + quant.bytes, 0) : group.bytes;
  const partial = unused.length > 0 && unused.every((quant) => !quant.add && quant.parts > 1 && !quant.complete);
  const quants = unused.map((quant) => quant.parts > 1 && !quant.complete ? `${quant.label} (${localQuantState(quant, models)})` : quant.label).join(", ");
  return <div className={`data-row model-row file-row ${selected ? "selected" : ""}`} role="row" onClick={onSelect}>
    <div role="cell" className="model-identity picked"><span className="pick pick-space" aria-hidden="true" /><div className="identity-text"><button className="model-name" aria-pressed={selected} onClick={(event) => { event.stopPropagation(); onSelect(); }}><ModelName name={group.name} /></button><span className="model-meta">{[group.publisher, quants || null, bytes(size), group.readOnly ? "read-only" : null].filter(Boolean).join(" · ")}</span></div></div>
    <div role="cell" className="model-state">{!listed && <span title={notAddedHelp}><Badge>Not added</Badge></span>}</div>
    <div role="cell" />
    <div role="cell" />
    <div role="cell" className="row-end"><Button className="row-action" aria-label={`${partial ? "Finish" : "Add"} ${group.name}`} onClick={(event) => { event.stopPropagation(); onSelect(); }}>{partial ? "Finish" : "Add"}</Button></div>
  </div>;
}

export function ModelList({ models, selected, onSelect, mode, mutate, busy, online, idleControl, budgetControl, files = [], inventory, selectedFile, onSelectFile, onDiscover, client, resources }: ListProps) {
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
  const [service, setService] = useState<Service | null>(null);
  const [dragging, setDragging] = useState("");
  const dragged = useRef("");
  const [scope, setScopeState] = useState<"auto" | "enabled" | "disabled" | "files" | null>("auto");
  // A row whose policy the operator just changed stays put, so it doesn't vanish under the filter.
  const [kept, setKept] = useState<ReadonlySet<string>>(() => new Set());
  const setScope = (next: "enabled" | "disabled" | "files" | null) => { setScopeState(next); setKept(new Set()); if (next === "files") setService(null); };
  const keep = (id: string) => setKept((current) => current.has(id) ? current : new Set([...current, id]));
  const served = service ? models.filter((model) => offers(model, service)) : models;
  const enabledCount = served.filter((model) => model.policy !== "disabled").length, disabledCount = served.length - enabledCount;
  // On the board a card dropped on Disabled would vanish under an Enabled filter, so the lanes show every model.
  const scoped = mode !== "models" ? null : scope === "auto" ? (enabledCount > 0 && disabledCount > 0 ? "enabled" : models.length === 0 && files.length > 0 ? "files" : null) : scope === "files" && files.length === 0 ? null : scope;
  const matches = scoped === "files" ? [] : matchingModels(served, query);
  const searching = query.trim() !== "";
  const revealed = scoped === "enabled" && searching ? matches.filter((model) => model.policy === "disabled").length : 0;
  const visible = sortModels(scoped === null || (scoped === "enabled" && searching) ? matches : matches.filter((model) => kept.has(model.id) || (model.policy === "disabled") === (scoped === "disabled")), sort);
  const scopeChips = mode === "models" && [enabledCount, disabledCount, files.length].filter((count) => count > 0).length > 1 ? <div className="chip-filter scope-filter" role="group" aria-label="Status filter">
    {enabledCount > 0 && <button aria-pressed={scoped === "enabled"} onClick={() => setScope(scoped === "enabled" ? null : "enabled")}>Enabled <span>{enabledCount}</span></button>}
    {disabledCount > 0 && <button aria-pressed={scoped === "disabled"} onClick={() => setScope(scoped === "disabled" ? null : "disabled")}>Disabled <span>{disabledCount}</span></button>}
    {files.length > 0 && <button aria-pressed={scoped === "files"} title={notAddedHelp} onClick={() => setScope(scoped === "files" ? null : "files")}>Not added <span>{files.length}</span></button>}
    {files.length > 0 && <HelpTip label="Not added">{notAddedHelp} Add one to run it, or delete it to free space.</HelpTip>}
    {revealed > 0 && <span className="muted scope-note">{revealed} disabled match</span>}
  </div> : null;
  const onSort = (key: SortKey) => setSort((current) => current?.key === key ? (current.descending ? null : { key, descending: true }) : { key, descending: false });
  const allChosen = visible.length > 0 && visible.every((model) => chosen.has(model.id));
  const toggle = (id: string) => setChosen((current) => { const next = new Set(current); if (next.has(id)) next.delete(id); else next.add(id); return next; });
  const visibleFiles = service === null && (scoped === null || scoped === "files" || searching) ? files.filter((group) => fileMatches(group, query)) : [];
  const chips = services.map(([id, label]) => [id, label, models.filter((model) => offers(model, id)).length] as const).filter(([, , count]) => count > 0);
  const filtered = service !== null && !chips.some(([id]) => id === service);
  useEffect(() => { if (filtered) setService(null); }, [filtered]);
  const quants = useMemo(() => new Map(mode === "models" && inventory ? models.map((model) => [model.id, quantLabel(model, inventory)] as const) : []), [mode, models, inventory]);
  const locked = busy || !online;
  const budget = resources?.resident_budget_bytes;
  const draggedModel = models.find((model) => model.id === dragging);
  const dragFit = draggedModel && draggedModel.policy !== "hot" ? hotFit(draggedModel, models, resources) : null;
  const updateAction = client && mode === "models" && models.length > 0 ? <span className="update-action"><Button variant="ghost" disabled={!online || checking} onClick={() => void checkUpdates()}>{checking ? "Checking…" : "Check for updates"}</Button><HelpTip label="Check for updates">Compares each Hugging Face model with its upstream files. Nothing is downloaded.</HelpTip></span> : null;
  const disk = inventory && <span className="muted library-total">{bytes(inventory.totalBytes)} on disk{inventory.removableBytes > 0 && files.length > 0 && <> · <button className="link-button" onClick={() => setScope("files")}>{bytes(inventory.removableBytes)} not added</button></>}{inventory.truncated && <HelpTip label="disk use">Large folders are counted up to a limit.</HelpTip>}</span>;
  const filesOnly = scoped === "files";
  return <section className="inventory" aria-label="Model inventory">
    <div className="toolbar models-toolbar">
      <input type="search" aria-label="Search models" placeholder="Search models" value={query} onChange={(event) => { setQuery(event.target.value); setKept(new Set()); }} />
      {(disk || updateAction) && <div className="toolbar-side">{disk}{updateAction}</div>}
      {(disk || updateAction) && (chips.length > 1 || scopeChips) && <span className="toolbar-break" />}
      {chips.length > 1 && <div className="chip-filter" role="group" aria-label="Service filter"><button aria-pressed={service === null && !filesOnly} onClick={() => { setService(null); if (filesOnly) setScope(null); }}>All <span>{models.length}</span></button>{chips.map(([id, label, count]) => <button key={id} aria-pressed={service === id} onClick={() => { setService(service === id ? null : id); if (filesOnly) setScope(null); }}>{label} <span>{count}</span></button>)}</div>}
      {scopeChips}
      {budgetControl}
    </div>
    {mode === "models" && chosen.size > 0 && <BulkBar models={models} chosen={chosen} mutate={mutate} disabled={locked} onClear={() => setChosen(new Set())} onKeep={(ids) => setKept((current) => new Set([...current, ...ids]))} />}
    {models.length === 0 && files.length === 0 ? <Empty title="No models yet">{onDiscover ? <><button className="link-button" onClick={onDiscover}>Find a model</button> to get started.</> : "Find a model in Discover to get started."}</Empty> : visible.length === 0 && visibleFiles.length === 0 ? <Empty title="No matches">Try another search or filter.</Empty> : mode === "policy" ? <div className="policy-board">
      {(["hot", "on-demand", "disabled"] as Policy[]).map((policy) => <section className={`policy-lane ${dragging ? "droppable" : ""}`} key={policy} aria-label={`${policyNames[policy]} models`}
        onDragOver={(event) => { if (dragged.current) event.preventDefault(); }}
        onDrop={(event) => { event.preventDefault(); const model = models.find((item) => item.id === dragged.current); dragged.current = ""; setDragging(""); if (model && !(policy === "disabled" && model.active_requests > 0)) setPolicy(mutate, model, policy, client); }}>
        <header><h2>{policyNames[policy]}</h2><span className="muted">{matches.filter((model) => model.policy === policy).length}{policy === "hot" ? ` · ${budget ? `${bytesPair(resources!.reserved_bytes, budget, " of ")} used` : `${bytes(models.filter((model) => model.policy === "hot").reduce((sum, model) => sum + (model.resident_bytes ?? 0), 0))} held`}` : ""}</span></header>
        <p className={policy === "hot" && dragFit && dragFit.fit !== "fits" ? "tone-warning" : "muted"}>{policy === "hot" ? (dragFit ? hotFitNote(draggedModel!, dragFit) : "Loaded at startup and kept ready") : policy === "on-demand" ? "Loads on the first request" : "Hidden from apps"}</p>
        {policy === "on-demand" && idleControl}
        {visible.filter((model) => model.policy === policy).map((model) => <div key={model.id} className={`lane-model ${selected === model.id ? "selected" : ""}`} draggable={!locked} onDragStart={(event) => { dragged.current = model.id; setDragging(model.id); event.dataTransfer.effectAllowed = "move"; }} onDragEnd={() => { dragged.current = ""; setDragging(""); }}>
          <button className="lane-open" data-model={model.id} aria-pressed={selected === model.id} title={model.id} onClick={() => onSelect(model.id)}><strong><ModelName name={model.id} /></strong><span className="muted"><span className={`status-dot ${workerTone(model)}`} /> {stateLabel(model)}<LoadingNote model={model} /> · {bytes(model.resident_bytes)}</span></button>
          <PolicySelect model={model} models={models} resources={resources} mutate={mutate} disabled={locked} client={client} />
        </div>)}
      </section>)}
    </div> : <div className="data-table model-table" role="table" aria-label="Registered models">
      <div className="data-head" role="row"><SortHeader label={filesOnly ? "Files no model uses" : "Model"} id="name" sort={sort} onSort={onSort} pick={filesOnly ? <span className="pick pick-space" aria-hidden="true" /> : <label className="pick-hit"><input type="checkbox" className="pick" aria-label="Select all shown models" checked={allChosen} onChange={() => setChosen(allChosen ? new Set() : new Set(visible.map((model) => model.id)))} /></label>} />{filesOnly ? <><span role="columnheader" /><span role="columnheader" /><span role="columnheader" /></> : <><SortHeader label="State" id="state" sort={sort} onSort={onSort} /><span role="columnheader">Policy</span><SortHeader label="Memory" id="memory" sort={sort} onSort={onSort} numeric /></>}<span role="columnheader"><span className="sr-only">Action</span></span></div>
      {visible.map((model) => <div className={`data-row model-row ${selected === model.id ? "selected" : ""} ${chosen.has(model.id) ? "chosen" : ""}`} role="row" key={model.id} onClick={() => onSelect(model.id)}>
        <div role="cell" className="model-identity picked"><label className="pick-hit" onClick={(event) => event.stopPropagation()}><input type="checkbox" className="pick" aria-label={`Select ${model.id}`} checked={chosen.has(model.id)} onChange={() => toggle(model.id)} /></label><div className="identity-text"><button className="model-name" data-model={model.id} aria-pressed={selected === model.id} onClick={(event) => { event.stopPropagation(); onSelect(model.id); }}><ModelName name={model.id} /></button><span className="model-meta"><span className="row-state"><span className={`status-dot ${workerTone(model)}`} />{stateLabel(model)}</span>{[...taskNames(model), quants.get(model.id)].filter(Boolean).join(" · ")}{updates.has(model.id) && <> · {client ? <ModelUpdate client={client} model={model} online={online} available compact onDone={() => { setChecked(null); void listed.reload(); }} /> : <span className="update-note">update available</span>}</>}</span></div></div>
        <div role="cell" className={`model-state ${model.error || !model.downloaded ? "tone-bad" : ""}`} title={residencyNote(model) ?? undefined}><span className={`status-dot ${workerTone(model)}`} />{stateLabel(model)}{model.loaded && model.active_requests > 0 ? <span className="muted"> · {model.active_requests}/{model.max_inflight} busy</span> : null}<LoadingNote model={model} /></div>
        <div role="cell" onChange={() => keep(model.id)}><PolicySelect model={model} models={models} resources={resources} mutate={mutate} disabled={locked} client={client} /></div>
        <div role="cell" className={`numeric ${model.loaded ? "" : "muted"}`}>{bytes(model.resident_bytes)}</div>
        <div role="cell" className="row-end"><RowAction model={model} mutate={mutate} disabled={locked} /></div>
      </div>)}
      {visibleFiles.map((group) => <FileRow key={group.id} group={group} models={models} listed={filesOnly} selected={selectedFile === group.id} onSelect={() => onSelectFile?.(group.id)} />)}
    </div>}
  </section>;
}

export const primaryRepo = (model: Model, inventory: LocalInventory | null): string | null => weightsGroup(model, inventory)?.repo ?? null;

type InspectorTab = "overview" | "settings" | "manage";

export function Inspector({ model, models = [], resources, busy, online, onClose, onDirty, mutate, client, stopLoad, inventory = null, onRemoved, onOpenModel, onOpenFile, onDiscover }: { model: Model; models?: Model[]; resources?: Resources; busy: boolean; online: boolean; onClose: () => void; onDirty: (value: boolean) => void; mutate: Mutate; client: Client; stopLoad?: () => void; inventory?: LocalInventory | null; onRemoved?: () => void; onOpenModel?: (id: string) => void; onOpenFile?: (group: string, quant: string) => void; onDiscover?: (target: DiscoverTarget) => void }) {
  const [profileDirty, setProfileDirty] = useState(false);
  const [artifactDirty, setArtifactDirty] = useState(false);
  const [runtimeDirty, setRuntimeDirty] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const dirty = profileDirty || artifactDirty || runtimeDirty;
  useEffect(() => { onDirty(dirty); }, [dirty, onDirty]);
  const heading = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    // Remounts for other reasons (a new connection, a Barista change) must not pull focus out of where the operator is typing.
    const active = document.activeElement;
    if (!active || active === document.body || active.closest(".inventory")) heading.current?.focus({ preventScroll: true });
  }, []);
  const close = () => { onClose(); focusRow(model.id); };
  const loadBlocked = loadReason(model);
  const unloadBlocked = unloadReason(model);
  const cleanupRetry = model.state === "cleanup-pending" && model.cleanup_retry_available === true;
  const [tab, setTab] = useState<InspectorTab>("overview");
  const repo = primaryRepo(model, inventory);
  const [advanced, setAdvanced] = useState(() => readLocal("juiced.rewrite.inspectorAdvanced") === "1");
  const fit = hotFit(model, models, resources);
  const tabs = [["overview", "Overview", false], ["settings", "Settings", dirty], ["manage", "Manage", false]] as const;
  const pick = (index: number) => { const next = tabs[(index + tabs.length) % tabs.length]![0]; setTab(next); document.getElementById(`tab-${next}`)?.focus(); };
  const removal = removalReason(model);
  const remove = () => { setConfirmRemove(false); void mutate("delete", { model: model.id, expected_revision: model.revision }, `Removed ${model.id}. Its files stay on disk.`).then((removed) => { if (removed) onRemoved?.(); }); };
  const hint = !model.loaded ? loadBlocked : unloadBlocked;
  return <aside className="inspector" aria-label="Model inspector" onKeyDown={(event) => {
    if (event.key !== "Escape" || event.defaultPrevented || (event.target as HTMLElement).closest("[popover], dialog") || openPopover()) return;
    event.preventDefault(); close();
  }}>
    <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={close}><Icon name="back" /> Back to models</Button><Button variant="ghost" className="close-inspector" aria-label="Close model inspector" onClick={close}><Icon name="close" /></Button></div>
    <div className="inspector-body">
      <div className="inspector-title"><h2 ref={heading} tabIndex={-1}><ModelName name={model.id} /></h2><Badge tone={!model.downloaded ? "bad" : workerTone(model)}>{stateLabel(model)}</Badge></div>
      {repo && <a className="model-card-link" href={`https://huggingface.co/${repo}`} target="_blank" rel="noreferrer noopener">{repo} <span aria-hidden="true">↗</span><span className="sr-only">: model card and README on Hugging Face</span></a>}
      <ModelUpdate client={client} model={model} online={online && !dirty} />
      <div className="tab-row"><div className="tabs" role="tablist" aria-label="Model sections" onKeyDown={(event) => {
        const index = tabs.findIndex(([id]) => id === tab);
        if (event.key === "ArrowRight") { event.preventDefault(); pick(index + 1); }
        if (event.key === "ArrowLeft") { event.preventDefault(); pick(index - 1); }
        if (event.key === "Home") { event.preventDefault(); pick(0); }
        if (event.key === "End") { event.preventDefault(); pick(tabs.length - 1); }
      }}>{tabs.map(([id, label, changed]) => <button key={id} role="tab" id={`tab-${id}`} aria-controls={`panel-${id}`} aria-selected={tab === id} tabIndex={tab === id ? 0 : -1} onClick={() => setTab(id)}>{label}{changed && <span className="tab-dot" aria-label="unsaved" />}</button>)}</div>
        <Switch checked={advanced} label="Show advanced settings" onChange={(next) => { setAdvanced(next); writeLocal("juiced.rewrite.inspectorAdvanced", next ? "1" : ""); }}>Advanced</Switch></div>
      {model.error && <p className={["request_termination_unconfirmed", "state_termination_unconfirmed"].includes(model.error) ? "inline-warning" : "inline-error"} role="alert">{workerFailure(model.error)}</p>}
      <div role="tabpanel" id="panel-overview" aria-labelledby="tab-overview" hidden={tab !== "overview"}>
      <section className="inspector-section"><span className="field-label" id="policy-label">Loading policy</span>
        <div className="segmented wide" role="radiogroup" aria-labelledby="policy-label">{(["hot", "on-demand", "disabled"] as Policy[]).map((policy) => <button key={policy} role="radio" aria-checked={model.policy === policy} disabled={busy || !online || dirty || (policy === "disabled" && model.active_requests > 0)} title={policy === "disabled" && model.active_requests > 0 ? "Requests are still running on this model" : policy === "disabled" && model.loaded ? "Disabling also unloads the model" : undefined} onClick={() => setPolicy(mutate, model, policy, client)}>{policyNames[policy]}</button>)}</div>
        {model.policy === "hot" && cleanupRetry && <p className="field-help">Retrying cleanup keeps the Hot policy.</p>}
        {fit && <p className={`field-help ${fit.fit !== "fits" && !(model.policy === "hot" && model.loaded) ? "tone-warning" : ""}`}>{hotFitNote(model, fit)}</p>}
        <ModelIdle model={model} client={client} online={online} disabled={busy || dirty} />
      </section>
      <section className="inspector-section"><h3>Worker</h3>
        {model.state === "loading" && <p className="field-help load-line" role="status">Loading<LoadingNote model={model} bar /></p>}
        <dl className="facts"><div><dt>Requests</dt><dd>{model.active_requests} / {model.max_inflight}</dd></div>
          {advanced && <div><dt>Process</dt><dd>{model.pid ?? "Not running"}</dd></div>}
          {(advanced || !model.downloaded) && <div><dt>Weights</dt><dd className={model.downloaded ? undefined : "tone-bad"}>{model.downloaded ? "Present" : "Missing"}</dd></div>}
          {advanced && typeof model.last_load_ms === "number" && <div><dt>Last load</dt><dd>{seconds(model.last_load_ms)}</dd></div>}</dl>
        {(model.uncertain_state_operations ?? 0) > 0 && <p className="inline-warning">A snapshot didn't confirm it finished. Its slot and memory stay held until you retry cleanup.</p>}
        {(model.uncertain_requests ?? 0) > 0 && <p className="inline-warning">The last request didn't confirm it stopped. Retry cleanup to release its slot.</p>}
        <div className="action-row wrap">{model.loaded || cleanupRetry
          ? <Button disabled={busy || !online || !!unloadBlocked || dirty} onClick={() => void mutate("unload", { model: model.id }, cleanupRetry ? `Confirmed cleanup for ${model.id}.` : `Unloaded ${model.id}.`)}>{cleanupRetry ? "Retry cleanup" : "Unload"}</Button>
          : <Button variant="primary" disabled={busy || !online || !!loadBlocked || dirty} onClick={() => void mutate("load", { model: model.id }, `Loaded ${model.id}.`)}>Load</Button>}
          {stopLoad && <Button onClick={stopLoad}>Stop waiting</Button>}</div>
        {(model.policy === "disabled" || !model.downloaded || model.active_requests > 0 || model.policy === "hot") && hint && <p className={!model.loaded && !model.downloaded ? "inline-error" : "field-help"}>{hint}</p>}
        {dirty && <p className="field-help">Save or cancel your changes first.</p>}
      </section>
      <ModelCheck model={model} client={client} online={online} disabled={busy || dirty} />
      {onOpenModel && onOpenFile && onDiscover && <ModelQuants model={model} models={models} inventory={inventory} client={client} online={online} onOpenModel={onOpenModel} onOpenFile={onOpenFile} onDiscover={onDiscover} />}
      </div>
      <div role="tabpanel" id="panel-settings" aria-labelledby="tab-settings" hidden={tab !== "settings"}>
      <ContextInspector model={model} client={client} online={online} busy={busy} policyDirty={artifactDirty || runtimeDirty} onDirty={setProfileDirty} mutate={mutate} advanced={advanced} />
      <QuantSync model={model} client={client} online={online} busy={busy} blocked={dirty} />
      {(advanced || artifactDirty) && model.artifact_set && <ArtifactRevisionInspector model={model} client={client} busy={busy} online={online} otherDirty={profileDirty || runtimeDirty} onDirty={setArtifactDirty} mutate={mutate} />}
      {(advanced || runtimeDirty) && <RuntimeInspector model={model} client={client} busy={busy} online={online} otherDirty={profileDirty || artifactDirty} onDirty={setRuntimeDirty} mutate={mutate} />}
      </div>
      <div role="tabpanel" id="panel-manage" aria-labelledby="tab-manage" hidden={tab !== "manage"}>
      {advanced && <section className="inspector-section"><h3>Operations</h3><div className="chip-row">{model.operations.map((operation) => <code className="chip" key={operation}>{operation}</code>)}</div></section>}
      <ModelFiles model={model} inventory={inventory} />
      <section className="inspector-section"><div className="action-row"><Button variant="danger" disabled={busy || !online || dirty || !!removal} onClick={() => setConfirmRemove(true)}>Remove model</Button><HelpTip label="Remove model">{removalNote(model, inventory)}</HelpTip></div>{removal && <p className="field-help">{removal}</p>}</section>
      </div>
    </div>
    {confirmRemove && <Dialog title={`Remove ${model.id}?`} onClose={() => setConfirmRemove(false)}><p>Its settings go. Files stay on disk.</p><div className="dialog-actions"><Button onClick={() => setConfirmRemove(false)}>Cancel</Button><Button variant="danger" onClick={remove}>Remove model</Button></div></Dialog>}
  </aside>;
}

function readLocal(key: string) { try { return localStorage.getItem(key) ?? ""; } catch { return ""; } }
function writeLocal(key: string, value: string) { try { if (value) localStorage.setItem(key, value); else localStorage.removeItem(key); } catch { /* Storage can be unavailable in private sessions. */ } }
