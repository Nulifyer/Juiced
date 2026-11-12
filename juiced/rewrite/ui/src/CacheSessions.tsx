import type { Client } from "./api";
import { bytes } from "./domain";
import { Meter } from "./charts";
import { Badge, Button, Empty, HelpTip } from "./ui";
import { useOperation, useResource } from "./useResource";

export type SessionMode = "off" | "memory" | "persistent";
type Settings = { mode: SessionMode; budget_bytes: number; model_budget_bytes: number; max_sessions: number; save_delay_ms: number; models: Record<string, SessionMode> };
type Slot = { slot: number; state: string; tokens: number | null };
type ModelRow = { model: string; mode: string; effective: string; reason: string | null; sessions: number; used_bytes: number; budget_bytes: number; slots: Slot[] };
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
    models: body.models.map((item) => { const row = record(item); return { model: str(row.model) ?? "", mode: str(row.mode) ?? "off", effective: str(row.effective) ?? "off", reason: str(row.reason), sessions: num(row.sessions) ?? 0, used_bytes: num(row.used_bytes) ?? 0, budget_bytes: num(row.budget_bytes) ?? 0, slots: (Array.isArray(row.slots) ? row.slots : []).map((slot) => { const entry = record(slot); return { slot: num(entry.slot) ?? 0, state: str(entry.state) ?? "empty", tokens: num(entry.tokens) }; }) }; }),
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

export function CacheSessions({ client, online }: { client: Client; online: boolean }) {
  const view = useResource(client, "juiced/cache/sessions", parseSessions, 3000);
  const turns = useResource(client, "juiced/requests", parseTurns, 3000);
  const operation = useOperation(client, async () => { await view.reload(); await turns.reload(); });
  const data = view.data;
  if (!data) return view.error ? <div className="action-row"><p className="inline-error" role="alert">{view.error}</p><Button onClick={() => void view.reload()}>Retry</Button></div> : <p className="muted">Loading sessions…</p>;
  const blocked = !online || operation.busy;
  const configure = (settings: Settings, success: string) => void operation.run("juiced/cache/sessions", { action: "configure", expected_generation: data.generation, settings }, success);
  const forget = (target: { model?: string; session?: string }, success: string) => void operation.run("juiced/cache/sessions", { action: "forget", ...target }, success);
  const setModelMode = (model: string, value: string) => {
    const models = { ...data.settings.models };
    if (value === "inherit") delete models[model]; else models[model] = value as SessionMode;
    configure({ ...data.settings, models }, value === "inherit" ? `${model} follows the default mode.` : `${model} uses ${modeNames[value as SessionMode]} sessions.`);
  };
  return <div className="page-body cache-sessions">
    <section className="panel session-summary" aria-label="Session continuation">
      <div className="summary-mode">
        <div className="page-title"><h2>Continue conversations</h2><HelpTip label="Session continuation">When a client sends an older conversation again, Juiced restores the engine's saved state for it so only the new message is processed. Persistent keeps that state on disk across unloads and restarts; Memory only routes each conversation back to the slot that still holds it. Only fingerprints of the messages are stored, never their text.</HelpTip></div>
        <div className="segmented" role="radiogroup" aria-label="Default session mode">{modes.map((item) => <button key={item} role="radio" aria-checked={data.settings.mode === item} disabled={blocked} onClick={() => data.settings.mode !== item && configure({ ...data.settings, mode: item }, `Session continuation is ${modeNames[item]}.`)}>{modeNames[item]}</button>)}</div>
      </div>
      <div className="summary-usage">
        <Meter label="Saved state" value={data.settings.budget_bytes ? data.totalBytes / data.settings.budget_bytes * 100 : null} />
        <span className="muted">{bytes(data.totalBytes)} of {bytes(data.settings.budget_bytes)} · {data.totalSessions} {data.totalSessions === 1 ? "session" : "sessions"}</span>
      </div>
    </section>
    {operation.error && <p className="inline-error" role="alert">{operation.error}</p>}
    {view.error && <p className="inline-warning">Showing the last observation. {view.error}</p>}
    <section aria-label="Models">
      <div className="data-table session-models" role="table" aria-label="Session models">
        <div role="row" className="data-head"><span role="columnheader">Model</span><span role="columnheader">Mode</span><span role="columnheader">Slots</span><span role="columnheader" className="numeric">Saved</span></div>
        {data.models.map((row) => <div role="row" className="data-row" key={row.model} aria-label={row.model}>
          <span role="cell" className="break-word"><strong>{row.model}</strong>{row.reason && <span className="field-help">{row.reason}</span>}</span>
          <span role="cell"><select aria-label={`Session mode for ${row.model}`} value={data.settings.models[row.model] ?? "inherit"} disabled={blocked} onChange={(event) => setModelMode(row.model, event.target.value)}><option value="inherit">Default ({modeNames[data.settings.mode]})</option>{modes.map((item) => <option key={item} value={item}>{modeNames[item]}</option>)}</select></span>
          <span role="cell" className="slot-chips">{row.slots.map((slot) => <span key={slot.slot} className="slot-chip" data-state={slot.state} title={slot.tokens ? `${slot.tokens.toLocaleString()} tokens` : undefined}>{slot.slot}: {slot.state}</span>)}</span>
          <span role="cell" className="numeric">{row.sessions ? `${bytes(row.used_bytes)} · ${row.sessions}` : "—"}</span>
        </div>)}
      </div>
    </section>
    <section aria-label="Recent turns">
      <div className="toolbar"><h2 className="toolbar-title">Recent turns</h2><HelpTip label="Recent turns">Slot hit: the conversation was still loaded. Restored: Juiced loaded its saved state first. Cold: the whole prompt was processed.</HelpTip></div>
      {(turns.data ?? []).length === 0 ? <p className="muted">Send a chat to any text model to see how each turn continued.</p>
        : <ul className="turn-list">{turns.data!.map((turn) => <li key={turn.id}><Badge tone={turn.path === "cold" || turn.path === "bypass" ? "quiet" : "good"}>{pathNames[turn.path] ?? turn.path}</Badge><span className="break-word">{turn.model}</span><span className="muted">{turn.processed !== null ? `${turn.processed.toLocaleString()} new` : ""}{turn.cached ? ` · ${turn.cached.toLocaleString()} reused` : ""}{turn.restoreMs !== null ? ` · restore ${Math.round(turn.restoreMs)} ms` : ""}{turn.elapsedMs !== null ? ` · ${(turn.elapsedMs / 1000).toFixed(2)} s` : ""}</span></li>)}</ul>}
    </section>
    <section aria-label="Saved sessions">
      <div className="toolbar"><h2 className="toolbar-title">Saved sessions</h2>{data.sessions.length > 0 && <Button variant="danger" disabled={blocked} onClick={() => forget({}, "Forgot every saved session.")}>Forget all</Button>}</div>
      {data.sessions.length === 0 ? <Empty title="No saved sessions">{data.settings.mode === "persistent" ? "Conversations are saved here automatically after each reply." : "Set the mode to Persistent to keep conversations across unloads and restarts."}</Empty>
        : <div className="data-table session-table" role="table" aria-label="Saved session list">
          <div role="row" className="data-head"><span role="columnheader">Session</span><span role="columnheader">Model</span><span role="columnheader">Used</span><span role="columnheader" className="numeric">Size</span><span role="columnheader"><span className="sr-only">Actions</span></span></div>
          {data.sessions.map((session) => <div role="row" className="data-row" key={session.session} aria-label={`Session ${session.session.slice(0, 8)}`}>
            <span role="cell"><code title={session.session}>{session.session.slice(0, 8)}</code> <span className="muted">{session.protocol}</span>{!session.execution_current && <Badge tone="warning">Stale</Badge>}</span>
            <span role="cell" className="break-word">{session.model}</span>
            <span role="cell" className="muted">{ago(session.last_used_unix_ms)}</span>
            <span role="cell" className="numeric">{bytes(session.bytes)}</span>
            <span role="cell" className="row-end"><Button variant="ghost" disabled={blocked} onClick={() => forget({ session: session.session }, "Session forgotten.")}>Forget</Button></span>
          </div>)}
        </div>}
    </section>
  </div>;
}
