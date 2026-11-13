import { useEffect, useRef, useState } from "react";
import { chatTestTokenLimit, chatTokenCeiling, type Client } from "./api";
import { eligible, operationEligible, type Model } from "./domain";
import { speechModels } from "./speech";
import { Badge, Button, Empty, HelpTip, Switch } from "./ui";
import { AssistantBody, Composer, UserBody, thinkingClock, useAttachments } from "./ChatParts";
import { withImages } from "./chatImages";
import { TranscriptionTest } from "./TranscriptionTest";
import { CompareTest, EmbeddingsTest } from "./TestPanels";
import { ImagesTest, SpeakTest } from "./MediaTests";
import { AudioGenerationTest, Model3dTest } from "./GenerationTests";

export type TestMode = "try" | "compare";
type Kind = "chat" | "embeddings" | "transcription" | "speech" | "images" | "audio" | "3d";
const kinds: [Kind, string, (model: Model) => boolean][] = [
  ["chat", "Chat", (model) => eligible(model)],
  ["embeddings", "Embeddings", (model) => operationEligible(model, "embeddings")],
  ["transcription", "Speech to text", (model) => speechModels([model]).length > 0],
  ["speech", "Text to speech", (model) => operationEligible(model, "audio/speech")],
  ["images", "Images", (model) => operationEligible(model, "images/generations")],
  ["audio", "Audio", (model) => operationEligible(model, "audio/generations")],
  ["3d", "3D", (model) => operationEligible(model, "3d/generations")],
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
  const [maxTokens, setMaxTokens] = useState("");
  const [thinking, setThinking] = useState(true);
  if (!current) return <section className="service-tests"><Empty title="Nothing to try yet">Set a model to Hot or On demand in Loading policy, then pick it here.</Empty></section>;
  const props = { model: current.model, client, online };
  return <section className="service-tests" aria-label="Try a model">
    <div className="toolbar test-toolbar">
      <select aria-label="Model" value={current.model.id} onChange={(event) => { setId(event.target.value); writeLocal("juiced.rewrite.testModel", event.target.value); }}>
        {groups.map((group) => <optgroup key={group.kind} label={group.label}>{group.list.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}</optgroup>)}
      </select>
      <Badge>{current.label}</Badge>
      <span className="muted">{current.model.loaded ? "Loaded" : "Loads on first use"}{current.kind === "chat" && current.model.vision ? " · accepts images" : ""}</span>
      {current.kind === "chat" && <span className="chat-options">
        <label className="inline-field">Max tokens <input className="short-input" inputMode="numeric" aria-label="Max tokens per reply" value={maxTokens} placeholder={String(chatTestTokenLimit(current.model))} onChange={(event) => setMaxTokens(event.target.value.replace(/\D/g, ""))} /></label>
        <Switch checked={thinking} label="Thinking" onChange={setThinking}>Thinking</Switch>
        <HelpTip label="Chat test options">Max tokens caps each reply, up to {chatTokenCeiling(current.model).toLocaleString()}. Turning thinking off asks a reasoning model to answer directly; other models ignore it.</HelpTip>
      </span>}
    </div>
    {current.kind === "chat" ? <ChatTest {...props} maxTokens={Math.min(chatTokenCeiling(current.model), Math.max(1, Number(maxTokens) || chatTestTokenLimit(current.model)))} thinking={thinking ? undefined : false} />
      : current.kind === "embeddings" ? <EmbeddingsTest {...props} />
      : current.kind === "transcription" ? <TranscriptionTest {...props} />
      : current.kind === "speech" ? <SpeakTest {...props} />
      : current.kind === "audio" ? <AudioGenerationTest {...props} />
      : current.kind === "3d" ? <Model3dTest {...props} />
      : <ImagesTest {...props} />}
  </section>;
}

type Turn = { role: "user" | "assistant"; content: string; images?: string[]; reasoning?: string; since?: number; thoughtMs?: number; tokens?: number | null; rate?: number | null; error?: string; streaming?: boolean };

function ChatTest({ model: selected, client, online, maxTokens, thinking }: { model: Model; client: Client; online: boolean; maxTokens: number; thinking?: boolean }) {
  const [prompt, setPrompt] = useState("");
  const [turns, setTurns] = useState<Turn[]>([]);
  const [busy, setBusy] = useState(false);
  const vision = selected.vision === true;
  const attachments = useAttachments(vision);
  const request = useRef<AbortController | null>(null);
  const thread = useRef<HTMLDivElement>(null);
  useEffect(() => () => { request.current?.abort(); }, [client]);
  useEffect(() => { thread.current?.scrollTo({ top: thread.current.scrollHeight }); }, [turns]);
  async function send() {
    const text = prompt.trim();
    const images = attachments.list;
    if ((!text && images.length === 0) || busy) return;
    const history = [...turns.filter((turn) => !turn.error).map(({ role, content, images }) => ({ role, content: role === "user" ? withImages(content, (images ?? []).map((url) => ({ url }))) : content })), { role: "user", content: withImages(text, images) }];
    setTurns((current) => [...current, { role: "user", content: text, images: images.map((image) => image.url) }, { role: "assistant", content: "", streaming: true }]);
    setPrompt(""); attachments.clear(); setBusy(true);
    const controller = new AbortController(); request.current = controller;
    const clock = thinkingClock();
    const update = (patch: Partial<Turn>) => setTurns((current) => current.map((turn, index) => index === current.length - 1 ? { ...turn, ...patch } : turn));
    try {
      const final = await client.streamChat(selected.id, history, maxTokens, (state) => update({ content: state.content, reasoning: state.reasoning, ...clock.observe(state.reasoning, !!state.content) }), controller.signal, thinking);
      update({ content: final.content || "The model returned no text.", reasoning: final.reasoning, thoughtMs: clock.finish(), tokens: final.outputTokens, rate: final.decodeRate, streaming: false });
    } catch (error) {
      update({ streaming: false, thoughtMs: clock.finish(), error: controller.signal.aborted ? "Stopped." : error instanceof Error ? error.message : "The request failed." });
    } finally {
      if (request.current === controller) { request.current = null; setBusy(false); }
    }
  }
  return <section className="chat" aria-label="Chat test">
    <div ref={thread} className="chat-thread" aria-live="polite" aria-busy={busy}>
      {turns.length > 0 && !busy && <Button variant="ghost" className="new-chat" onClick={() => setTurns([])}>New chat</Button>}
      {turns.length === 0 ? <p className="muted chat-empty">Messages stay in this tab and aren't saved.{vision ? " This model accepts images." : ""}</p> : turns.map((turn, index) => <article key={index} className={`chat-turn ${turn.role}`} aria-label={turn.role === "user" ? "You" : "Model reply"}>
        {turn.role === "user" ? <UserBody text={turn.content} images={turn.images} /> : <AssistantBody text={turn.content} reasoning={turn.reasoning ?? ""} streaming={!!turn.streaming} since={turn.since} thoughtMs={turn.thoughtMs} />}
        {turn.error && <p className="inline-error" role="alert">{turn.error}</p>}
        {turn.role === "assistant" && !turn.streaming && !turn.error && <span className="chat-stats">{turn.tokens ?? "—"} tokens{turn.rate ? ` · ${Math.round(turn.rate)} tok/s` : ""}</span>}
      </article>)}
    </div>
    <Composer label="Prompt" placeholder={vision ? "Message the model, or add images" : "Message the model"} value={prompt} onChange={setPrompt} onSubmit={() => void send()} busy={busy} onStop={() => request.current?.abort()} canSend={(!!prompt.trim() || attachments.list.length > 0) && online} attachments={attachments} vision={vision} />
  </section>;
}
