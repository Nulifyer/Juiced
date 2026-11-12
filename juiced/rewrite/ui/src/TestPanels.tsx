import { useEffect, useRef, useState } from "react";
import { chatTestTokenLimit, type Client } from "./api";
import { eligible, offers, operationEligible, type Model } from "./domain";
import { Button, Empty } from "./ui";

type Vector = { values: number[]; norm: number };
const cosine = (left: Vector, right: Vector) => left.values.reduce((sum, value, index) => sum + value * (right.values[index] ?? 0), 0) / (left.norm * right.norm || 1);

export function EmbeddingsTest({ models, client, online }: { models: Model[]; client: Client; online: boolean }) {
  const ready = models.filter((model) => operationEligible(model, "embeddings"));
  const [id, setId] = useState("");
  const [text, setText] = useState("");
  const [vectors, setVectors] = useState<Vector[] | null>(null);
  const [inputs, setInputs] = useState<string[]>([]);
  const [elapsed, setElapsed] = useState<number | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const selected = ready.find((model) => model.id === id) ?? (ready.length === 1 && !id ? ready[0] : undefined);
  async function run() {
    const lines = text.split("\n").map((line) => line.trim()).filter(Boolean).slice(0, 16);
    if (!selected || lines.length === 0) return;
    setBusy(true); setError(""); setVectors(null);
    const started = performance.now();
    try {
      const response = await client.request<{ data?: { embedding?: number[] }[] }>("embeddings", { model: selected.id, input: lines });
      const data = (response.data ?? []).map((item) => { const values = Array.isArray(item.embedding) ? item.embedding.filter((value) => typeof value === "number") : []; return { values, norm: Math.hypot(...values) }; });
      if (data.length !== lines.length || data.some((item) => item.values.length === 0)) throw new Error("The model returned an unexpected embedding response.");
      setVectors(data); setInputs(lines); setElapsed(performance.now() - started);
    } catch (failure) { setError(failure instanceof Error ? failure.message : "The request failed."); }
    finally { setBusy(false); }
  }
  if (ready.length === 0) return <Empty title="No embedding models available">Add an embedding model in Library, then set it to Hot or On demand.</Empty>;
  return <section className="test-panel" aria-label="Embeddings test">
    <div className="toolbar"><select aria-label="Model" value={selected?.id ?? ""} disabled={busy} onChange={(event) => setId(event.target.value)}><option value="">Choose a model…</option>{ready.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}</select></div>
    <label htmlFor="embedding-input">Texts, one per line</label>
    <textarea id="embedding-input" rows={5} value={text} placeholder={"A cat sits on the mat\nA kitten rests on a rug\nStock prices fell today"} onChange={(event) => setText(event.target.value)} />
    <div className="action-row"><Button variant="primary" disabled={!selected || !text.trim() || busy || !online} onClick={() => void run()}>{busy ? "Embedding…" : "Embed"}</Button>{vectors && <span className="muted">{vectors.length} vectors · {vectors[0]!.values.length} dimensions · {Math.round(elapsed ?? 0)} ms</span>}</div>
    {error && <p className="inline-error" role="alert">{error}</p>}
    {vectors && vectors.length > 1 && <div className="similarity" role="table" aria-label="Cosine similarity">
      <div role="row" className="similarity-row"><span role="columnheader" />{inputs.map((_, index) => <span role="columnheader" key={index}>{index + 1}</span>)}</div>
      {vectors.map((left, row) => <div role="row" className="similarity-row" key={row}><span role="rowheader" title={inputs[row]}>{row + 1}. {inputs[row]}</span>{vectors.map((right, column) => { const value = cosine(left, right); return <span role="cell" key={column} style={{ background: `rgba(140, 172, 241, ${Math.max(0, value) * 0.45})` }}>{value.toFixed(2)}</span>; })}</div>)}
    </div>}
    {vectors && <ul className="vector-preview">{vectors.map((vector, index) => <li key={index}><span className="muted">{index + 1}.</span> <code>[{vector.values.slice(0, 6).map((value) => value.toFixed(4)).join(", ")}, …]</code></li>)}</ul>}
  </section>;
}

type Column = { model: string; content: string; tokens: number | null; rate: number | null; firstToken: number | null; total: number | null; error: string; streaming: boolean };

export function CompareTest({ models, client, online }: { models: Model[]; client: Client; online: boolean }) {
  const ready = models.filter((model) => eligible(model) && offers(model, "chat"));
  const [picked, setPicked] = useState<string[]>(() => ready.slice(0, 2).map((model) => model.id));
  const [prompt, setPrompt] = useState("");
  const [columns, setColumns] = useState<Column[]>([]);
  const controllers = useRef<AbortController[]>([]);
  useEffect(() => () => controllers.current.forEach((controller) => controller.abort()), [client]);
  const busy = columns.some((column) => column.streaming);
  async function run() {
    const chosen = picked.filter((id, index) => id && picked.indexOf(id) === index).map((id) => ready.find((model) => model.id === id)).filter((model): model is Model => !!model);
    if (chosen.length < 2 || !prompt.trim()) return;
    controllers.current = chosen.map(() => new AbortController());
    setColumns(chosen.map((model) => ({ model: model.id, content: "", tokens: null, rate: null, firstToken: null, total: null, error: "", streaming: true })));
    const messages = [{ role: "user", content: prompt.trim() }];
    await Promise.all(chosen.map(async (model, index) => {
      const started = performance.now();
      const update = (patch: Partial<Column>) => setColumns((current) => current.map((column, at) => at === index ? { ...column, ...patch } : column));
      let first: number | null = null;
      try {
        const final = await client.streamChat(model.id, messages, chatTestTokenLimit(model), (state) => { if (first === null && (state.content || state.reasoning)) { first = performance.now() - started; update({ firstToken: first }); } update({ content: state.content || state.reasoning }); }, controllers.current[index]!.signal);
        update({ content: final.content, tokens: final.outputTokens, rate: final.decodeRate, total: performance.now() - started, streaming: false });
      } catch (failure) { update({ streaming: false, error: controllers.current[index]?.signal.aborted ? "Stopped." : failure instanceof Error ? failure.message : "The request failed." }); }
    }));
  }
  if (ready.length < 2) return <Empty title="Add another chat model to compare">Compare needs at least two chat models that are Hot or On demand.</Empty>;
  return <section className="test-panel compare" aria-label="Compare models">
    <div className="toolbar">{[0, 1, 2].slice(0, Math.min(3, ready.length)).map((slot) => <select key={slot} aria-label={`Model ${slot + 1}`} value={picked[slot] ?? ""} disabled={busy} onChange={(event) => setPicked((current) => { const next = [...current]; next[slot] = event.target.value; return next; })}><option value="">{slot < 2 ? "Choose a model…" : "Optional third model"}</option>{ready.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}</select>)}</div>
    <form className="composer" onSubmit={(event) => { event.preventDefault(); void run(); }}>
      <textarea aria-label="Prompt" rows={2} value={prompt} placeholder="Ask all selected models the same thing" onChange={(event) => setPrompt(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void run(); } }} />
      {busy ? <Button variant="danger" onClick={() => controllers.current.forEach((controller) => controller.abort())}>Stop</Button> : <Button variant="primary" type="submit" disabled={!prompt.trim() || !online || new Set(picked.filter(Boolean)).size < 2}>Compare</Button>}
    </form>
    {columns.length > 0 && <div className="compare-grid" style={{ gridTemplateColumns: `repeat(${columns.length}, minmax(0, 1fr))` }}>{columns.map((column) => <article className="panel compare-column" key={column.model} aria-label={column.model}>
      <header className="panel-head"><h3 className="break-word">{column.model}</h3></header>
      <dl className="compare-stats"><div><dt>First token</dt><dd>{column.firstToken === null ? "—" : `${Math.round(column.firstToken)} ms`}</dd></div><div><dt>Total</dt><dd>{column.total === null ? "—" : `${(column.total / 1000).toFixed(1)} s`}</dd></div><div><dt>Speed</dt><dd>{column.rate ? `${Math.round(column.rate)} tok/s` : "—"}</dd></div></dl>
      {column.error ? <p className="inline-error" role="alert">{column.error}</p> : <p className="compare-text">{column.content || (column.streaming ? "Waiting…" : "")}</p>}
    </article>)}</div>}
  </section>;
}
