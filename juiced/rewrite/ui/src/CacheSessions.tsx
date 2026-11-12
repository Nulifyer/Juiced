import { useState } from "react";
import type { Client } from "./api";
import { Meter } from "./charts";
import { Badge, Button, Empty, HelpTip } from "./ui";
import { useOperation, useResource } from "./useResource";

export type SessionMode = "off" | "memory" | "persistent";
type Settings = { mode: SessionMode; budget_bytes: number; model_budget_bytes: number; max_sessions: number; save_delay_ms: number; models: Record<string, SessionMode> };
type ModelRow = { model: string; mode: string; effective: string; reason: string | null; sessions: number; used_bytes: number; budget_bytes: number };
type Session = { session: string; model: string | null; protocol: string; kind: string; bytes: number; last_used_unix_ms: number; execution_current: boolean | null; state: string };
export type SessionView = { generation: number; settings: Settings; models: ModelRow[]; sessions: Session[]; totalBytes: number; totalSessions: number; explicit: boolean };
export type Turn = { id: number; model: string; path: string; restoreMs: number | null; cached: number | null; processed: number | null; elapsedMs: number | null };

const modes: SessionMode[] = ["off", "memory", "persistent"];
export const modeNames: Record<SessionMode, string> = { off: "Off", memory: "Memory only", persistent: "Memory and disk" };
const pathNames: Record<string, string> = { "slot-hit": "Still loaded", restored: "Restored from disk", cold: "Processed in full", bypass: "Not cached" };
const apiNames: Record<string, string> = { chat: "Chat", completions: "Completions", responses: "Responses" };
const record = (value: unknown): Record<string, unknown> => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
const num = (value: unknown) => typeof value === "number" && Number.isFinite(value) ? value : null;
const str = (value: unknown) => typeof value === "string" ? value : null;
const mode = (value: unknown): SessionMode => modes.includes(value as SessionMode) ? value as SessionMode : "off";

export function size(value: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let scaled = Math.max(0, value), unit = 0;
  while (scaled >= 1024 && unit < units.length - 1) { scaled /= 1024; unit += 1; }
  const text = unit === 0 || scaled >= 100 ? scaled.toFixed(0) : scaled.toFixed(1).replace(/\.0$/, "");
  return `${text} ${units[unit]}`;
}

export function parseSessions(value: unknown): SessionView {
  const body = record(value), settings = record(body.settings), totals = record(body.totals);
  if (num(body.configuration_generation) === null || !Array.isArray(body.models) || !Array.isArray(body.sessions)) throw new Error("The service returned an unexpected session list.");
  return {
    generation: num(body.configuration_generation)!, explicit: body.explicit_snapshots_enabled === true,
    settings: { mode: mode(settings.mode), budget_bytes: num(settings.budget_bytes) ?? 0, model_budget_bytes: num(settings.model_budget_bytes) ?? 0, max_sessions: num(settings.max_sessions) ?? 0, save_delay_ms: num(settings.save_delay_ms) ?? 0, models: Object.fromEntries(Object.entries(record(settings.models)).map(([key, item]) => [key, mode(item)])) },
    models: body.models.map((item) => { const row = record(item); return { model: str(row.model) ?? "", mode: str(row.mode) ?? "off", effective: str(row.effective) ?? "off", reason: str(row.reason), sessions: num(row.sessions) ?? 0, used_bytes: num(row.used_bytes) ?? 0, budget_bytes: num(row.budget_bytes) ?? 0 }; }),
    sessions: body.sessions.map((item) => { const row = record(item); return { session: str(row.session) ?? "", model: str(row.model), protocol: str(row.protocol) ?? "", kind: str(row.kind) ?? "", bytes: num(row.bytes) ?? 0, last_used_unix_ms: num(row.last_used_unix_ms) ?? 0, execution_current: typeof row.execution_current === "boolean" ? row.execution_current : null, state: str(row.state) ?? "" }; }),
    totalBytes: num(totals.bytes) ?? 0, totalSessions: num(totals.sessions) ?? 0,
  };
}

export function parseTurns(value: unknown): Turn[] {
  const data = record(value).data;
  return (Array.isArray(data) ? data : []).flatMap((item): Turn[] => {
    const row = record(item), cache = record(row.session_cache);
    const path = str(cache.path);
    return path ? [{ id: num(row.id) ?? 0, model: str(row.model) ?? "", path, restoreMs: num(cache.restore_ms), cached: num(row.cached_tokens), processed: num(row.prompt_processed_tokens), elapsedMs: num(row.elapsed_ms) }] : [];
  }).sort((left, right) => right.id - left.id).slice(0, 12);
}

export function turnDetail(turn: Turn): string {
  const tokens = turn.cached ? `${turn.cached.toLocaleString()} cached + ${(turn.processed ?? 0).toLocaleString()} new tokens` : turn.processed !== null ? `${turn.processed.toLocaleString()} new tokens` : "";
  return [tokens, turn.restoreMs !== null ? `restored in ${Math.round(turn.restoreMs)} ms` : "", turn.elapsedMs !== null ? `${(turn.elapsedMs / 1000).toFixed(2)} s` : ""].filter(Boolean).join(" · ");
}

function ago(ms: number) {
  if (!ms) return "Unknown";
  const seconds = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)}h ago`;
  return `${Math.round(seconds / 86400)}d ago`;
}

const reasons: Record<string, string> = {
  engine_unsupported: "This engine doesn't support the conversation cache.",
  protected_memory: "Protected memory keeps the cache off.",
  cache_storage_unavailable: "Cache storage is unavailable, so only memory is used.",
};

export function ConversationCache({ client, online }: { client: Client; online: boolean }) {
  const view = useResource(client, "juiced/cache/sessions", parseSessions, 3000);
  const turns = useResource(client, "juiced/requests", parseTurns, 3000);
  const operation = useOperation(client, async () => { await view.reload(); await turns.reload(); });
  const [adding, setAdding] = useState("");
  const [addMode, setAddMode] = useState<SessionMode | "">("");
  const data = view.data;
  if (!data) return view.error ? <div className="action-row"><p className="inline-error" role="alert">{view.error}</p><Button onClick={() => void view.reload()}>Retry</Button></div> : <p className="muted">Loading the conversation cache…</p>;
  const blocked = !online || operation.busy;
  const fallback = data.settings.mode;
  const configure = (settings: Settings, success: string) => operation.run("juiced/cache/sessions", { action: "configure", expected_generation: data.generation, settings }, success);
  const forget = (target: { model?: string; session?: string }, success: string) => void operation.run("juiced/cache/sessions", { action: "forget", ...target }, success);
  const setModelMode = (model: string, value: SessionMode | null) => {
    const models = { ...data.settings.models };
    if (value === null || value === fallback) delete models[model]; else models[model] = value;
    return configure({ ...data.settings, models }, value === null || value === fallback ? `${model} uses the default again.` : `${model} uses ${modeNames[value]}.`);
  };
  const rows = new Map(data.models.map((row) => [row.model, row]));
  const overrides = Object.entries(data.settings.models).filter(([, value]) => value !== fallback).sort(([left], [right]) => left.localeCompare(right));
  const addable = data.models.filter((row) => row.reason !== "engine_unsupported" && !(row.model in data.settings.models)).map((row) => row.model).sort((left, right) => left.localeCompare(right));
  const otherModes = modes.filter((item) => item !== fallback);
  const chosenMode = addMode && addMode !== fallback ? addMode : otherModes[0];
  const count = `${data.totalSessions} ${data.totalSessions === 1 ? "conversation" : "conversations"}`;
  return <>
    <section className="panel session-summary" aria-label="Conversation cache">
      <div className="summary-mode">
        <div className="summary-title">
          <div className="page-title"><h2>Conversation cache</h2><HelpTip label="Conversation cache"><p><strong>Off:</strong> Juiced doesn't track conversations. A turn is fast only if the engine still holds that exact prompt.</p><p><strong>Memory only:</strong> each conversation goes back to the engine slot that still holds it. Needs a model with more than one slot. Lost when the model unloads.</p><p><strong>Memory and disk:</strong> also saves each conversation to disk after a reply, so it comes back after an unload, reload or restart.</p><p>Only llama.cpp models use this. Juiced stores fingerprints of the messages, never their text.</p></HelpTip></div>
          <p className="muted">Saves each conversation's processed prompt so the next turn only processes the new message.</p>
        </div>
        <div className="segmented" role="radiogroup" aria-label="Default cache mode">{modes.map((item) => <button key={item} role="radio" aria-checked={fallback === item} disabled={blocked} onClick={() => fallback !== item && void configure({ ...data.settings, mode: item, models: Object.fromEntries(Object.entries(data.settings.models).filter(([, value]) => value !== item)) }, `Conversation cache: ${modeNames[item]}.`)}>{modeNames[item]}</button>)}</div>
      </div>
      <div className="summary-usage">
        <Meter label="Disk used by saved conversations" value={data.settings.budget_bytes ? data.totalBytes / data.settings.budget_bytes * 100 : null} />
        <span className="muted">{size(data.totalBytes)} of {size(data.settings.budget_bytes)} used · {count}</span>
      </div>
    </section>
    {operation.error && <p className="inline-error" role="alert">{operation.error}</p>}
    {view.error && <p className="inline-warning">Showing the last observation. {view.error}</p>}
    <section aria-label="Model overrides" className="session-overrides">
      <div className="toolbar"><h2 className="toolbar-title">Model overrides</h2><HelpTip label="Model overrides">Give one model a different mode. Every other model uses the default above.</HelpTip>{overrides.length === 0 && <span className="muted">None. Every model uses {modeNames[fallback]}.</span>}</div>
      {overrides.length > 0 && <div className="data-table override-table" role="table" aria-label="Overrides">
        {overrides.map(([model, value]) => {
          const row = rows.get(model);
          const note = !row ? "Not installed" : row.effective !== value && row.reason ? reasons[row.reason] ?? row.reason : null;
          return <div role="row" className="data-row" key={model} aria-label={model}>
            <span role="cell" className="break-word"><strong>{model}</strong>{note && <span className="field-help">{note}</span>}</span>
            <span role="cell"><select aria-label={`Cache mode for ${model}`} value={value} disabled={blocked} onChange={(event) => void setModelMode(model, event.target.value as SessionMode)}>{modes.map((item) => <option key={item} value={item}>{modeNames[item]}{item === fallback ? " (default)" : ""}</option>)}</select></span>
            <span role="cell" className="row-end"><Button variant="ghost" disabled={blocked} onClick={() => void setModelMode(model, null)}>Remove</Button></span>
          </div>;
        })}
      </div>}
      {addable.length > 0 && <div className="toolbar override-add" aria-label="Add an override">
        <select aria-label="Model to override" value={adding} disabled={blocked} onChange={(event) => setAdding(event.target.value)}><option value="">Choose a model…</option>{addable.map((model) => <option key={model} value={model}>{model}</option>)}</select>
        <select aria-label="Override mode" value={chosenMode} disabled={blocked} onChange={(event) => setAddMode(event.target.value as SessionMode)}>{otherModes.map((item) => <option key={item} value={item}>{modeNames[item]}</option>)}</select>
        <Button disabled={blocked || !adding} onClick={async () => { if (await setModelMode(adding, chosenMode) !== undefined) setAdding(""); }}>Add override</Button>
      </div>}
    </section>
    <section aria-label="Recent turns">
      <div className="toolbar"><h2 className="toolbar-title">Recent turns</h2><HelpTip label="Recent turns"><p><strong>Still loaded:</strong> the engine still held this conversation.</p><p><strong>Restored from disk:</strong> Juiced loaded the saved conversation first.</p><p><strong>Processed in full:</strong> nothing was cached, so the whole prompt was processed.</p><p><strong>Not cached:</strong> the client turned caching off for the request.</p><p>Cached tokens were reused. New tokens were processed for this turn.</p></HelpTip></div>
      {(turns.data ?? []).length === 0 ? <p className="muted">Send a chat to a llama.cpp model to see how each turn used the cache.</p>
        : <ul className="turn-list">{turns.data!.map((turn) => <li key={turn.id}><Badge tone={turn.path === "cold" || turn.path === "bypass" ? "quiet" : "good"}>{pathNames[turn.path] ?? turn.path}</Badge><span className="break-word">{turn.model}</span><span className="muted">{turnDetail(turn)}</span></li>)}</ul>}
    </section>
    <section aria-label="Saved conversations">
      <div className="toolbar"><h2 className="toolbar-title">Saved conversations</h2><HelpTip label="Saved conversations">Conversations saved to disk, newest first. Each is named by a short fingerprint. Forgetting one only removes the cache; the next turn processes the whole prompt again.</HelpTip>{data.sessions.length > 0 && <><span className="spacer" /><Button variant="ghost" className="quiet-danger" disabled={blocked} onClick={() => forget({}, "Forgot every saved conversation.")}>Forget all</Button></>}</div>
      {data.sessions.length === 0 ? <Empty title="No saved conversations">{fallback === "persistent" ? "Conversations are saved here automatically after each reply." : "Choose Memory and disk to keep conversations across unloads and restarts."}</Empty>
        : <div className="data-table session-table" role="table" aria-label="Saved conversation list">
          <div role="row" className="data-head"><span role="columnheader">Conversation</span><span role="columnheader">Model</span><span role="columnheader">Last used</span><span role="columnheader" className="numeric">Size</span><span role="columnheader"><span className="sr-only">Actions</span></span></div>
          {data.sessions.map((session) => <div role="row" className="data-row" key={`${session.session}-${session.model}-${session.last_used_unix_ms}`} aria-label={`Conversation ${session.session.slice(0, 8)}`}>
            <span role="cell"><code title={session.session}>{session.session.slice(0, 8)}</code> <span className="muted">{apiNames[session.protocol] ?? session.protocol}{session.kind === "header" ? " · client session ID" : ""}</span>{session.execution_current === false && <span title="Saved by a different engine build or launch settings. It can't be restored and will be replaced."><Badge tone="warning">Engine changed</Badge></span>}{session.state !== "saved" && <span title="The save didn't finish. Its space is returned once the engine that wrote it has exited."><Badge tone="warning">Incomplete</Badge></span>}</span>
            <span role="cell" className="break-word">{session.model ?? <span className="muted">Removed model</span>}</span>
            <span role="cell" className="muted">{ago(session.last_used_unix_ms)}</span>
            <span role="cell" className="numeric">{size(session.bytes)}</span>
            <span role="cell" className="row-end"><Button variant="ghost" disabled={blocked} onClick={() => forget({ session: session.session }, "Conversation forgotten.")}>Forget</Button></span>
          </div>)}
        </div>}
    </section>
  </>;
}
