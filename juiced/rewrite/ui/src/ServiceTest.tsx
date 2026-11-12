import { useEffect, useRef, useState } from "react";
import { chatTestTokenLimit, type Client } from "./api";
import { eligible, type Model } from "./domain";
import { Button, Empty } from "./ui";
import { TranscriptionTest } from "./TranscriptionTest";

export function ServiceTest(props: { models: Model[]; client: Client; online: boolean }) {
  const [operation, setOperation] = useState<"chat" | "transcription">("chat");
  return <section className="service-tests"><div className="service-switcher" role="group" aria-label="Test operation"><Button aria-pressed={operation === "chat"} onClick={() => setOperation("chat")}>Chat</Button><Button aria-pressed={operation === "transcription"} onClick={() => setOperation("transcription")}>Transcription</Button></div>{operation === "chat" ? <ChatTest {...props} /> : <TranscriptionTest {...props} />}</section>;
}

function ChatTest({ models, client, online }: { models: Model[]; client: Client; online: boolean }) {
  const [id, setId] = useState("");
  const [prompt, setPrompt] = useState("");
  const [result, setResult] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const request = useRef<AbortController | null>(null);
  const ready = models.filter(eligible);
  useEffect(() => () => { request.current?.abort(); }, [client]);
  const selected = ready.find((model) => model.id === id);
  async function run() {
    if (!selected || !prompt.trim()) return;
    const controller = new AbortController();
    request.current = controller;
    setBusy(true); setError(""); setResult("");
    try {
      const response = await client.testChat(selected, prompt, controller.signal);
      if (!controller.signal.aborted) setResult(response || "The request completed without text content.");
    } catch (error) {
      if (!controller.signal.aborted) setError(error instanceof Error ? error.message : "The test failed. Retry when the service is available.");
    } finally {
      if (request.current === controller) { setBusy(false); request.current = null; }
    }
  }
  return <section className="service-test">
    {ready.length === 0 ? <Empty title="No eligible chat models">Enable a downloaded model with the chat/completions operation in Loading policy. Tests never enable models or change saved settings.</Empty> : <>
      <div className="test-controls"><label htmlFor="test-model">Model</label><select id="test-model" value={id} disabled={busy} onChange={(event) => { setId(event.target.value); setResult(""); setError(""); }}><option value="">Choose a model…</option>{ready.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}</select><p className="field-help">An On demand model loads when the test begins. This uses a shared worker.</p></div>
      {selected && <p className="field-help">This test requests at most {chatTestTokenLimit(selected)} output tokens.</p>}
      <label htmlFor="test-prompt">Temporary prompt</label><textarea id="test-prompt" rows={6} value={prompt} disabled={busy} onChange={(event) => setPrompt(event.target.value)} placeholder="Send a small request to verify the selected model." />
      <div className="test-action"><span className="muted">Nothing is saved after leaving this screen.</span><div className="action-row"><Button disabled={busy || (!result && !prompt && !error)} onClick={() => { setPrompt(""); setResult(""); setError(""); }}>Reset</Button>{busy ? <Button variant="danger" onClick={() => { request.current?.abort(); setError("Test stopped."); }}>Stop test</Button> : <Button variant="primary" disabled={!selected || !prompt.trim() || !online} onClick={() => void run()}>Run test</Button>}</div></div>
      {!selected && id && <p className="inline-warning">The selected model is no longer eligible. Choose an enabled model.</p>}
      {error && <p className="inline-error" role="alert">{error}</p>}
      <div className="test-output" aria-live="polite" aria-busy={busy}><h2>Response</h2>{busy ? <p className="muted">Waiting for the selected worker…</p> : result ? <pre>{result}</pre> : <p className="muted">Run a test to inspect its response.</p>}</div>
    </>}
  </section>;
}
