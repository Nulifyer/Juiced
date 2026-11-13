import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import { bytes, type Model } from "./domain";
import { transcriptionForm, transcriptionText } from "./speech";
import { recordingType, toSpeechWav } from "./audio16k";
import { Button, HelpTip, Icon } from "./ui";

export function TranscriptionTest({ model, client, online }: { model: Model; client: Client; online: boolean }) {
  const [file, setFile] = useState<File | null>(null);
  const [language, setLanguage] = useState("auto");
  const [result, setResult] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [elapsed, setElapsed] = useState<number | null>(null);
  const [recording, setRecording] = useState<number | null>(null);
  const [now, setNow] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const request = useRef<AbortController | null>(null);
  const recorder = useRef<MediaRecorder | null>(null);
  useEffect(() => () => { request.current?.abort(); stopTracks(); }, [client]);
  useEffect(() => {
    if (recording === null) return;
    const timer = setInterval(() => setNow(performance.now()), 250);
    return () => clearInterval(timer);
  }, [recording]);
  function stopTracks() { recorder.current?.stream.getTracks().forEach((track) => track.stop()); }
  function clear() { request.current?.abort(); request.current = null; setBusy(false); setResult(null); setError(""); setElapsed(null); }
  async function transcribe(source = file) {
    if (!source) return;
    request.current?.abort();
    const controller = new AbortController(); request.current = controller;
    setBusy(true); setError(""); setResult(null);
    const started = performance.now();
    try {
      const wav = await toSpeechWav(source, source.name);
      const form = await transcriptionForm(model.id, wav, language);
      if (controller.signal.aborted) return;
      const response = await client.multipart("audio/transcriptions", form, controller.signal);
      if (!controller.signal.aborted) { setResult(transcriptionText(response)); setElapsed(performance.now() - started); }
    } catch (failure) { if (!controller.signal.aborted) setError(failure instanceof Error ? failure.message : "Transcription failed. Check the file and retry."); }
    finally { if (request.current === controller) { request.current = null; setBusy(false); } }
  }
  async function record() {
    if (recorder.current) { recorder.current.stop(); return; }
    clear();
    const type = recordingType();
    if (type === null || !navigator.mediaDevices?.getUserMedia) { setError("This browser can't record here. Recording needs HTTPS or localhost."); return; }
    let stream: MediaStream;
    try { stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true } }); }
    catch { setError("Microphone access was denied or no microphone is available."); return; }
    const chunks: Blob[] = [];
    const media = new MediaRecorder(stream, type ? { mimeType: type } : undefined);
    media.ondataavailable = (event) => { if (event.data.size) chunks.push(event.data); };
    media.onstop = () => {
      stream.getTracks().forEach((track) => track.stop());
      recorder.current = null; setRecording(null);
      const recorded = new File(chunks, "recording.webm", { type: media.mimeType || "audio/webm" });
      if (input.current) input.current.value = "";
      setFile(recorded);
      if (recorded.size) void transcribe(recorded);
    };
    recorder.current = media;
    media.start(250);
    setRecording(performance.now()); setNow(performance.now());
  }
  const seconds = recording === null ? 0 : Math.max(0, Math.floor((now - recording) / 1000));
  return <section className="test-panel" aria-label="Transcription test">
    <div className="toolbar media-options">
      <input ref={input} type="file" aria-label="Audio file" accept="audio/*,.wav,.mp3,.m4a,.ogg,.opus,.flac,.webm" disabled={recording !== null} onChange={(event) => { clear(); setFile(event.target.files?.[0] ?? null); }} />
      <Button className={recording !== null ? "recording" : ""} disabled={!online || busy} aria-pressed={recording !== null} onClick={() => void record()}><Icon name="mic" size={14} /> {recording !== null ? `Stop · ${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}` : "Record"}</Button>
      <label className="inline-field">Language <input className="short-input" aria-label="Audio language" value={language} placeholder="auto" onChange={(event) => { clear(); setLanguage(event.target.value); }} /></label>
      <HelpTip label="Audio input">Any audio the browser can play. It's converted to 16 kHz mono WAV here before upload, up to 8 MiB (about 4 minutes). Use auto, or a language code such as en.</HelpTip>
    </div>
    <p className="field-help">{recording !== null ? "Recording… press Stop to transcribe." : file ? `${file.name} · ${bytes(file.size)}` : "Choose an audio file or record from the microphone."}</p>
    <div className="action-row">
      {busy ? <Button variant="danger" onClick={() => { clear(); setError("Stopped. The service may restart the speech model to clean up."); }}>Stop</Button>
        : <Button variant="primary" disabled={!online || !file || recording !== null} onClick={() => void transcribe()}>Transcribe</Button>}
      {busy && <span className="muted">Transcribing… the first request also loads the model.</span>}
      {elapsed !== null && !busy && <span className="muted">Done in {(elapsed / 1000).toFixed(2)} s</span>}
    </div>
    {error && <p role="alert" className="inline-error">{error}</p>}
    {result !== null && <div className="test-output" aria-live="polite"><h3>Transcript</h3><p className="transcript">{result || "No speech detected."}</p></div>}
  </section>;
}
