import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import { MAX_STEPS, callTool, describe, managementTools, outcome, parseArguments, readOnly, streamReply, systemPrompt, type ChatMessage, type McpTool } from "./assistantTools";
import { eligible, type Model } from "./domain";
import { Message } from "./ServiceTest";
import { Badge, Button, Empty, HelpTip, Icon } from "./ui";
import "./assistant.css";

type Step = "running" | "pending" | "done" | "failed" | "skipped";
type Item =
  | { id: number; kind: "user"; text: string }
  | { id: number; kind: "assistant"; text: string; reasoning: string; streaming: boolean }
  | { id: number; kind: "tool"; name: string; label: string; change: boolean; step: Step; detail?: string }
  | { id: number; kind: "note"; text: string };
type NewItem = Item extends infer Entry ? Entry extends Item ? Omit<Entry, "id"> : never : never;
type Saved = { items: Item[]; messages: ChatMessage[] };

const storageKey = "juiced.assistant.v1";
const replyTokens = (model: Model) => Math.max(1, Math.min(4096, model.request_limits?.output_tokens ?? 4096));
const modelKey = "juiced.assistant.model";
function readSession<T>(key: string, fallback: T): T {
  try { const text = sessionStorage.getItem(key); return text ? JSON.parse(text) as T : fallback; } catch { return fallback; }
}
function writeSession(key: string, value: unknown) {
  try { sessionStorage.setItem(key, JSON.stringify(value)); } catch { /* Session storage can be unavailable or full. */ }
}
function restore(): Saved {
  const saved = readSession<Saved>(storageKey, { items: [], messages: [] });
  if (!Array.isArray(saved.items) || !Array.isArray(saved.messages)) return { items: [], messages: [] };
  return { messages: saved.messages, items: saved.items.map((item) => item.kind === "tool" && (item.step === "pending" || item.step === "running") ? { ...item, step: "skipped", detail: "Not run." } : item.kind === "assistant" ? { ...item, streaming: false } : item) };
}

export function Assistant({ open, onClose, models, client, online, onChanged }: { open: boolean; onClose: () => void; models: Model[]; client: Client; online: boolean; onChanged: () => void }) {
  const [saved] = useState(restore);
  const [items, setItems] = useState<Item[]>(saved.items);
  const messages = useRef<ChatMessage[]>(saved.messages);
  const [modelId, setModelId] = useState(() => readSession(modelKey, ""));
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [tools, setTools] = useState<McpTool[] | null>(null);
  const [toolsError, setToolsError] = useState("");
  const request = useRef<AbortController | null>(null);
  const approvals = useRef(new Map<number, (approved: boolean) => void>());
  const nextId = useRef(Math.max(0, ...saved.items.map((item) => item.id)) + 1);
  const thread = useRef<HTMLDivElement>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const chat = models.filter(eligible).sort((left, right) => Number(right.loaded) - Number(left.loaded) || left.id.localeCompare(right.id));
  const model = chat.find((entry) => entry.id === modelId) ?? chat.find((entry) => entry.loaded) ?? chat[0];

  useEffect(() => { writeSession(storageKey, { items, messages: messages.current }); thread.current?.scrollTo({ top: thread.current.scrollHeight }); }, [items]);
  useEffect(() => () => { request.current?.abort(); for (const resolve of approvals.current.values()) resolve(false); }, [client]);
  useEffect(() => {
    if (!open) return;
    field.current?.focus();
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape" && !document.querySelector("dialog[open], [popover]:popover-open")) onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);
  useEffect(() => {
    if (!open || !online) return;
    const controller = new AbortController();
    client.mcp<{ tools?: McpTool[] }>("tools/list", {}, controller.signal)
      .then((result) => { setTools(managementTools(result.tools ?? [])); setToolsError(""); })
      .catch((error) => { if (!controller.signal.aborted) { setTools(null); setToolsError(error instanceof Error ? error.message : "The MCP gateway is unavailable."); } });
    return () => controller.abort();
  }, [open, online, client]);

  const add = (item: NewItem) => { const id = nextId.current++; setItems((current) => [...current, { ...item, id } as Item]); return id; };
  const patch = (id: number, change: Partial<Item>) => setItems((current) => current.map((item) => item.id === id ? { ...item, ...change } as Item : item));
  const approval = (id: number) => new Promise<boolean>((resolve) => approvals.current.set(id, (approved) => { approvals.current.delete(id); resolve(approved); }));
  const decide = (id: number, approved: boolean) => approvals.current.get(id)?.(approved);

  async function send() {
    const text = input.trim();
    if (!text || busy || !model || !tools?.length) return;
    setInput(""); setBusy(true);
    const controller = new AbortController(); request.current = controller;
    add({ kind: "user", text });
    messages.current = [...messages.current, { role: "user", content: text }];
    const byName = new Map(tools.map((tool) => [tool.name, tool]));
    try {
      const status = await callTool(client, "juiced_status", {}, controller.signal);
      const system: ChatMessage = { role: "system", content: systemPrompt(status.ok ? status.value as Record<string, unknown> : null) };
      let finished = false;
      for (let step = 0; step < MAX_STEPS && !finished; step += 1) {
        const reply = add({ kind: "assistant", text: "", reasoning: "", streaming: true });
        const answer = await streamReply(client, model.id, [system, ...messages.current], tools, replyTokens(model), (state) => patch(reply, { text: state.content, reasoning: state.reasoning }), controller.signal);
        const calls = answer.toolCalls;
        if (!answer.content && !answer.reasoning && calls.length) setItems((current) => current.filter((item) => item.id !== reply));
        else patch(reply, { text: answer.content || (calls.length ? "" : "The model returned no text."), reasoning: answer.reasoning, streaming: false });
        messages.current = [...messages.current, { role: "assistant", content: answer.content || null, ...(calls.length ? { tool_calls: calls.map((call) => ({ id: call.id, type: "function" as const, function: { name: call.name, arguments: call.arguments || "{}" } })) } : {}) }];
        if (!calls.length) { finished = true; break; }
        let changed = false;
        for (const call of calls) {
          const tool = byName.get(call.name);
          const args = parseArguments(call.arguments);
          let result: string;
          if (!tool || !args) {
            const reason = !tool ? `Unknown tool ${call.name}.` : "The tool arguments are not a JSON object.";
            add({ kind: "tool", name: call.name, label: call.name.replace(/^juiced_/, "").replaceAll("_", " "), change: false, step: "failed", detail: reason });
            result = JSON.stringify({ error: { message: reason } });
          } else if (readOnly(tool)) {
            const id = add({ kind: "tool", name: call.name, label: describe(call.name, args), change: false, step: "running" });
            const outcomeValue = await callTool(client, call.name, args, controller.signal);
            patch(id, { step: outcomeValue.ok ? "done" : "failed", detail: outcomeValue.ok ? undefined : outcome(call.name, args, outcomeValue) });
            result = outcomeValue.text;
          } else {
            const id = add({ kind: "tool", name: call.name, label: describe(call.name, args), change: true, step: "pending" });
            const approved = await approval(id);
            if (controller.signal.aborted) throw new DOMException("Stopped.", "AbortError");
            if (!approved) {
              patch(id, { step: "skipped", detail: "Skipped." });
              result = JSON.stringify({ skipped: true, message: "The operator skipped this action. Do not retry it unless asked." });
            } else {
              patch(id, { step: "running" });
              const outcomeValue = await callTool(client, call.name, args, controller.signal);
              patch(id, { step: outcomeValue.ok ? "done" : "failed", detail: outcome(call.name, args, outcomeValue) });
              result = outcomeValue.text;
              changed = true;
            }
          }
          messages.current = [...messages.current, { role: "tool", tool_call_id: call.id, content: result }];
        }
        if (changed) onChanged();
      }
      if (!finished) add({ kind: "note", text: `Stopped after ${MAX_STEPS} steps. Send another message to continue.` });
    } catch (error) {
      setItems((current) => current.map((item) => item.kind === "assistant" && item.streaming ? { ...item, streaming: false } : item.kind === "tool" && (item.step === "pending" || item.step === "running") ? { ...item, step: "skipped", detail: "Not run." } : item));
      add({ kind: "note", text: controller.signal.aborted ? "Stopped." : error instanceof Error ? error.message : "The request failed." });
    } finally {
      for (const resolve of approvals.current.values()) resolve(false);
      if (request.current === controller) { request.current = null; setBusy(false); }
    }
  }
  function stop() { request.current?.abort(); for (const resolve of approvals.current.values()) resolve(false); }
  function reset() { messages.current = []; setItems([]); }

  const blocked = !online ? "The service is offline." : toolsError ? toolsError : tools && !tools.length ? "Connect with an operator key so the assistant can manage Juiced." : !model ? "Load a chat model first. Set one to Hot or On demand in Loading policy." : "";
  return <aside className="assistant-drawer" aria-label="Assistant" hidden={!open}>
    <div className="assistant-head">
      <h2>Assistant</h2>
      <HelpTip label="the assistant">The model you pick reads Juiced through its management tools. It asks before it changes anything. Models whose chat template supports tool calls work best.</HelpTip>
      <span className="assistant-spacer" />
      {items.length > 0 && !busy && <Button variant="ghost" onClick={reset}>New chat</Button>}
      <Button variant="ghost" aria-label="Close assistant" onClick={onClose}><Icon name="close" /></Button>
    </div>
    <div className="assistant-model">
      <select aria-label="Assistant model" value={model?.id ?? ""} disabled={busy || !chat.length} onChange={(event) => { setModelId(event.target.value); writeSession(modelKey, event.target.value); }}>
        {!chat.length && <option value="">No chat models</option>}
        {chat.map((entry) => <option key={entry.id} value={entry.id}>{entry.id}{entry.loaded ? "" : " (loads on first use)"}</option>)}
      </select>
    </div>
    <div ref={thread} className="assistant-thread" aria-live="polite" aria-busy={busy}>
      {items.length === 0 ? <Empty title="Ask Juiced's assistant">Try "Which models fit this machine?" or "Load a small chat model". Messages stay in this browser session.</Empty> : items.map((item) => {
        if (item.kind === "user") return <article key={item.id} className="chat-turn user" aria-label="You"><Message text={item.text} /></article>;
        if (item.kind === "assistant") return <article key={item.id} className="chat-turn assistant" aria-label="Assistant reply">
          {item.reasoning && <div className="chat-reasoning"><Message text={item.reasoning} /></div>}
          {item.text ? <Message text={item.text} /> : item.streaming && <p className="muted">Thinking…</p>}
        </article>;
        if (item.kind === "note") return <p key={item.id} className="assistant-note" role="status">{item.text}</p>;
        if (!item.change) return <div key={item.id} className={`assistant-step ${item.step}`} aria-label="Tool step"><span className={`status-dot ${item.step === "done" ? "good" : item.step === "failed" ? "bad" : "warning"}`} /><span>{item.label}{item.step === "running" ? "…" : ""}</span>{item.detail && <span className="assistant-detail">{item.detail}</span>}</div>;
        return <div key={item.id} className={`assistant-approval ${item.step}`} role="group" aria-label="Proposed change">
          <p className="assistant-question">{item.label}</p>
          {item.step === "pending" ? <div className="action-row"><Button variant="primary" onClick={() => decide(item.id, true)}>Approve</Button><Button onClick={() => decide(item.id, false)}>Skip</Button></div>
            : <p className="assistant-result"><Badge tone={item.step === "done" ? "good" : item.step === "failed" ? "bad" : "quiet"}>{item.step === "done" ? "Done" : item.step === "failed" ? "Failed" : item.step === "running" ? "Running" : "Skipped"}</Badge>{item.detail && <span>{item.detail}</span>}</p>}
        </div>;
      })}
    </div>
    {blocked && <p className="assistant-blocked" role="status">{blocked}</p>}
    <form className="composer" onSubmit={(event) => { event.preventDefault(); void send(); }}>
      <textarea ref={field} aria-label="Message the assistant" rows={2} value={input} placeholder="Ask about models, engines or settings" onChange={(event) => setInput(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} />
      {busy ? <Button variant="danger" onClick={stop}>Stop</Button> : <Button variant="primary" type="submit" disabled={!input.trim() || !!blocked || !tools}>Send</Button>}
    </form>
  </aside>;
}
