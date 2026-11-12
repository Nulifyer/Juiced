import { useState } from "react";
import type { Client } from "./api";
import { bytes } from "./domain";
import { Meter } from "./charts";
import { Badge, Button, Empty, HelpTip } from "./ui";
import { useOperation, useResource } from "./useResource";

export type SessionMode = "off" | "memory" | "persistent";
type Settings = { mode: SessionMode; budget_bytes: number; model_budget_bytes: number; max_sessions: number; save_delay_ms: number; models: Record<string, SessionMode> };
type ModelRow = { model: string; mode: string; effective: string; reason: string | null; sessions: number; used_bytes: number; budget_bytes: number };
type Session = { session: string; model: string; protocol: string; kind: string; bytes: number; last_used_unix_ms: number; execution_current: boolean; state: string };
export type SessionView = { generation: number; settings: Settings; models: ModelRow[]; sessions: Session[]; totalBytes: number; totalSessions: number; explicit: boolean };
export type Turn = { id: number; model: string; path: string; restoreMs: number | null; cached: number | null; processed: number | null; elapsedMs: number | null };

const modes: SessionMode[] = ["off", "memory", "persistent"];
const modeNames: Record<SessionMode, string> = { off: "Off", memory: "Memory", persistent: "Persistent" };
const pathNames: Record<string, string> = { "slot-hit": "Slot hit", restored: "Restored", cold: "Cold", bypass: "Bypassed" };
const record = (value: unknown): Record<string, unknown> => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
const num = (value: unknown) => typeof value === "number" && Number.isFinite(value) ? value : null;
const str = (value: unknown) => typeof value === "string" ? value : null;
const mode = (value: unknown): SessionMode => modes.includes(value as SessionMode) ? value as SessionMode : "off";

export function parseSessions(value: unknown): SessionView {
  const body = record(value), settings = record(body.settings), totals = record(body.totals);
  if (num(body.configuration_generation) === null || !Array.isArray(body.models) || !Array.isArray(body.sessions)) throw new Error("The service returned an unexpected session list.");
  return {
    generation: num(body.configuration_generation)!, explicit: body.explicit_snapshots_enabled === true,
    settings: { mode: mode(settings.mode), budget_bytes: num(settings.budget_bytes) ?? 0, model_budget_bytes: num(settings.model_budget_bytes) ?? 0, max_sessions: num(settings.max_sessions) ?? 0, save_delay_ms: num(settings.save_delay_ms) ?? 0, models: Object.fromEntries(Object.entries(record(settings.models)).map(([key, item]) => [key, mode(item)])) },
    models: body.models.map((item) => { const row = record(item); return { model: str(row.model) ?? "", mode: str(row.mode) ?? "off", effective: str(row.effective) ?? "off", reason: str(row.reason), sessions: num(row.sessions) ?? 0, used_bytes: num(row.used_bytes) ?? 0, budget_bytes: num(row.budget_bytes) ?? 0 }; }),
    sessions: body.sessions.map((item) => { const row = record(item); return { session: str(row.session) ?? "", model: str(row.model) ?? "", protocol: str(row.protocol) ?? "", kind: str(row.kind) ?? "", bytes: num(row.bytes) ?? 0, last_used_unix_ms: num(row.last_used_unix_ms) ?? 0, execution_current: row.execution_current === true, state: str(row.state) ?? "" }; }),
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

function ago(ms: number) {
  const seconds = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)}h ago`;
  return `${Math.round(seconds / 86400)}d ago`;
}

const reasons: Record<string, string> = {
  engine_unsupported: "This engine does not save prompt state.",
  protected_memory: "Protected memory keeps sessions off.",
  cache_storage_unavailable: "Cache storage is unavailable, so only memory is used.",
};

export function CacheSessions({ client, online }: { client: Client; online: boolean }) {
  const view = useResource(client, "juiced/cache/sessions", parseSessions, 3000);
  const turns = useResource(client, "juiced/requests", parseTurns, 3000);
  const operation = useOperation(client, async () => { await view.reload(); await turns.reload(); });
  const [adding, setAdding] = useState("");
  const [addMode, setAddMode] = useState<SessionMode | "">("");
  const data = view.data;
  if (!data) return view.error ? <div className="action-row"><p className="inline-error" role="alert">{view.error}</p><Button onClick={() => void view.reload()}>Retry</Button></div> : <p className="muted">Loading sessions…</p>;
  const blocked = !online || operation.busy;
  const fallback = data.settings.mode;
  const configure = (settings: Settings, success: string) => operation.run("juiced/cache/sessions", { action: "configure", expected_generation: data.generation, settings }, success);
  const forget = (target: { model?: string; session?: string }, success: string) => void operation.run("juiced/cache/sessions", { action: "forget", ...target }, success);
  const setModelMode = (model: string, value: SessionMode | null) => {
    const models = { ...data.settings.models };
    if (value === null || value === fallback) delete models[model]; else models[model] = value;
    return configure({ ...data.settings, models }, value === null || value === fallback ? `${model} follows the default mode.` : `${model} uses ${modeNames[value]} sessions.`);
  };
  const rows = new Map(data.models.map((row) => [row.model, row]));
  const overrides = Object.entries(data.settings.models).filter(([, value]) => value !== fallback).sort(([left], [right]) => left.localeCompare(right));
  const addable = data.models.filter((row) => row.reason !== "engine_unsupported" && !(row.model in data.settings.models)).map((row) => row.model).sort((left, right) => left.localeCompare(right));
  const otherModes = modes.filter((item) => item !== fallback);
  const chosenMode = addMode && addMode !== fallback ? addMode : otherModes[0];
  return <div className="page-body cache-sessions">
    <section className="panel session-summary" aria-label="Session continuation">
      <div className="summary-mode">
        <div className="page-title"><h2>Continue conversations</h2><HelpTip label="Session continuation">When a client sends an older conversation again, Juiced restores the engine's saved state for it so only the new message is processed. Persistent keeps that state on disk across unloads, reloads and restarts; Memory only routes each conversation back to the slot that still holds it. Only fingerprints of the messages are stored, never their text.</HelpTip></div>
        <div className="segmented" role="radiogroup" aria-label="Default session mode">{modes.map((item) => <button key={item} role="radio" aria-checked={fallback === item} disabled={blocked} onClick={() => fallback !== item && void configure({ ...data.settings, mode: item, models: Object.fromEntries(Object.entries(data.settings.models).filter(([, value]) => value !== item)) }, `Session continuation is ${modeNames[item]}.`)}>{modeNames[item]}</button>)}</div>
      </div>
      <div className="summary-usage">
        <Meter label="Saved state" value={data.settings.budget_bytes ? data.totalBytes / data.settings.budget_bytes * 100 : null} />
        <span className="muted">{bytes(data.totalBytes)} of {bytes(data.settings.budget_bytes)} · {data.totalSessions} {data.totalSessions === 1 ? "session" : "sessions"}</span>
      </div>
    </section>
    {operation.error && <p className="inline-error" role="alert">{operation.error}</p>}
    {view.error && <p className="inline-warning">Showing the last observation. {view.error}</p>}
    <section aria-label="Model overrides" className="session-overrides">
      <div className="toolbar"><h2 className="toolbar-title">Model overrides</h2><HelpTip label="Model overrides">Models without an override use the default mode. Only llama.cpp models save conversation state.</HelpTip>{overrides.length === 0 && <span className="muted">None. Every model uses {modeNames[fallback]}.</span>}</div>
      {overrides.length > 0 && <div className="data-table override-table" role="table" aria-label="Session overrides">
        {overrides.map(([model, value]) => {
          const row = rows.get(model);
          const note = !row ? "Not installed" : row.effective !== value && row.reason ? reasons[row.reason] ?? row.reason : null;
          return <div role="row" className="data-row" key={model} aria-label={model}>
            <span role="cell" className="break-word"><strong>{model}</strong>{note && <span className="field-help">{note}</span>}</span>
            <span role="cell"><select aria-label={`Session mode for ${model}`} value={value} disabled={blocked} onChange={(event) => void setModelMode(model, event.target.value as SessionMode)}>{modes.map((item) => <option key={item} value={item}>{modeNames[item]}{item === fallback ? " (default)" : ""}</option>)}</select></span>
            <span role="cell" className="row-end"><Button variant="ghost" disabled={blocked} onClick={() => void setModelMode(model, null)}>Remove</Button></span>
          </div>;
        })}
      </div>}
      <div className="toolbar override-add" aria-label="Add an override">
        <select aria-label="Model to override" value={adding} disabled={blocked || addable.length === 0} onChange={(event) => setAdding(event.target.value)}><option value="">{addable.length ? "Choose a model…" : "No other llama.cpp models"}</option>{addable.map((model) => <option key={model} value={model}>{model}</option>)}</select>
        <select aria-label="Override mode" value={chosenMode} disabled={blocked} onChange={(event) => setAddMode(event.target.value as SessionMode)}>{otherModes.map((item) => <option key={item} value={item}>{modeNames[item]}</option>)}</select>
        <Button disabled={blocked || !adding} onClick={async () => { if (await setModelMode(adding, chosenMode) !== undefined) setAdding(""); }}>Add override</Button>
      </div>
    </section>
    <section aria-label="Recent turns">
      <div className="toolbar"><h2 className="toolbar-title">Recent turns</h2><HelpTip label="Recent turns">Slot hit: the conversation was still loaded. Restored: Juiced loaded its saved state first. Cold: the whole prompt was processed.</HelpTip></div>
      {(turns.data ?? []).length === 0 ? <p className="muted">Send a chat to any text model to see how each turn continued.</p>
        : <ul className="turn-list">{turns.data!.map((turn) => <li key={turn.id}><Badge tone={turn.path === "cold" || turn.path === "bypass" ? "quiet" : "good"}>{pathNames[turn.path] ?? turn.path}</Badge><span className="break-word">{turn.model}</span><span className="muted">{turn.processed !== null ? `${turn.processed.toLocaleString()} new` : ""}{turn.cached ? ` · ${turn.cached.toLocaleString()} reused` : ""}{turn.restoreMs !== null ? ` · restore ${Math.round(turn.restoreMs)} ms` : ""}{turn.elapsedMs !== null ? ` · ${(turn.elapsedMs / 1000).toFixed(2)} s` : ""}</span></li>)}</ul>}
    </section>
    <section aria-label="Saved sessions">
      <div className="toolbar"><h2 className="toolbar-title">Saved sessions</h2>{data.sessions.length > 0 && <Button variant="danger" disabled={blocked} onClick={() => forget({}, "Forgot every saved session.")}>Forget all</Button>}</div>
      {data.sessions.length === 0 ? <Empty title="No saved sessions">{fallback === "persistent" ? "Conversations are saved here automatically after each reply." : "Set the mode to Persistent to keep conversations across unloads and restarts."}</Empty>
        : <div className="data-table session-table" role="table" aria-label="Saved session list">
          <div role="row" className="data-head"><span role="columnheader">Session</span><span role="columnheader">Model</span><span role="columnheader">Used</span><span role="columnheader" className="numeric">Size</span><span role="columnheader"><span className="sr-only">Actions</span></span></div>
          {data.sessions.map((session) => <div role="row" className="data-row" key={`${session.session}-${session.model}-${session.last_used_unix_ms}`} aria-label={`Session ${session.session.slice(0, 8)}`}>
            <span role="cell"><code title={session.session}>{session.session.slice(0, 8)}</code> <span className="muted">{session.protocol}</span>{!session.execution_current && <Badge tone="warning">Stale</Badge>}{session.state !== "saved" && <span title="The save did not finish. Its space is returned once the engine that wrote it has exited."><Badge tone="warning">Incomplete</Badge></span>}</span>
            <span role="cell" className="break-word">{session.model}</span>
            <span role="cell" className="muted">{ago(session.last_used_unix_ms)}</span>
            <span role="cell" className="numeric">{bytes(session.bytes)}</span>
            <span role="cell" className="row-end"><Button variant="ghost" disabled={blocked} onClick={() => forget({ session: session.session }, "Session forgotten.")}>Forget</Button></span>
          </div>)}
        </div>}
    </section>
  </div>;
}
