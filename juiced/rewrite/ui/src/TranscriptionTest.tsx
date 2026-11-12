import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import { bytes, type Model } from "./domain";
import { transcriptionForm, transcriptionText } from "./speech";
import { Button } from "./ui";

export function TranscriptionTest({ model, client, online }: { model: Model; client: Client; online: boolean }) {
  const [file, setFile] = useState<File | null>(null);
  const [language, setLanguage] = useState("auto");
  const [result, setResult] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [elapsed, setElapsed] = useState<number | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const request = useRef<AbortController | null>(null);
  useEffect(() => () => { request.current?.abort(); }, [client]);
  function clear() { request.current?.abort(); request.current = null; setBusy(false); setResult(null); setError(""); setElapsed(null); }
  async function transcribe() {
    if (!file) return;
    request.current?.abort();
    const controller = new AbortController(); request.current = controller;
    setBusy(true); setError(""); setResult(null);
    const started = performance.now();
    try {
      const form = await transcriptionForm(model.id, file, language);
      if (controller.signal.aborted) return;
      const response = await client.multipart("audio/transcriptions", form, controller.signal);
      if (!controller.signal.aborted) { setResult(transcriptionText(response)); setElapsed(performance.now() - started); }
    } catch (failure) { if (!controller.signal.aborted) setError(failure instanceof Error ? failure.message : "Transcription failed. Check the file and retry."); }
    finally { if (request.current === controller) { request.current = null; setBusy(false); } }
  }
  return <section className="test-panel" aria-label="Transcription test">
    <div className="toolbar media-options">
      <input ref={input} type="file" aria-label="Audio file" accept=".wav,audio/wav" onChange={(event) => { clear(); setFile(event.target.files?.[0] ?? null); }} />
      <label className="inline-field">Language <input className="short-input" aria-label="Audio language" value={language} placeholder="auto" onChange={(event) => { clear(); setLanguage(event.target.value); }} /></label>
    </div>
    <p className="field-help">{file ? `${file.name} · ${bytes(file.size)}` : "WAV up to 8 MiB. Use auto, or a language code such as en."}</p>
    <div className="action-row">
      {busy ? <Button variant="danger" onClick={() => { clear(); setError("Stopped. The service may restart the speech model to clean up."); }}>Stop</Button>
        : <Button variant="primary" disabled={!online || !file} onClick={() => void transcribe()}>Transcribe</Button>}
      {busy && <span className="muted">Transcribing… the first request also loads the model.</span>}
      {elapsed !== null && !busy && <span className="muted">Done in {(elapsed / 1000).toFixed(2)} s</span>}
    </div>
    {error && <p role="alert" className="inline-error">{error}</p>}
    {result !== null && <div className="test-output" aria-live="polite"><h3>Transcript</h3><p className="transcript">{result || "No speech detected."}</p></div>}
  </section>;
}
