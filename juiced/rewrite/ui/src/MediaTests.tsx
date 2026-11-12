import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import { operationEligible, type Model } from "./domain";
import { Button, Empty } from "./ui";

type Props = { models: Model[]; client: Client; online: boolean };
const failure = (error: unknown, fallback: string) => error instanceof Error ? error.message : fallback;

function readLocal(key: string) { try { return localStorage.getItem(key) ?? ""; } catch { return ""; } }
function writeLocal(key: string, value: string) { try { localStorage.setItem(key, value); } catch { /* Storage can be unavailable in private sessions. */ } }

export function parseVoices(value: unknown): string[] {
  const data = value && typeof value === "object" ? (value as { data?: unknown }).data : undefined;
  const first = Array.isArray(data) ? data[0] as { voices?: unknown } | undefined : undefined;
  return Array.isArray(first?.voices) ? first.voices.filter((voice): voice is string => typeof voice === "string").slice(0, 512) : [];
}

function useModelChoice(ready: Model[]) {
  const [id, setId] = useState("");
  const selected = ready.find((model) => model.id === id) ?? (!id && ready.length === 1 ? ready[0] : undefined);
  return { id, setId, selected };
}

function useRequest(client: Client) {
  const controller = useRef<AbortController | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => () => controller.current?.abort(), [client]);
  async function run<T>(work: (signal: AbortSignal) => Promise<T>): Promise<T | undefined> {
    controller.current?.abort();
    const current = new AbortController(); controller.current = current;
    setBusy(true);
    try { return await work(current.signal); }
    finally { if (controller.current === current) { controller.current = null; setBusy(false); } }
  }
  const stop = () => { controller.current?.abort(); controller.current = null; setBusy(false); };
  return { busy, run, stop };
}

export function SpeakTest({ models, client, online }: Props) {
  const ready = models.filter((model) => operationEligible(model, "audio/speech"));
  const choice = useModelChoice(ready);
  const [voices, setVoices] = useState<{ model: string; list: string[]; error: string } | null>(null);
  const [voice, setVoice] = useState("");
  const [speed, setSpeed] = useState(1);
  const [format, setFormat] = useState<"mp3" | "wav">("mp3");
  const [text, setText] = useState("");
  const [audio, setAudio] = useState<{ url: string; seconds: number; size: number } | null>(null);
  const [error, setError] = useState("");
  const request = useRequest(client);
  const current = voices && voices.model === choice.selected?.id ? voices : null;
  const list = current?.list ?? [];
  const chosenVoice = list.includes(voice) ? voice : list[0] ?? "";
  useEffect(() => () => { if (audio) URL.revokeObjectURL(audio.url); }, [audio]);
  useEffect(() => {
    setAudio(null); setError("");
    const model = choice.selected?.id;
    if (!model) return;
    const controller = new AbortController();
    client.request(`juiced/voices?model=${encodeURIComponent(model)}`, undefined, controller.signal)
      .then((value) => setVoices({ model, list: parseVoices(value), error: "" }))
      .catch((caught) => { if (!controller.signal.aborted) setVoices({ model, list: [], error: failure(caught, "Voices unavailable.") }); });
    return () => controller.abort();
  }, [client, choice.selected?.id]);
  async function speak() {
    if (!choice.selected || !text.trim()) return;
    setError(""); setAudio(null);
    const started = performance.now();
    try {
      const blob = await request.run((signal) => client.binary("audio/speech", { model: choice.selected!.id, input: text.trim(), voice: chosenVoice || undefined, speed, response_format: format }, signal, 32 * 1024 * 1024));
      if (blob) setAudio({ url: URL.createObjectURL(blob), seconds: (performance.now() - started) / 1000, size: blob.size });
    } catch (caught) { if (!(caught instanceof DOMException && caught.name === "AbortError")) setError(failure(caught, "Speech failed.")); }
  }
  if (ready.length === 0) return <Empty title="No speech models available">Add a text-to-speech model such as Kokoro in Library, then set it to Hot or On demand.</Empty>;
  return <section className="test-panel" aria-label="Speech test">
    <div className="toolbar media-options">
      <select aria-label="Model" value={choice.selected?.id ?? ""} disabled={request.busy} onChange={(event) => choice.setId(event.target.value)}><option value="">Choose a model…</option>{ready.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}</select>
      <select aria-label="Voice" value={chosenVoice} disabled={request.busy || list.length === 0} onChange={(event) => setVoice(event.target.value)}>{list.length === 0 ? <option value="">{choice.selected ? current?.error ? "Voices unavailable" : current ? "Default voice" : "Loading voices…" : "Choose a model first"}</option> : list.map((item) => <option key={item} value={item}>{item}</option>)}</select>
      <label className="inline-field">Speed <input type="range" aria-label="Speed" min={0.5} max={2} step={0.05} value={speed} onChange={(event) => setSpeed(Number(event.target.value))} /><span className="numeric">{speed.toFixed(2)}×</span></label>
      <div className="segmented" role="radiogroup" aria-label="Audio format">{(["mp3", "wav"] as const).map((item) => <button key={item} role="radio" aria-checked={format === item} onClick={() => setFormat(item)}>{item.toUpperCase()}</button>)}</div>
    </div>
    <label htmlFor="speech-input">Text</label>
    <textarea id="speech-input" rows={4} value={text} maxLength={16384} placeholder="Type something to hear it spoken." onChange={(event) => setText(event.target.value)} />
    <div className="action-row">{request.busy ? <Button variant="danger" onClick={request.stop}>Stop</Button> : <Button variant="primary" disabled={!choice.selected || !text.trim() || !online} onClick={() => void speak()}>Speak</Button>}{audio && <span className="muted">Ready in {audio.seconds.toFixed(2)} s · {Math.round(audio.size / 1024)} KiB</span>}</div>
    {error && <p className="inline-error" role="alert">{error}</p>}
    {audio && <div className="audio-result"><audio controls autoPlay src={audio.url} aria-label="Generated speech" /><a className="button" href={audio.url} download={`speech.${format}`}>Download</a></div>}
  </section>;
}

const sizes = ["512x512", "768x768", "1024x1024", "512x768", "768x512", "1024x576", "576x1024"];
type Picture = { url: string; prompt: string; seconds: number; size: string; seed: number };

export function ImagesTest({ models, client, online }: Props) {
  const ready = models.filter((model) => operationEligible(model, "images/generations"));
  const choice = useModelChoice(ready);
  const sizeKey = `juiced.rewrite.imageSize.${choice.selected?.id ?? ""}`;
  const [size, setSize] = useState("512x512");
  const [prompt, setPrompt] = useState("");
  const [negative, setNegative] = useState("");
  const [steps, setSteps] = useState("");
  const [seed, setSeed] = useState("");
  const [pictures, setPictures] = useState<Picture[]>([]);
  const [error, setError] = useState("");
  const request = useRequest(client);
  const urls = useRef<string[]>([]);
  useEffect(() => () => urls.current.forEach((url) => URL.revokeObjectURL(url)), []);
  useEffect(() => {
    const saved = readLocal(sizeKey);
    setSize(sizes.includes(saved) ? saved : "512x512");
    setPictures([]); setError("");
  }, [sizeKey]);
  async function generate() {
    if (!choice.selected || !prompt.trim()) return;
    setError("");
    const started = performance.now();
    const chosenSeed = seed.trim() ? Number(seed) : Math.floor(Math.random() * 2 ** 31);
    try {
      const reply = await request.run((signal) => client.request<{ data?: { b64_json?: string }[]; output_format?: string }>("images/generations", { model: choice.selected!.id, prompt: prompt.trim(), size, seed: chosenSeed, ...(negative.trim() ? { negative_prompt: negative.trim() } : {}), ...(steps.trim() ? { steps: Number(steps) } : {}) }, signal));
      const data = reply?.data?.[0]?.b64_json;
      if (!reply) return;
      if (!data) throw new Error("The model returned no image.");
      const binary = Uint8Array.from(atob(data), (character) => character.charCodeAt(0));
      const url = URL.createObjectURL(new Blob([binary], { type: `image/${reply.output_format ?? "png"}` }));
      urls.current.push(url);
      setPictures((current) => [{ url, prompt: prompt.trim(), seconds: (performance.now() - started) / 1000, size, seed: chosenSeed }, ...current].slice(0, 8));
    } catch (caught) { if (!(caught instanceof DOMException && caught.name === "AbortError")) setError(failure(caught, "Image generation failed.")); }
  }
  if (ready.length === 0) return <Empty title="No image models available">Add an image model such as SD Turbo in Library, then set it to Hot or On demand.</Empty>;
  const stepsInvalid = !!steps.trim() && !(Number.isInteger(Number(steps)) && Number(steps) >= 1 && Number(steps) <= 150);
  const seedInvalid = !!seed.trim() && !(Number.isInteger(Number(seed)) && Number(seed) >= 0 && Number(seed) <= 4294967295);
  return <section className="test-panel" aria-label="Image test">
    <div className="toolbar media-options">
      <select aria-label="Model" value={choice.selected?.id ?? ""} disabled={request.busy} onChange={(event) => choice.setId(event.target.value)}><option value="">Choose a model…</option>{ready.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}</select>
      <select aria-label="Size" value={size} disabled={request.busy} onChange={(event) => { setSize(event.target.value); if (choice.selected) writeLocal(sizeKey, event.target.value); }}>{sizes.map((item) => <option key={item} value={item}>{item.replace("x", " × ")}</option>)}</select>
      <label className="inline-field">Steps <input className="short-input" inputMode="numeric" placeholder="Model default" value={steps} onChange={(event) => setSteps(event.target.value)} /></label>
      <label className="inline-field">Seed <input className="short-input" inputMode="numeric" placeholder="Random" value={seed} onChange={(event) => setSeed(event.target.value)} /></label>
    </div>
    <label htmlFor="image-prompt">Prompt</label>
    <textarea id="image-prompt" rows={3} value={prompt} maxLength={16384} placeholder="A lighthouse on a cliff at dusk, oil painting" onChange={(event) => setPrompt(event.target.value)} />
    <label htmlFor="image-negative">Avoid</label>
    <input id="image-negative" value={negative} placeholder="Optional: blurry, text, watermark" onChange={(event) => setNegative(event.target.value)} />
    {(stepsInvalid || seedInvalid) && <p className="inline-warning">{stepsInvalid ? "Steps must be a whole number from 1 to 150." : "Seed must be a whole number from 0 to 4294967295."}</p>}
    <div className="action-row">{request.busy ? <Button variant="danger" onClick={request.stop}>Stop</Button> : <Button variant="primary" disabled={!choice.selected || !prompt.trim() || !online || stepsInvalid || seedInvalid} onClick={() => void generate()}>Generate</Button>}{request.busy && <span className="muted">Generating… the first image also loads the model.</span>}{pictures.length > 0 && !request.busy && <Button onClick={() => setPictures([])}>Clear</Button>}</div>
    {error && <p className="inline-error" role="alert">{error}</p>}
    {pictures.length > 0 && <ul className="image-results" aria-label="Generated images">{pictures.map((picture) => <li key={picture.url}>
      <img src={picture.url} alt={picture.prompt} />
      <div className="image-meta"><span className="muted">{picture.size.replace("x", " × ")} · seed {picture.seed} · {picture.seconds.toFixed(1)} s</span><a className="button ghost" href={picture.url} download={`image-${picture.seed}.png`}>Download</a></div>
    </li>)}</ul>}
  </section>;
}
