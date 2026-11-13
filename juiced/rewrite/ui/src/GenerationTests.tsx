import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import type { Model } from "./domain";
import { Button, HelpTip } from "./ui";

type Props = { model: Model; client: Client; online: boolean };
const failure = (error: unknown, fallback: string) => error instanceof Error ? error.message : fallback;
const aborted = (error: unknown) => error instanceof DOMException && error.name === "AbortError";

function useRun(client: Client) {
  const controller = useRef<AbortController | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => () => controller.current?.abort(), [client]);
  async function run<T>(work: (signal: AbortSignal) => Promise<T>): Promise<T> {
    controller.current?.abort();
    const current = new AbortController(); controller.current = current;
    setBusy(true);
    try { return await work(current.signal); }
    finally { if (controller.current === current) { controller.current = null; setBusy(false); } }
  }
  const stop = () => { controller.current?.abort(); controller.current = null; setBusy(false); };
  return { busy, run, stop };
}

const wholeNumber = (value: string, low: number, high: number) => !value.trim() || (Number.isInteger(Number(value)) && Number(value) >= low && Number(value) <= high);

export function AudioGenerationTest({ model, client, online }: Props) {
  const [prompt, setPrompt] = useState("");
  const [lyrics, setLyrics] = useState("");
  const [duration, setDuration] = useState("10");
  const [seed, setSeed] = useState("");
  const [clip, setClip] = useState<{ url: string; seconds: number; size: number } | null>(null);
  const [error, setError] = useState("");
  const request = useRun(client);
  useEffect(() => () => { if (clip) URL.revokeObjectURL(clip.url); }, [clip]);
  useEffect(() => { setClip(null); setError(""); }, [model.id]);
  const durationValid = wholeNumber(duration, 1, 600);
  const seedValid = wholeNumber(seed, 0, 4294967295);
  async function generate() {
    if (!prompt.trim()) return;
    setError(""); setClip(null);
    const started = performance.now();
    try {
      const blob = await request.run((signal) => client.binary("audio/generations", { model: model.id, prompt: prompt.trim(), ...(lyrics.trim() ? { lyrics: lyrics.trim() } : {}), ...(duration.trim() ? { duration: Number(duration) } : {}), ...(seed.trim() ? { seed: Number(seed) } : {}) }, signal, 192 * 1024 * 1024));
      setClip({ url: URL.createObjectURL(blob), seconds: (performance.now() - started) / 1000, size: blob.size });
    } catch (caught) { if (!aborted(caught)) setError(failure(caught, "Audio generation failed.")); }
  }
  return <section className="test-panel" aria-label="Audio generation test">
    <div className="toolbar media-options">
      <label className="inline-field">Length <input className="short-input" inputMode="numeric" aria-label="Length in seconds" value={duration} onChange={(event) => setDuration(event.target.value)} /> s</label>
      <label className="inline-field">Seed <input className="short-input" inputMode="numeric" placeholder="Random" value={seed} onChange={(event) => setSeed(event.target.value)} /></label>
      <HelpTip label="Audio generation">Music models (ACE-Step) follow the style prompt and sing any lyrics. Sound-effect models (ThinkSound, MOSS-SoundEffect) ignore lyrics and make clips of up to 30 s.</HelpTip>
    </div>
    <label htmlFor="audio-prompt">Prompt</label>
    <textarea id="audio-prompt" rows={2} value={prompt} maxLength={16384} placeholder="Warm lo-fi beat with soft piano, or: glass shattering on a stone floor" onChange={(event) => setPrompt(event.target.value)} />
    <label htmlFor="audio-lyrics">Lyrics</label>
    <textarea id="audio-lyrics" rows={3} value={lyrics} maxLength={16384} placeholder={"Optional, for music: [verse]\nOne sung line per row"} onChange={(event) => setLyrics(event.target.value)} />
    {(!durationValid || !seedValid) && <p className="inline-warning">{!durationValid ? "Length must be a whole number of seconds from 1 to 600." : "Seed must be a whole number from 0 to 4294967295."}</p>}
    <div className="action-row">{request.busy ? <Button variant="danger" onClick={request.stop}>Stop</Button> : <Button variant="primary" disabled={!prompt.trim() || !online || !durationValid || !seedValid} onClick={() => void generate()}>Generate</Button>}{request.busy && <span className="muted">Generating… this can take minutes, and the first clip also loads the model.</span>}{clip && <span className="muted">Ready in {clip.seconds.toFixed(1)} s · {Math.round(clip.size / 1024)} KiB</span>}</div>
    {error && <p className="inline-error" role="alert">{error}</p>}
    {clip && <div className="audio-result"><audio controls src={clip.url} aria-label="Generated audio" /><a className="button" href={clip.url} download="generated.wav">Download</a></div>}
  </section>;
}

/** Counts the meshes and vertices a glTF-binary declares in its JSON chunk. */
export function glbSummary(buffer: ArrayBuffer): { meshes: number; vertices: number } | null {
  const view = new DataView(buffer);
  if (buffer.byteLength < 20 || view.getUint32(0, true) !== 0x46546c67 || view.getUint32(4, true) !== 2) return null;
  const length = view.getUint32(12, true);
  if (view.getUint32(16, true) !== 0x4e4f534a || 20 + length > buffer.byteLength) return null;
  try {
    const json = JSON.parse(new TextDecoder().decode(new Uint8Array(buffer, 20, length))) as { meshes?: { primitives?: { attributes?: { POSITION?: number } }[] }[]; accessors?: { count?: number }[] };
    const meshes = Array.isArray(json.meshes) ? json.meshes : [];
    const vertices = meshes.flatMap((mesh) => mesh.primitives ?? []).reduce((total, primitive) => total + (json.accessors?.[primitive.attributes?.POSITION ?? -1]?.count ?? 0), 0);
    return { meshes: meshes.length, vertices };
  } catch { return null; }
}

const resolutions = ["512", "1024", "1536"] as const;

export function Model3dTest({ model, client, online }: Props) {
  const [image, setImage] = useState<{ url: string; data: string; name: string } | null>(null);
  const [resolution, setResolution] = useState<(typeof resolutions)[number]>("512");
  const [background, setBackground] = useState("");
  const [seed, setSeed] = useState("");
  const [mesh, setMesh] = useState<{ url: string; seconds: number; size: number; summary: ReturnType<typeof glbSummary> } | null>(null);
  const [error, setError] = useState("");
  const request = useRun(client);
  useEffect(() => () => { if (mesh) URL.revokeObjectURL(mesh.url); }, [mesh]);
  useEffect(() => () => { if (image) URL.revokeObjectURL(image.url); }, [image]);
  useEffect(() => { setMesh(null); setError(""); }, [model.id]);
  const seedValid = wholeNumber(seed, 0, 4294967295);
  function pick(file: File | undefined) {
    setError("");
    if (!file) return;
    if (file.size > 6 * 1024 * 1024) { setError("Choose an image under 6 MiB."); return; }
    const reader = new FileReader();
    reader.onload = () => { if (typeof reader.result === "string") setImage({ url: URL.createObjectURL(file), data: reader.result, name: file.name }); };
    reader.onerror = () => setError("The image couldn't be read.");
    reader.readAsDataURL(file);
  }
  async function generate() {
    if (!image) return;
    setError(""); setMesh(null);
    const started = performance.now();
    try {
      const blob = await request.run((signal) => client.binary("3d/generations", { model: model.id, image: image.data, resolution: Number(resolution), ...(background ? { bg_removal: background } : {}), ...(seed.trim() ? { seed: Number(seed) } : {}) }, signal, 384 * 1024 * 1024));
      setMesh({ url: URL.createObjectURL(blob), seconds: (performance.now() - started) / 1000, size: blob.size, summary: glbSummary(await blob.arrayBuffer()) });
    } catch (caught) { if (!aborted(caught)) setError(failure(caught, "3D generation failed.")); }
  }
  return <section className="test-panel" aria-label="3D generation test">
    <div className="toolbar media-options">
      <div className="segmented" role="radiogroup" aria-label="Resolution">{resolutions.map((item) => <button key={item} role="radio" aria-checked={resolution === item} disabled={request.busy} onClick={() => setResolution(item)}>{item}</button>)}</div>
      <select aria-label="Background removal" value={background} disabled={request.busy} onChange={(event) => setBackground(event.target.value)}><option value="">Background: automatic</option><option value="threshold">Background: threshold</option><option value="birefnet">Background: BiRefNet</option></select>
      <label className="inline-field">Seed <input className="short-input" inputMode="numeric" placeholder="Random" value={seed} onChange={(event) => setSeed(event.target.value)} /></label>
      <HelpTip label="3D generation">Turns one object photo into a textured mesh (.glb). 512 is fastest; 1024 and 1536 need the model's high-resolution files and take several minutes. BiRefNet cuts out real photo backgrounds.</HelpTip>
    </div>
    <div className="action-row wrap">
      <label className="button">Choose image<input type="file" accept="image/png,image/jpeg,image/bmp,image/gif" className="sr-only" onChange={(event) => pick(event.target.files?.[0])} /></label>
      {image && <span className="muted break-word">{image.name}</span>}
    </div>
    {image && <img className="mesh-input" src={image.url} alt="Input for 3D generation" />}
    {!seedValid && <p className="inline-warning">Seed must be a whole number from 0 to 4294967295.</p>}
    <div className="action-row">{request.busy ? <Button variant="danger" onClick={request.stop}>Stop</Button> : <Button variant="primary" disabled={!image || !online || !seedValid} onClick={() => void generate()}>Generate mesh</Button>}{request.busy && <span className="muted">Reconstructing… this takes minutes.</span>}</div>
    {error && <p className="inline-error" role="alert">{error}</p>}
    {mesh && <div className="audio-result"><span className="muted">Ready in {mesh.seconds.toFixed(0)} s · {(mesh.size / 1024 ** 2).toFixed(1)} MiB{mesh.summary ? ` · ${mesh.summary.vertices.toLocaleString()} vertices` : ""}</span><a className="button primary" href={mesh.url} download="model.glb">Download .glb</a></div>}
  </section>;
}
