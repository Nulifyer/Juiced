import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, type Client } from "./api";
import { MAX_STEPS, action, callTool, candidates, describe, implication, outcome, parseArguments, permissionHelp, permissionNames, readOnly, stopCodes, streamReply, systemPrompt, type BaristaSettings, type ChatMessage, type Permission } from "./baristaTools";
import { withImages, withoutImages } from "./chatImages";
import { AssistantBody, Composer, UserBody, thinkingClock, useAttachments } from "./ChatParts";
import type { Model } from "./domain";
import { Badge, Button, Empty, HelpTip, Icon } from "./ui";
import "./barista.css";

type Step = "running" | "pending" | "done" | "failed" | "skipped";
type Item =
  | { id: number; kind: "user"; text: string; images?: string[] }
  | { id: number; kind: "assistant"; text: string; reasoning: string; streaming: boolean; since?: number; thoughtMs?: number }
  | { id: number; kind: "tool"; name: string; label: string; change: boolean; step: Step; detail?: string }
  | { id: number; kind: "note"; text: string };
type NewItem = Item extends infer Entry ? Entry extends Item ? Omit<Entry, "id"> : never : never;
type Saved = { items: Item[]; messages: ChatMessage[] };

const storageKey = "juiced.barista.v1";
const replyTokens = (model: Model) => Math.max(1, Math.min(4096, model.request_limits?.output_tokens ?? 4096));
function restore(): Saved {
  try {
    const saved = JSON.parse(sessionStorage.getItem(storageKey) ?? "null") as Saved | null;
    if (!saved || !Array.isArray(saved.items) || !Array.isArray(saved.messages)) return { items: [], messages: [] };
    return { messages: saved.messages, items: saved.items.map((item) => item.kind === "tool" && (item.step === "pending" || item.step === "running") ? { ...item, step: "skipped", detail: "Not run." } : item.kind === "assistant" ? { ...item, streaming: false } : item) };
  } catch { return { items: [], messages: [] }; }
}
function persist(saved: Saved) {
  try { sessionStorage.setItem(storageKey, JSON.stringify(saved)); return; } catch { /* Images can exceed the session storage quota. */ }
  try { sessionStorage.setItem(storageKey, JSON.stringify({ items: saved.items.map((item) => item.kind === "user" ? { ...item, images: undefined } : item), messages: withoutImages(saved.messages) })); } catch { /* Session storage can be unavailable. */ }
}

/** The invitation decision is a client preference. Juiced has no stable installation ID yet, so it is keyed by service origin. */
export function invitationKey(base: string): string {
  return `juiced.barista.invitation:${base || location.origin}`;
}

const panelKey = "juiced.barista.panel";
const dockQuery = "(min-width: 1100px)";

/** Whether the panel is open and pinned is a per-client preference, so it lives in this browser. */
export function useBaristaPanel() {
  const [state, setState] = useState<{ open: boolean; pinned: boolean }>(() => {
    try { const saved = JSON.parse(localStorage.getItem(panelKey) ?? "null"); return { open: saved?.open === true, pinned: saved?.pinned === true }; } catch { return { open: false, pinned: false }; }
  });
  const [dockable, setDockable] = useState(() => typeof matchMedia === "function" && matchMedia(dockQuery).matches);
  useEffect(() => {
    if (typeof matchMedia !== "function") return;
    const query = matchMedia(dockQuery);
    const change = () => setDockable(query.matches);
    change(); query.addEventListener("change", change);
    return () => query.removeEventListener("change", change);
  }, []);
  const update = useCallback((change: Partial<{ open: boolean; pinned: boolean }>) => setState((current) => {
    const next = { ...current, ...change };
    try { localStorage.setItem(panelKey, JSON.stringify(next)); } catch { /* Storage can be unavailable in private sessions. */ }
    return next;
  }), []);
  const setOpen = useCallback((open: boolean) => update({ open }), [update]);
  const onPin = useCallback((pinned: boolean) => update({ pinned }), [update]);
  return { open: state.open, setOpen, docked: state.pinned && dockable, layout: { pinned: state.pinned, dockable, onPin } };
}

/** Barista settings for operators. Other callers get null and see no Barista controls. */
export function useBarista(client: Client, online: boolean) {
  const [settings, setSettings] = useState<BaristaSettings | null>(null);
  const reload = useCallback(async (signal?: AbortSignal) => {
    try { const next = await client.request<BaristaSettings>("juiced/barista", undefined, signal); if (!signal?.aborted) setSettings(next); }
    catch (error) { if (!signal?.aborted && error instanceof ApiError && (error.status === 401 || error.status === 403)) setSettings(null); }
  }, [client]);
  useEffect(() => {
    setSettings(null);
    if (!online) return;
    const controller = new AbortController();
    void reload(controller.signal);
    return () => controller.abort();
  }, [reload, online]);
  return { settings, setSettings, reload };
}

function Setup({ settings, models, client, online, onSaved, onDisable, onChanged }: { settings: BaristaSettings; models: Model[]; client: Client; online: boolean; onSaved: (settings: BaristaSettings) => void; onDisable?: () => void; onChanged: () => void }) {
  const choices = candidates(models);
  const [modelId, setModelId] = useState(() => choices.find((model) => model.id === settings.model)?.id ?? choices[0]?.id ?? "");
  const [permission, setPermission] = useState<Permission>(settings.permission);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const model = choices.find((entry) => entry.id === modelId) ?? choices[0];
  const unchanged = settings.enabled && model?.id === settings.model && permission === settings.permission && model?.policy !== "disabled";

  async function save(enabled: boolean) {
    if (saving) return;
    setSaving(true); setError("");
    try {
      if (enabled && model?.policy === "disabled") { await client.request("juiced/policy", { model: model.id, policy: "on-demand", expected_revision: model.revision }); onChanged(); }
      if (!enabled) onDisable?.();
      onSaved(await client.request<BaristaSettings>("juiced/barista", { expected_revision: settings.revision, enabled, model: enabled ? model?.id ?? null : settings.model, permission: enabled ? permission : settings.permission }));
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "The change failed. Refresh and retry.");
    } finally { setSaving(false); }
  }

  return <form className="barista-setup" aria-label={settings.enabled ? "Barista settings" : "Set up Barista"} onSubmit={(event) => { event.preventDefault(); void save(true); }}>
    {!settings.enabled && <p>Barista manages Juiced for you from a chat. It uses one of your downloaded chat models.</p>}
    {choices.length === 0 ? <Empty title="No downloaded chat models">Download a chat model in Models, then come back here.</Empty> : <>
      <div><label htmlFor="barista-model">Model</label>
        <select id="barista-model" value={model?.id ?? ""} disabled={saving} onChange={(event) => setModelId(event.target.value)}>{choices.map((entry) => <option key={entry.id} value={entry.id}>{entry.id}</option>)}</select>
        {model && <p className="field-help">{implication(model)}</p>}
        {model && !settings.qualified && <p className="barista-unqualified">Not yet tested with Barista. Check what it tells you. <HelpTip label="Barista model testing">Juiced plans to test models on a fixed set of management tasks before it recommends one for Barista. No model has been tested yet, including this one. Barista still checks every change against your permissions.</HelpTip></p>}
      </div>
      <div><span className="field-label" id="barista-permission-label">Permissions</span>
        <div className="segmented wide" role="radiogroup" aria-labelledby="barista-permission-label">{(["full", "ask", "read_only"] as Permission[]).map((value) => <button key={value} type="button" role="radio" aria-checked={permission === value} disabled={saving} onClick={() => setPermission(value)}>{permissionNames[value]}</button>)}</div>
        <p className="field-help">{permissionHelp[permission]}</p>
      </div>
    </>}
    {error && <p className="inline-error" role="alert">{error}</p>}
    <div className="action-row">
      {settings.enabled && <Button variant="danger" disabled={saving || !online} onClick={() => void save(false)}>Turn off Barista</Button>}
      {model && <Button variant="primary" type="submit" disabled={saving || !online || unchanged}>{settings.enabled ? "Save" : model.policy === "disabled" ? "Set to On demand and turn on" : "Turn on Barista"}</Button>}
    </div>
  </form>;
}

function StepIcon({ step }: { step: Step }) {
  if (step === "running" || step === "pending") return <span className="step-spinner" aria-hidden="true" />;
  if (step === "done") return <span className="step-icon done"><Icon name="check" size={13} /></span>;
  if (step === "failed") return <span className="step-icon failed"><Icon name="close" size={13} /></span>;
  return <span className="step-icon skipped" aria-hidden="true">–</span>;
}

export type BaristaLayout = { pinned: boolean; dockable: boolean; onPin: (pinned: boolean) => void };

export function Barista({ open, onClose, models, ready, client, online, settings, onSettings, onChanged, layout }: { open: boolean; onClose: () => void; models: Model[]; ready: boolean; client: Client; online: boolean; settings: BaristaSettings; onSettings: (settings: BaristaSettings) => void; onChanged: () => void; layout: BaristaLayout }) {
  const [saved] = useState(restore);
  const [items, setItems] = useState<Item[]>(saved.items);
  const messages = useRef<ChatMessage[]>(saved.messages);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [task, setTask] = useState("");
  const [showSettings, setShowSettings] = useState(false);
  const request = useRef<AbortController | null>(null);
  const approvals = useRef(new Map<number, (approved: boolean) => void>());
  const nextId = useRef(Math.max(0, ...saved.items.map((item) => item.id)) + 1);
  const thread = useRef<HTMLDivElement>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const wasOpen = useRef(open);
  const model = models.find((entry) => entry.id === settings.model);
  const vision = model?.vision === true;
  const attachments = useAttachments(vision);
  const waiting = items.some((item) => item.kind === "tool" && item.step === "pending");
  const docked = layout.pinned && layout.dockable;
  const gone = ready && settings.enabled && !model;
  const setup = !settings.enabled || showSettings || gone;

  const stop = useCallback(() => { request.current?.abort(); for (const resolve of approvals.current.values()) resolve(false); }, []);
  useEffect(() => { persist({ items, messages: messages.current }); thread.current?.scrollTo({ top: thread.current.scrollHeight }); }, [items]);
  useEffect(() => stop, [client, stop]);
  useEffect(() => { if (!settings.enabled) { stop(); setShowSettings(false); } }, [settings.enabled, stop]);
  useEffect(() => {
    const opened = open && !wasOpen.current;
    wasOpen.current = open;
    if (opened && !setup) field.current?.focus();
  }, [open, setup]);
  useEffect(() => {
    if (!open || docked) return;
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape" && !document.querySelector("dialog[open], [popover]:popover-open")) onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, docked, onClose]);

  const add = (item: NewItem) => { const id = nextId.current++; setItems((current) => [...current, { ...item, id } as Item]); return id; };
  const patch = (id: number, change: Partial<Item>) => setItems((current) => current.map((item) => item.id === id ? { ...item, ...change } as Item : item));
  const approval = (id: number) => new Promise<boolean>((resolve) => approvals.current.set(id, (approved) => { approvals.current.delete(id); resolve(approved); }));
  const decide = (id: number, approved: boolean) => approvals.current.get(id)?.(approved);

  async function send() {
    const text = input.trim();
    const images = attachments.list;
    if ((!text && images.length === 0) || busy || !model) return;
    setInput(""); attachments.clear(); setBusy(true); setTask(text || "Image");
    const controller = new AbortController(); request.current = controller;
    const signal = controller.signal;
    add({ kind: "user", text, ...(images.length ? { images: images.map((image) => image.url) } : {}) });
    messages.current = [...messages.current, { role: "user", content: withImages(text, images) }];
    try {
      const current = await client.request<BaristaSettings>("juiced/barista", undefined, signal);
      onSettings(current);
      if (!current.enabled) throw new ApiError("Barista is off.", 409, "assistant_disabled");
      const byName = new Map(current.tools.map((tool) => [tool.name, tool]));
      const status = await callTool(client, "juiced_status", {}, current.revision, false, signal);
      const system: ChatMessage = { role: "system", content: systemPrompt(model.id, current.permission, status.ok ? status.value as Record<string, unknown> : null) };
      let finished = false;
      for (let step = 0; step < MAX_STEPS && !finished; step += 1) {
        const reply = add({ kind: "assistant", text: "", reasoning: "", streaming: true });
        const clock = thinkingClock();
        const answer = await streamReply(client, model.id, [system, ...messages.current], current.tools, replyTokens(model), (state) => patch(reply, { text: state.content, reasoning: state.reasoning, ...clock.observe(state.reasoning, !!state.content || state.toolCalls.length > 0) }), signal);
        const calls = answer.toolCalls;
        const thoughtMs = clock.finish();
        if (!answer.content && !answer.reasoning && calls.length) setItems((list) => list.filter((item) => item.id !== reply));
        else patch(reply, { text: answer.content || (calls.length ? "" : "The model returned no text."), reasoning: answer.reasoning, thoughtMs, streaming: false });
        messages.current = [...messages.current, { role: "assistant", content: answer.content || null, ...(calls.length ? { tool_calls: calls.map((call) => ({ id: call.id, type: "function" as const, function: { name: call.name, arguments: call.arguments || "{}" } })) } : {}) }];
        if (!calls.length) { finished = true; break; }
        let changed = false;
        for (const call of calls) {
          const tool = byName.get(call.name);
          const args = parseArguments(call.arguments);
          let result: string;
          if (!args) {
            const reason = "The tool arguments are not a JSON object.";
            add({ kind: "tool", name: call.name, label: call.name.replace(/^juiced_/, "").replaceAll("_", " "), change: false, step: "failed", detail: reason });
            result = JSON.stringify({ error: { message: reason } });
          } else if (readOnly(tool)) {
            const id = add({ kind: "tool", name: call.name, label: describe(call.name, args), change: false, step: "running" });
            const value = await callTool(client, call.name, args, current.revision, false, signal);
            patch(id, { step: value.ok ? "done" : "failed", detail: value.ok ? undefined : outcome(call.name, args, value) });
            result = value.text;
          } else {
            const confirm = tool?.requires_confirmation === true;
            const id = add({ kind: "tool", name: call.name, label: confirm ? describe(call.name, args) : action(call.name, args), change: true, step: confirm ? "pending" : "running" });
            const approved = !confirm || await approval(id);
            if (signal.aborted) throw new DOMException("Stopped.", "AbortError");
            if (!approved) {
              patch(id, { step: "skipped", detail: "Skipped." });
              result = JSON.stringify({ skipped: true, message: "The operator skipped this action. Do not retry it unless asked." });
            } else {
              patch(id, { step: "running" });
              const value = await callTool(client, call.name, args, current.revision, confirm, signal);
              patch(id, { step: value.ok ? "done" : "failed", detail: outcome(call.name, args, value) });
              result = value.text;
              changed ||= value.ok;
            }
          }
          messages.current = [...messages.current, { role: "tool", tool_call_id: call.id, content: result }];
        }
        if (changed) onChanged();
      }
      if (!finished) add({ kind: "note", text: `Stopped after ${MAX_STEPS} steps. Send another message to continue.` });
    } catch (error) {
      setItems((list) => list.map((item) => item.kind === "assistant" && item.streaming ? { ...item, streaming: false } : item.kind === "tool" && (item.step === "pending" || item.step === "running") ? { ...item, step: "skipped", detail: "Not run." } : item));
      const settingsChanged = error instanceof ApiError && stopCodes.has(error.code);
      add({ kind: "note", text: signal.aborted ? "Stopped. Changes that already finished stay in place." : settingsChanged ? `${error.message} Changes that already finished stay in place.` : error instanceof Error ? error.message : "The request failed." });
      if (settingsChanged) void client.request<BaristaSettings>("juiced/barista").then(onSettings, () => undefined);
    } finally {
      for (const resolve of approvals.current.values()) resolve(false);
      if (request.current === controller) { request.current = null; setBusy(false); setTask(""); }
    }
  }
  function reset() { messages.current = []; setItems([]); }

  const blocked = !online ? "The service is offline."
    : model?.policy === "disabled" ? `${model.id} is Disabled. Set it to On demand, or pick another model in settings.` : "";
  const activity = waiting ? "Waiting for your approval" : busy ? `Working on "${task.length > 48 ? `${task.slice(0, 47)}…` : task}"` : "Ready";
  return <aside className={`barista-panel ${docked ? "docked" : "overlay"}`} aria-label="Barista" hidden={!open}>
    <div className="barista-head">
      <h2>Barista</h2>
      <HelpTip label="Barista">Barista reads and changes Juiced through the same checks as the console. It cannot change its own permissions. Turning it off stops its current request. Changes that already finished stay in place.</HelpTip>
      <span className="barista-spacer" />
      {settings.enabled && !setup && items.length > 0 && !busy && <Button variant="ghost" onClick={reset}>Clear</Button>}
      {layout.dockable && <Button variant="ghost" className="barista-icon" aria-label="Pin Barista" title={layout.pinned ? "Unpin, so Barista opens over the page" : "Pin beside the page"} aria-pressed={layout.pinned} onClick={() => layout.onPin(!layout.pinned)}><Icon name="pin" size={16} /></Button>}
      {settings.enabled && !gone && <Button variant="ghost" className="barista-icon" aria-label="Barista settings" aria-pressed={showSettings} onClick={() => setShowSettings(!showSettings)}><Icon name="settings" size={16} /></Button>}
      <Button variant="ghost" className="barista-icon" aria-label="Close Barista" onClick={onClose}><Icon name="close" /></Button>
    </div>
    {settings.enabled && !setup && <p className="barista-meta" aria-label="Barista status"><span className="barista-model">{settings.model}</span><Badge>{permissionNames[settings.permission]}</Badge>{vision && <Badge>Images</Badge>}<span className={`barista-task ${busy ? "active" : ""}`}>{activity}</span></p>}
    {setup ? <>
      {gone && <p className="barista-blocked" role="status">{settings.model ?? "The Barista model"} is no longer registered. Pick another model.</p>}
      <Setup key={`${settings.revision}:${settings.enabled}`} settings={settings} models={models} client={client} online={online} onSaved={(next) => { onSettings(next); setShowSettings(false); }} onDisable={stop} onChanged={onChanged} />
    </> : <>
      <div ref={thread} className="barista-thread" aria-live="polite" aria-busy={busy}>
        {items.length === 0 ? <Empty title="Ask Barista">Try "Which models fit this machine?" or "Load a small chat model". This chat clears when you close the tab.</Empty> : items.map((item) => {
          if (item.kind === "user") return <article key={item.id} className="chat-turn user" aria-label="You"><UserBody text={item.text} images={item.images} /></article>;
          if (item.kind === "assistant") return <article key={item.id} className="chat-turn assistant" aria-label="Barista reply"><AssistantBody text={item.text} reasoning={item.reasoning} streaming={item.streaming} since={item.since} thoughtMs={item.thoughtMs} /></article>;
          if (item.kind === "note") return <p key={item.id} className="barista-note" role="status">{item.text}</p>;
          if (!item.change) return <div key={item.id} className={`barista-step ${item.step}`} aria-label="Tool step"><StepIcon step={item.step} /><span className="barista-step-label">{item.label}</span>{item.detail && <span className="barista-detail">{item.detail}</span>}</div>;
          return <div key={item.id} className={`barista-change ${item.step}`} role="group" aria-label="Change">
            <p className="barista-question"><StepIcon step={item.step} />{item.label}</p>
            {item.step === "pending" ? <div className="action-row"><Button variant="primary" onClick={() => decide(item.id, true)}>Approve</Button><Button onClick={() => decide(item.id, false)}>Skip</Button></div>
              : <p className="barista-result"><Badge tone={item.step === "done" ? "good" : item.step === "failed" ? "bad" : "quiet"}>{item.step === "done" ? "Done" : item.step === "failed" ? "Failed" : item.step === "running" ? "Running" : "Skipped"}</Badge>{item.detail && <span>{item.detail}</span>}</p>}
          </div>;
        })}
      </div>
      {blocked && <p className="barista-blocked" role="status">{blocked}</p>}
      <Composer label="Message Barista" placeholder={vision ? "Ask about Juiced, or add a screenshot" : "Ask about models, engines or settings"} value={input} onChange={setInput} onSubmit={() => void send()} busy={busy} onStop={stop} canSend={(!!input.trim() || attachments.list.length > 0) && !!model && !blocked} attachments={attachments} vision={vision} inputRef={field} />
    </>}
  </aside>;
}

export function BaristaInvite({ onEnable, onDismiss }: { onEnable: () => void; onDismiss: () => void }) {
  return <div className="barista-invite" role="group" aria-label="Barista invitation">
    <Button variant="primary" onClick={onEnable}><Icon name="chat" size={14} />Enable Barista</Button>
    <Button variant="ghost" onClick={onDismiss}>Not now</Button>
  </div>;
}
