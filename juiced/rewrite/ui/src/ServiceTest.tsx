import { useEffect, useRef, useState } from "react";
import { chatTestTokenLimit, type Client } from "./api";
import { eligible, operationEligible, type Model } from "./domain";
import { speechModels } from "./speech";
import { Badge, Button, Empty } from "./ui";
import { TranscriptionTest } from "./TranscriptionTest";
import { CompareTest, EmbeddingsTest } from "./TestPanels";
import { ImagesTest, SpeakTest } from "./MediaTests";

export type TestMode = "try" | "compare";
type Kind = "chat" | "embeddings" | "transcription" | "speech" | "images";
const kinds: [Kind, string, (model: Model) => boolean][] = [
  ["chat", "Chat", (model) => eligible(model)],
  ["embeddings", "Embeddings", (model) => operationEligible(model, "embeddings")],
  ["transcription", "Speech to text", (model) => speechModels([model]).length > 0],
  ["speech", "Text to speech", (model) => operationEligible(model, "audio/speech")],
  ["images", "Images", (model) => operationEligible(model, "images/generations")],
];
function readLocal(key: string) { try { return localStorage.getItem(key) ?? ""; } catch { return ""; } }
function writeLocal(key: string, value: string) { try { localStorage.setItem(key, value); } catch { /* Storage can be unavailable in private sessions. */ } }

export function ServiceTest({ mode, models, client, online }: { models: Model[]; client: Client; online: boolean; mode: TestMode }) {
  if (mode === "compare") return <section className="service-tests wide"><CompareTest models={models} client={client} online={online} /></section>;
  return <TryModel models={models} client={client} online={online} />;
}

function TryModel({ models, client, online }: { models: Model[]; client: Client; online: boolean }) {
  const [id, setId] = useState(() => readLocal("juiced.rewrite.testModel"));
  const seen = new Set<string>();
  const groups = kinds.map(([kind, label, accepts]) => ({ kind, label, list: models.filter((model) => accepts(model) && !seen.has(model.id) && (seen.add(model.id), true)) })).filter((group) => group.list.length > 0);
  const choices = groups.flatMap((group) => group.list.map((model) => ({ model, kind: group.kind, label: group.label })));
  const current = choices.find((choice) => choice.model.id === id) ?? choices.find((choice) => choice.kind === "chat" && choice.model.loaded) ?? choices[0];
  if (!current) return <section className="service-tests"><Empty title="Nothing to try yet">Set a model to Hot or On demand in Loading policy, then pick it here.</Empty></section>;
  const props = { model: current.model, client, online };
  return <section className="service-tests" aria-label="Try a model">
    <div className="toolbar test-toolbar">
      <select aria-label="Model" value={current.model.id} onChange={(event) => { setId(event.target.value); writeLocal("juiced.rewrite.testModel", event.target.value); }}>
        {groups.map((group) => <optgroup key={group.kind} label={group.label}>{group.list.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}</optgroup>)}
      </select>
      <Badge>{current.label}</Badge>
      <span className="muted">{current.model.loaded ? "Loaded" : "Loads on first use"}{current.kind === "chat" ? ` · up to ${chatTestTokenLimit(current.model)} tokens per reply` : ""}</span>
    </div>
    {current.kind === "chat" ? <ChatTest {...props} />
      : current.kind === "embeddings" ? <EmbeddingsTest {...props} />
      : current.kind === "transcription" ? <TranscriptionTest {...props} />
      : current.kind === "speech" ? <SpeakTest {...props} />
      : <ImagesTest {...props} />}
  </section>;
}

type Turn = { role: "user" | "assistant"; content: string; reasoning?: string; tokens?: number | null; rate?: number | null; error?: string; streaming?: boolean };

function inline(text: string) {
  return text.split(/(`[^`\n]+`|\*\*[^*\n]+\*\*)/).map((piece, index) => piece.startsWith("`") && piece.endsWith("`") && piece.length > 2 ? <code key={index}>{piece.slice(1, -1)}</code> : piece.startsWith("**") && piece.endsWith("**") && piece.length > 4 ? <strong key={index}>{piece.slice(2, -2)}</strong> : piece);
}
function Message({ text }: { text: string }) {
  const parts = text.split(/```[^\n]*\n?/);
  return <div className="message-text">{parts.map((part, index) => index % 2 ? <pre key={index} className="code-block">{part.replace(/\n$/, "")}</pre> : <p key={index}>{inline(part)}</p>)}</div>;
}

function ChatTest({ model: selected, client, online }: { model: Model; client: Client; online: boolean }) {
  const [prompt, setPrompt] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [busy, setBusy] = useState(false);
  const request = useRef<AbortController | null>(null);
  const thread = useRef<HTMLDivElement>(null);
  useEffect(() => () => { request.current?.abort(); }, [client]);
  useEffect(() => { thread.current?.scrollTo({ top: thread.current.scrollHeight }); }, [turns]);
  async function send() {
    const text = prompt.trim();
    if (!text || busy) return;
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
  return <section className="chat" aria-label="Chat test">
    <div ref={thread} className="chat-thread" aria-live="polite" aria-busy={busy}>
      {turns.length > 0 && !busy && <Button variant="ghost" className="new-chat" onClick={() => setTurns([])}>New chat</Button>}
      {turns.length === 0 ? <p className="muted chat-empty">Messages stay in this tab and aren't saved.</p> : turns.map((turn, index) => <article key={index} className={`chat-turn ${turn.role}`} aria-label={turn.role === "user" ? "You" : "Model reply"}>
        {turn.reasoning && <div className="chat-reasoning"><Message text={turn.reasoning} /></div>}
        {turn.content ? <Message text={turn.content} /> : turn.streaming && <p className="muted">Thinking…</p>}
        {turn.error && <p className="inline-error" role="alert">{turn.error}</p>}
        {turn.role === "assistant" && !turn.streaming && !turn.error && <span className="chat-stats">{turn.tokens ?? "—"} tokens{turn.rate ? ` · ${Math.round(turn.rate)} tok/s` : ""}</span>}
      </article>)}
    </div>
    <form className="composer" onSubmit={(event) => { event.preventDefault(); void send(); }}>
      <textarea aria-label="Prompt" rows={2} value={prompt} placeholder="Message the model" onChange={(event) => setPrompt(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} />
      {busy ? <Button variant="danger" onClick={() => request.current?.abort()}>Stop</Button> : <Button variant="primary" type="submit" disabled={!prompt.trim() || !online}>Send</Button>}
    </form>
  </section>;
}
