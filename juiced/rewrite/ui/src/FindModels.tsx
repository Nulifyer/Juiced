import { useEffect, useMemo, useRef, useState } from "react";
import { ApiError, type Client } from "./api";
import { installEngine, parseEngines, type EngineJob } from "./engines";
import { backendNames } from "./hardware";
import { bytes, policyNames, type Policy } from "./domain";
import { parseSystemInfo } from "./hardware";
import { parseSearch, progress, tasks, type LibraryModel, type Quant, type SearchResult } from "./libraryData";
import { Badge, Button, Empty, Icon } from "./ui";
import { notify } from "./toast";
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

const searchTimeout = 45000;
function modelId(file: string): string {
  const name = file.split(/[\\/]/).pop() ?? file;
  return name.replace(/\.(gguf|bin|safetensors|onnx)$/, "").trim();
}
type Install = { engine: string; backend: string; size: number | null };
export function FindModels({ client, online, onRegistered, onOpenModel }: { client: Client; online: boolean; onRegistered: () => Promise<void>; onOpenModel: (id: string) => void }) {
  const [needs, setNeeds] = useState<Install | null>(null);
  const [installing, setInstalling] = useState<EngineJob | null>(null);
  const [added, setAdded] = useState<{ model: string; backend: string; policy: Policy } | null>(null);
  const [addPolicy, setAddPolicy] = useState<Policy>("on-demand");
  const quantList = useRef<HTMLDivElement>(null);
  const [panelError, setPanelError] = useState("");
  const [query, setQuery] = useState("");
  const [task, setTask] = useState<string | null>(null);
  const [result, setResult] = useState<SearchResult | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [hubPending, setHubPending] = useState(false);
  const [selected, setSelected] = useState<{ repo: string; label: string } | null>(null);
  const [notice, setNotice] = useState("");
  const [adding, setAdding] = useState(false);
  useEffect(() => { setNeeds(null); setAdded(null); setPanelError(""); }, [selected?.repo, selected?.label]);
  useEffect(() => {
    const list = quantList.current, option = list?.querySelector<HTMLElement>('[aria-selected="true"]');
    if (list && option) list.scrollTop = option.offsetTop - list.offsetTop - list.clientHeight / 2 + option.offsetHeight / 2;
  }, [selected?.repo, selected?.label]);
  const [tick, setTick] = useState(0);
  const info = useResource(client, "system-info", parseSystemInfo, 0);
  const request = useRef<AbortController | null>(null);
  const lastKey = useRef("");
  useEffect(() => {
    const key = `${query.trim()}\u0000${task ?? ""}`;
    const refreshOnly = key === lastKey.current;
    lastKey.current = key;
    if (refreshOnly && request.current) return;
    const timer = setTimeout(async () => {
      request.current?.abort();
      const controller = new AbortController(); request.current = controller;
      const limit = setTimeout(() => controller.abort(), searchTimeout);
      setLoading(true); setHubPending(true);
      const params = new URLSearchParams({ limit: "12" });
      if (query.trim()) params.set("q", query.trim());
      if (task) params.set("task", task);
      let full = false;
      const fast = refreshOnly ? Promise.resolve() : client.request(`juiced/library/search?${params}&hub=false`, undefined, controller.signal).then((value) => {
        if (!full && !controller.signal.aborted) { setResult(parseSearch(value)); setLoading(false); }
      }).catch(() => undefined);
      try {
        const next = parseSearch(await client.request(`juiced/library/search?${params}`, undefined, controller.signal));
        full = true;
        if (!controller.signal.aborted) { setResult(next); setError(""); }
      } catch (failure) {
        if (request.current === controller) setError(controller.signal.aborted ? "Hugging Face didn't answer within 45 seconds. Recommended and downloaded models are still shown." : failure instanceof Error ? failure.message : "Search failed.");
      } finally {
        clearTimeout(limit);
        await fast;
        if (request.current === controller) { setLoading(false); setHubPending(false); request.current = null; }
      }
    }, refreshOnly ? 0 : 300);
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
      notify(`Downloading ${target.name} ${item.label}.`);
      setTick((value) => value + 1);
    } catch (failure) { setError(failure instanceof Error ? failure.message : "The download didn't start."); }
  }
  async function add(item: Quant, install?: Install) {
    if (!item.sha256) return;
    setAdding(true); setPanelError("");
    try {
      if (install) await installEngine(client, install.engine, install.backend, setInstalling);
      setInstalling(null);
      const response = await client.request<{ model: string | { id?: string }; backend: string }>("juiced/models/add", { artifact: item.sha256, id: modelId(item.file), policy: addPolicy });
      const id = typeof response.model === "string" ? response.model : response.model?.id ?? "the model";
      setNeeds(null); setAdded({ model: id, backend: response.backend, policy: addPolicy });
      await onRegistered();
    } catch (failure) {
      setInstalling(null);
      const body = failure instanceof ApiError ? failure.body as { install?: { engine?: string; backend?: string } } | undefined : undefined;
      if (failure instanceof ApiError && failure.code === "engine_not_installed" && body?.install?.engine && body.install.backend) {
        const { engine, backend } = body.install;
        const listed = await client.request("juiced/engines").then(parseEngines).catch(() => null);
        setNeeds({ engine, backend, size: listed?.installable.find((row) => row.engine === engine && row.backend === backend)?.archive_bytes ?? null });
      }
      else setPanelError(failure instanceof ApiError && failure.code === "model_exists" ? "A model with this name is already added. Rename or remove it under Models first." : failure instanceof Error ? failure.message : "The model couldn't be added.");
    } finally { setAdding(false); }
  }
  const models = useMemo(() => result?.models ?? [], [result]);
  return <div className={`find-models ${model ? "has-detail" : ""}`}>
    <div className="find-main">
      <div className="toolbar">
        <input type="search" aria-label="Search models" placeholder="Search models, e.g. qwen3, llama, whisper" value={query} onChange={(event) => setQuery(event.target.value)} />
        <div className="chip-filter" role="group" aria-label="Task"><button aria-pressed={task === null} onClick={() => setTask(null)}>All</button>{tasks.map(([id, label]) => <button key={id} aria-pressed={task === id} onClick={() => setTask(task === id ? null : id)}>{label}</button>)}</div>
      </div>
      {error && <div className="action-row"><p className="inline-error" role="alert">{error}</p><Button onClick={() => setTick((value) => value + 1)}>Retry</Button></div>}
      {result && hubPending && <p className="searching muted" role="status">Searching Hugging Face…</p>}
      {result && !hubPending && result.hub !== "ok" && result.hub !== "skipped" && <p className="field-help">{result.offline ? "Offline mode: showing recommended and downloaded models only." : "Hugging Face is unreachable right now; showing recommended and downloaded models."}</p>}
      {!result ? <p className="muted" role="status">{loading ? "Searching…" : ""}</p> : models.length === 0 ? <Empty title="No models found">Try a shorter search or another task.</Empty> : <ul className={`model-results ${loading ? "stale" : ""}`} aria-label="Search results" aria-busy={loading}>
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
      <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={() => setSelected(null)}><Icon name="back" /> Back to results</Button><Button variant="ghost" className="close-inspector" aria-label="Close detail" onClick={() => setSelected(null)}><Icon name="close" /></Button></div>
      <div className="inspector-body">
        <div className="inspector-title"><h2 className="break-word">{model.name}</h2></div>
        <p className="field-help">{[model.publisher, model.license, model.gated ? "Gated" : null].filter(Boolean).join(" · ")}</p>
        <div ref={quantList} className="quant-list" role="listbox" aria-label="Quants">{[...model.quants].sort((left, right) => (left.bytes ?? 0) - (right.bytes ?? 0)).map((entry) => {
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
        {added ? <div className="added"><p className="inline-success" role="status">Added {added.model}. It runs on {backendNames[added.backend] ?? added.backend} and {added.policy === "hot" ? "loads now and stays loaded" : added.policy === "on-demand" ? "loads on its first request" : "stays Disabled until you enable it in Models"}.</p><div className="action-row"><Button variant="primary" onClick={() => onOpenModel(added.model)}>Open model</Button></div></div>
          : <>{quant.present && <div className="add-policy"><span className="field-label" id="add-policy-label">Make available as</span><div className="segmented" role="radiogroup" aria-labelledby="add-policy-label">{(["disabled", "on-demand", "hot"] as Policy[]).map((policy) => <button key={policy} role="radio" aria-checked={addPolicy === policy} disabled={adding} onClick={() => setAddPolicy(policy)}>{policyNames[policy]}</button>)}</div></div>}
          {needs ? <div className="needs-engine"><p className="field-help">This model needs the {needs.engine} {backendNames[needs.backend] ?? needs.backend} engine{needs.size ? ` (${bytes(needs.size)} download)` : ""}. Juiced picked it for this machine.</p>
            {installing ? <><progress aria-label="Engine install progress" max={installing.total_bytes ?? 1} value={installing.total_bytes ? installing.downloaded_bytes : undefined} /><p className="field-help">Installing engine… {installing.state}</p></> : <Button variant="primary" disabled={!online || adding} onClick={() => void add(quant, needs)}>Install engine and add</Button>}</div>
          : <div className="action-row wrap">
          {quant.present ? <Button variant="primary" disabled={!online || adding || quant.parts > 1 || !quant.sha256} title={quant.parts > 1 ? "Split models can't be added yet" : undefined} onClick={() => void add(quant)}>{adding ? "Adding…" : "Add model"}</Button>
            : progress(quant) === null && <Button variant="primary" disabled={!online || !quant.pullable || model.gated} onClick={() => void download(model, quant)}>Download</Button>}
        </div>}</>}
        {panelError && <p className="inline-error" role="alert">{panelError}</p>}
        {model.gated && !quant.present && <p className="field-help">This repository is gated on Hugging Face and can't be downloaded here yet.</p>}
      </div>
    </aside>}
  </div>;
}
