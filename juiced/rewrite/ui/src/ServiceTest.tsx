import { useEffect, useRef, useState } from "react";
import { chatTestTokenLimit, type Client } from "./api";
import { eligible, type Model } from "./domain";
import { Button, Empty } from "./ui";
import { TranscriptionTest } from "./TranscriptionTest";
import { CompareTest, EmbeddingsTest } from "./TestPanels";

export type TestService = "chat" | "compare" | "embeddings" | "transcription";
export function ServiceTest({ operation, ...props }: { models: Model[]; client: Client; online: boolean; operation: TestService }) {
  return <section className={`service-tests ${operation === "compare" ? "wide" : ""}`}>{operation === "chat" ? <ChatTest {...props} /> : operation === "compare" ? <CompareTest {...props} /> : operation === "embeddings" ? <EmbeddingsTest {...props} /> : <TranscriptionTest {...props} />}</section>;
}

type Turn = { role: "user" | "assistant"; content: string; reasoning?: string; tokens?: number | null; rate?: number | null; error?: string; streaming?: boolean };

function inline(text: string) {
  return text.split(/(`[^`\n]+`|\*\*[^*\n]+\*\*)/).map((piece, index) => piece.startsWith("`") && piece.endsWith("`") && piece.length > 2 ? <code key={index}>{piece.slice(1, -1)}</code> : piece.startsWith("**") && piece.endsWith("**") && piece.length > 4 ? <strong key={index}>{piece.slice(2, -2)}</strong> : piece);
}
function Message({ text }: { text: string }) {
  const parts = text.split(/```[^\n]*\n?/);
  return <div className="message-text">{parts.map((part, index) => index % 2 ? <pre key={index} className="code-block">{part.replace(/\n$/, "")}</pre> : <p key={index}>{inline(part)}</p>)}</div>;
}

function ChatTest({ models, client, online }: { models: Model[]; client: Client; online: boolean }) {
  const ready = models.filter(eligible);
  const [id, setId] = useState("");
  const [prompt, setPrompt] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [busy, setBusy] = useState(false);
  const request = useRef<AbortController | null>(null);
  const thread = useRef<HTMLDivElement>(null);
  const selected = ready.find((model) => model.id === id) ?? (!id ? ready.find((model) => model.loaded) ?? (ready.length === 1 ? ready[0] : undefined) : undefined);
  useEffect(() => () => { request.current?.abort(); }, [client]);
  useEffect(() => { thread.current?.scrollTo({ top: thread.current.scrollHeight }); }, [turns]);
  async function send() {
    const text = prompt.trim();
    if (!selected || !text || busy) return;
    const history = [...turns.filter((turn) => !turn.error).map(({ role, content }) => ({ role, content })), { role: "user", content: text }];
    setTurns((current) => [...current, { role: "user", content: text }, { role: "assistant", content: "", streaming: true }]);
    setPrompt(""); setBusy(true);
    const controller = new AbortController(); request.current = controller;
    const update = (patch: Partial<Turn>) => setTurns((current) => current.map((turn, index) => index === current.length - 1 ? { ...turn, ...patch } : turn));
    try {
      const final = await client.streamChat(selected.id, history, chatTestTokenLimit(selected), (state) => update({ content: state.content, reasoning: state.reasoning }), controller.signal);
      update({ content: final.content || "The model returned no text.", reasoning: final.reasoning, tokens: final.outputTokens, rate: final.decodeRate, streaming: false });
    } catch (error) {
      update({ streaming: false, error: controller.signal.aborted ? "Stopped." : error instanceof Error ? error.message : "The request failed." });
    } finally {
      if (request.current === controller) { request.current = null; setBusy(false); }
    }
  }
  if (ready.length === 0) return <Empty title="No chat models available">Set a chat model to Hot or On demand in Loading policy.</Empty>;
  return <section className="chat" aria-label="Chat test">
    <div className="toolbar">
      <select aria-label="Model" value={selected?.id ?? ""} disabled={busy} onChange={(event) => setId(event.target.value)}><option value="">Choose a model…</option>{ready.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}</select>
      {selected && <span className="muted chat-meta">{selected.loaded ? "Loaded" : "Loads on first message"} · up to {chatTestTokenLimit(selected)} tokens per reply</span>}
      <Button disabled={busy || turns.length === 0} onClick={() => setTurns([])}>New chat</Button>
    </div>
    <div ref={thread} className="chat-thread" aria-live="polite" aria-busy={busy}>
      {turns.length === 0 ? <p className="muted chat-empty">Messages stay in this tab and aren't saved.</p> : turns.map((turn, index) => <article key={index} className={`chat-turn ${turn.role}`} aria-label={turn.role === "user" ? "You" : "Model reply"}>
        {turn.reasoning && <div className="chat-reasoning"><Message text={turn.reasoning} /></div>}
        {turn.content ? <Message text={turn.content} /> : turn.streaming && <p className="muted">Thinking…</p>}
        {turn.error && <p className="inline-error" role="alert">{turn.error}</p>}
        {turn.role === "assistant" && !turn.streaming && !turn.error && <span className="chat-stats">{turn.tokens ?? "—"} tokens{turn.rate ? ` · ${Math.round(turn.rate)} tok/s` : ""}</span>}
      </article>)}
    </div>
    <form className="composer" onSubmit={(event) => { event.preventDefault(); void send(); }}>
      <textarea aria-label="Prompt" rows={2} value={prompt} placeholder={selected ? "Message the model" : "Choose a model first"} disabled={!selected} onChange={(event) => setPrompt(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} />
      {busy ? <Button variant="danger" onClick={() => request.current?.abort()}>Stop</Button> : <Button variant="primary" type="submit" disabled={!selected || !prompt.trim() || !online}>Send</Button>}
    </form>
  </section>;
}
