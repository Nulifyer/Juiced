import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import { bytes, type Model } from "./domain";
import { speechModels, transcriptionForm, transcriptionText } from "./speech";
import { Button, Empty } from "./ui";

export function TranscriptionTest({ models, client, online }: { models: Model[]; client: Client; online: boolean }) {
  const [model, setModel] = useState(""); const [file, setFile] = useState<File | null>(null); const [language, setLanguage] = useState("auto");
  const [result, setResult] = useState<string | null>(null); const [error, setError] = useState(""); const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null); const request = useRef<AbortController | null>(null);
  const candidates = speechModels(models); const selected = candidates.find((item) => item.id === model);
  useEffect(() => () => { request.current?.abort(); }, [client]);
  useEffect(() => { if (model && !selected) invalidate(); }, [model, !!selected]);
  function invalidate() { request.current?.abort(); request.current = null; setBusy(false); setResult(null); setError(""); }
  function reset() { invalidate(); setFile(null); setLanguage("auto"); if (input.current) input.current.value = ""; }
  async function transcribe() {
    if (!selected || !file) return;
    request.current?.abort(); const controller = new AbortController(); request.current = controller;
    setBusy(true); setError(""); setResult(null);
    try {
      const form = await transcriptionForm(selected.id, file, language);
      if (controller.signal.aborted) return;
      const response = await client.multipart("audio/transcriptions", form, controller.signal);
      if (!controller.signal.aborted) setResult(transcriptionText(response));
    } catch (error) { if (!controller.signal.aborted) setError(error instanceof Error ? error.message : "Transcription failed. Check the file and model, then retry."); }
    finally { if (request.current === controller) { request.current = null; setBusy(false); } }
  }
  return <section className="transcription-test" aria-label="Temporary transcription test">
    {candidates.length === 0 ? <Empty title="No eligible speech models">Set a speech model to Hot or On demand in Loading policy.</Empty> : <>
      <div className="test-controls"><label htmlFor="speech-model">Speech model</label><select id="speech-model" value={model} onChange={(event) => { invalidate(); setModel(event.target.value); }}><option value="">Choose a speech model…</option>{candidates.map((item) => <option key={item.id} value={item.id}>{item.id}</option>)}</select><p className="field-help">Loads the model first if needed.</p></div>
      <div className="form-grid"><div className="form-wide"><label htmlFor="speech-file">Temporary WAV file</label><input ref={input} id="speech-file" type="file" accept=".wav,audio/wav" onChange={(event) => { invalidate(); setFile(event.target.files?.[0] ?? null); }} /><p className="field-help">WAV, up to 8 MiB.</p>{file && <p className="field-help selected-audio">{file.name} · {bytes(file.size)}</p>}</div><div><label htmlFor="speech-language">Audio language</label><input id="speech-language" value={language} placeholder="auto, de, en…" onChange={(event) => { invalidate(); setLanguage(event.target.value); }} /><p className="field-help">auto, or a code like en.</p></div></div>
      <div className="test-action"><span className="muted">Not saved.</span><div className="action-row"><Button disabled={!file && result === null && !error && !busy} onClick={reset}>Reset transcription</Button>{busy ? <Button variant="danger" onClick={() => { invalidate(); setError("Stopped. The service may restart the speech model to clean up."); }}>Stop test</Button> : <Button variant="primary" disabled={!online || !selected || !file} onClick={() => void transcribe()}>Transcribe audio</Button>}</div></div>
      {!selected && model && <p className="inline-warning">That model is no longer available. Pick another.</p>}
      {error && <p role="alert" className="inline-error">{error}</p>}
      {(busy || result !== null) && <div className="test-output transcription-output" aria-live="polite" aria-busy={busy}><h2>Transcript</h2>{busy ? <p className="muted">Transcribing…</p> : <><p role="status">Done.</p><pre>{result}</pre></>}</div>}
    </>}
  </section>;
}
