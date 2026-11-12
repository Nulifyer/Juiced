import { useEffect, useMemo, useRef, useState } from "react";
import type { Client } from "./api";
import { bytes } from "./domain";
import { parseSystemInfo } from "./hardware";
import { parseSearch, progress, tasks, type LibraryModel, type Quant, type SearchResult } from "./libraryData";
import { Registration } from "./Registration";
import { Badge, Button, Dialog, Empty, Icon } from "./ui";
import { useResource } from "./useResource";

function fit(size: number | null, vram: number | null, ram: number | null): [string, "good" | "warning" | "bad" | "quiet"] {
  if (size === null) return ["Size unknown", "quiet"];
  const needed = size * 1.15;
  if (vram !== null && needed <= vram) return ["Fits in GPU memory", "good"];
  if (ram !== null && needed <= ram) return [vram ? "Too big for the GPU; runs partly on CPU" : "Runs on the CPU", "warning"];
  return ["Likely too big for this machine", "bad"];
}
const common = ["Q4_K_M", "Q5_K_M", "Q6_K", "Q8_0", "F16", "BF16", "MXFP4"];
function featured(quants: Quant[]): Quant[] {
  const picked = quants.filter((quant) => quant.recommended || quant.present || quant.jobs.length > 0 || common.includes(quant.label.toUpperCase()));
  return (picked.length ? picked : quants.slice(0, 4)).slice(0, 6).sort((left, right) => (left.bytes ?? 0) - (right.bytes ?? 0));
}
const compact = (value: number | null) => value === null ? null : value >= 1e6 ? `${(value / 1e6).toFixed(1)}M` : value >= 1e3 ? `${Math.round(value / 1e3)}k` : String(value);

export function FindModels({ client, online, onRegistered }: { client: Client; online: boolean; onRegistered: () => Promise<void> }) {
  const [query, setQuery] = useState("");
  const [task, setTask] = useState<string | null>(null);
  const [result, setResult] = useState<SearchResult | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<{ repo: string; label: string } | null>(null);
  const [notice, setNotice] = useState("");
  const [adding, setAdding] = useState(false);
  const [tick, setTick] = useState(0);
  const info = useResource(client, "system-info", parseSystemInfo, 0);
  const request = useRef<AbortController | null>(null);
  useEffect(() => {
    const timer = setTimeout(async () => {
      request.current?.abort();
      const controller = new AbortController(); request.current = controller;
      setLoading(true);
      try {
        const params = new URLSearchParams({ limit: "12" });
        if (query.trim()) params.set("q", query.trim());
        if (task) params.set("task", task);
        const next = parseSearch(await client.request(`juiced/library/search?${params}`, undefined, controller.signal));
        if (!controller.signal.aborted) { setResult(next); setError(""); }
      } catch (failure) {
        if (!controller.signal.aborted) setError(failure instanceof Error ? failure.message : "Search failed.");
      } finally { if (!controller.signal.aborted) setLoading(false); }
    }, tick ? 0 : 300);
    return () => clearTimeout(timer);
  }, [client, query, task, tick]);
  useEffect(() => () => request.current?.abort(), []);
  useEffect(() => { if (!notice) return; const timer = setTimeout(() => setNotice(""), 4000); return () => clearTimeout(timer); }, [notice]);
  const downloading = result?.models.some((model) => model.quants.some((quant) => progress(quant) !== null)) ?? false;
  useEffect(() => {
    if (!downloading) return;
    const timer = setInterval(() => setTick((value) => value + 1), 2000);
    return () => clearInterval(timer);
  }, [downloading]);
  const model = result?.models.find((item) => item.repo === selected?.repo);
  const quant = model?.quants.find((item) => item.label === selected?.label);
  const gpu = info.data?.gpus.reduce((best, item) => Math.max(best, item.vram_total_bytes ?? 0), 0) || null;
  const ram = info.data?.memory?.total_bytes ?? null;
  async function download(target: LibraryModel, item: Quant) {
    if (!result) return;
    setError(""); setNotice("");
    try {
      await client.request("juiced/library/pull", { repo: target.repo, file: item.file, expected_generation: result.generation });
      setNotice(`Downloading ${target.name} ${item.label}.`);
      setTick((value) => value + 1);
    } catch (failure) { setError(failure instanceof Error ? failure.message : "The download didn't start."); }
  }
  const models = useMemo(() => result?.models ?? [], [result]);
  return <div className={`find-models ${model ? "has-detail" : ""}`}>
    <div className="find-main">
      <div className="toolbar">
        <input type="search" aria-label="Search models" placeholder="Search models, e.g. qwen3, llama, whisper" value={query} onChange={(event) => setQuery(event.target.value)} />
        <div className="chip-filter" role="group" aria-label="Task"><button aria-pressed={task === null} onClick={() => setTask(null)}>All</button>{tasks.map(([id, label]) => <button key={id} aria-pressed={task === id} onClick={() => setTask(task === id ? null : id)}>{label}</button>)}</div>
      </div>
      {error && <div className="action-row"><p className="inline-error" role="alert">{error}</p><Button onClick={() => setTick((value) => value + 1)}>Retry</Button></div>}
      {notice && <p className="inline-success" role="status">{notice}</p>}
      {result && result.hub !== "ok" && <p className="field-help">{result.offline ? "Offline mode: showing recommended and downloaded models only." : "Hugging Face is unreachable right now; showing recommended and downloaded models."}</p>}
      {!result ? <p className="muted">{loading ? "Searching…" : ""}</p> : models.length === 0 ? <Empty title="No models found">Try a shorter search or another task.</Empty> : <ul className="model-results" aria-label="Search results" aria-busy={loading}>
        {models.map((item) => <li key={`${item.source}:${item.repo}`} className={`model-result ${item.repo === selected?.repo ? "selected" : ""}`}>
          <div className="result-head">
            <strong className="break-word">{item.name}</strong>
            {item.source === "catalog" && <Badge tone="good">Recommended</Badge>}
            {item.quants.some((entry) => entry.present) && <Badge>On this machine</Badge>}
            <span className="muted result-meta">{[item.publisher, item.task, compact(item.downloads) && `${compact(item.downloads)} downloads`].filter(Boolean).join(" · ")}</span>
          </div>
          <div className="quant-chips" role="group" aria-label={`${item.name} quants`}>
            {item.quants.length === 0 ? <span className="muted">No GGUF files found</span> : featured(item.quants).map((entry) => {
              const percent = progress(entry);
              return <button key={entry.label} className={`quant-chip ${entry.present ? "present" : ""}`} aria-pressed={item.repo === selected?.repo && entry.label === selected?.label} onClick={() => setSelected({ repo: item.repo, label: entry.label })} title={entry.file}>
                {entry.present && <span aria-label="Downloaded">✓ </span>}{entry.label}<span className="muted"> {bytes(entry.bytes)}</span>{entry.recommended && <span className="quant-star" aria-label="Suggested">★</span>}
                {percent !== null && <span className="quant-progress" style={{ width: `${percent}%` }} />}
              </button>;
            })}
            {item.quants.length > featured(item.quants).length && <button className="quant-chip more" onClick={() => setSelected({ repo: item.repo, label: (item.quants.find((entry) => entry.recommended) ?? featured(item.quants)[0] ?? item.quants[0])!.label })}>+{item.quants.length - featured(item.quants).length} more</button>}
          </div>
        </li>)}
      </ul>}
    </div>
    {model && quant && <aside className="inspector find-detail" aria-label="Model detail">
      <div className="inspector-top"><Button variant="ghost" aria-label="Close detail" onClick={() => setSelected(null)}><Icon name="close" /></Button></div>
      <div className="inspector-body">
        <div className="inspector-title"><h2 className="break-word">{model.name}</h2></div>
        <p className="field-help">{[model.publisher, model.license, model.gated ? "Gated" : null].filter(Boolean).join(" · ")}</p>
        <div className="quant-list" role="listbox" aria-label="Quants">{[...model.quants].sort((left, right) => (left.bytes ?? 0) - (right.bytes ?? 0)).map((entry) => {
          const [label, tone] = fit(entry.bytes, gpu, ram);
          return <button key={entry.label} role="option" aria-selected={entry.label === quant.label} className="quant-option" onClick={() => setSelected({ repo: model.repo, label: entry.label })}>
            <span className={`status-dot ${tone}`} title={label} /><strong>{entry.label}</strong>{entry.recommended && <span className="quant-star" aria-label="Suggested">★</span>}{entry.present && <span className="muted">✓ downloaded</span>}<span className="numeric muted">{bytes(entry.bytes)}</span>
          </button>;
        })}</div>
        <dl className="facts">
          <div><dt>Selected</dt><dd>{quant.label}{quant.parts > 1 ? ` · ${quant.parts} files` : ""}</dd></div>
          <div><dt>This machine</dt><dd><Badge tone={fit(quant.bytes, gpu, ram)[1]}>{fit(quant.bytes, gpu, ram)[0]}</Badge></dd></div>
        </dl>
        {progress(quant) !== null && <><progress aria-label="Download progress" max={100} value={progress(quant)!} /><p className="field-help">Downloading… {Math.round(progress(quant)!)}%</p></>}
        <div className="action-row wrap">
          {quant.present ? <Button variant="primary" disabled={!online || quant.parts > 1 || !quant.sha256} title={quant.parts > 1 ? "Split models can't be added yet" : undefined} onClick={() => setAdding(true)}>Add model</Button>
            : progress(quant) === null && <Button variant="primary" disabled={!online || !quant.pullable || model.gated} onClick={() => void download(model, quant)}>Download</Button>}
        </div>
        {model.gated && !quant.present && <p className="field-help">This repository is gated on Hugging Face and can't be downloaded here yet.</p>}
      </div>
    </aside>}
    {adding && quant?.sha256 && <Dialog title={`Add ${model?.name ?? "model"}`} onClose={() => setAdding(false)}><Registration artifactId={quant.sha256} client={client} disabled={!online} onClose={() => setAdding(false)} onRegistered={async () => { await onRegistered(); }} /></Dialog>}
  </div>;
}
