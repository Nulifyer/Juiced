import { useEffect, useMemo, useRef, useState } from "react";
import type { Client } from "./api";
import { bytes } from "./domain";
import { gpuMemory, parseSystemInfo } from "./hardware";
import { AddModel, type AddTarget } from "./AddModel";
import { UrlDownload } from "./UrlDownload";
import { engineNames, parseSearch, progress, searchEngines, tasks, type LibraryModel, type Quant, type SearchResult } from "./libraryData";
import { Badge, Button, Empty, HelpTip, Icon, Menu } from "./ui";
import { LocalProgress, ServerFolder, useLocalFile } from "./LocalFiles";
import { FlmModels } from "./FlmModels";
import { notify } from "./toast";
import { useResource } from "./useResource";

const gpuOnly = new Set(["vllm", "thenoise"]);
function fit(size: number | null, vram: number | null, ram: number | null, engine?: string | null): [string, "good" | "warning" | "bad" | "quiet"] {
  if (size === null) return ["Size unknown", "quiet"];
  const needed = size * 1.15;
  if (vram !== null && needed <= vram) return ["Fits in GPU memory", "good"];
  if (engine && gpuOnly.has(engine)) return [vram ? "Too big for the GPU" : "Needs a supported GPU", "bad"];
  if (ram !== null && needed <= ram) return [vram ? "Too big for the GPU; runs partly on CPU" : "Runs on the CPU", "warning"];
  return ["Likely too big for this machine", "bad"];
}
const common = ["Q4_K_M", "Q5_K_M", "Q6_K", "Q8_0", "F16", "BF16", "MXFP4"];
function featured(quants: Quant[]): Quant[] {
  const picked = quants.filter((quant) => quant.recommended || quant.present || quant.jobs.length > 0 || quant.kind === "checkpoint" || common.includes(quant.label.toUpperCase()));
  return (picked.length ? picked : quants.slice(0, 4)).slice(0, 6).sort((left, right) => (left.bytes ?? 0) - (right.bytes ?? 0));
}
const compact = (value: number | null) => value === null ? null : value >= 1e6 ? `${(value / 1e6).toFixed(1)}M` : value >= 1e3 ? `${Math.round(value / 1e3)}k` : String(value);

const searchTimeout = 45000;
function modelId(file: string): string {
  const name = file.split(/[\\/]/).pop() ?? file;
  return name.replace(/\.(gguf|bin|safetensors|onnx)$/, "").trim();
}
export function FindModels({ client, online, onRegistered, onOpenModel, onDownloads }: { client: Client; online: boolean; onRegistered: () => Promise<void>; onOpenModel: (id: string) => void; onDownloads: () => void }) {
  const quantList = useRef<HTMLDivElement>(null);
  const [urlOpen, setUrlOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [task, setTask] = useState<string | null>(null);
  const [engine, setEngine] = useState<string | null>(null);
  const [result, setResult] = useState<SearchResult | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [hubPending, setHubPending] = useState(false);
  const [selected, setSelected] = useState<{ repo: string; label: string } | null>(null);
  const [notice, setNotice] = useState("");
  const local = useLocalFile(client);
  const [folderOpen, setFolderOpen] = useState(false);
  const picker = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const list = quantList.current, option = list?.querySelector<HTMLElement>('[aria-selected="true"]');
    if (list && option) list.scrollTop = option.offsetTop - list.offsetTop - list.clientHeight / 2 + option.offsetHeight / 2;
  }, [selected?.repo, selected?.label]);
  const [tick, setTick] = useState(0);
  const info = useResource(client, "system-info", parseSystemInfo, 0);
  const request = useRef<AbortController | null>(null);
  const lastKey = useRef("");
  useEffect(() => {
    const key = `${query.trim()}\u0000${task ?? ""}\u0000${engine ?? ""}`;
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
      if (engine) params.set("engine", engine);
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
  }, [client, query, task, engine, tick]);
  useEffect(() => () => request.current?.abort(), []);
  useEffect(() => { if (!notice) return; const timer = setTimeout(() => setNotice(""), 4000); return () => clearTimeout(timer); }, [notice]);
  const downloading = result?.models.some((model) => model.quants.some((quant) => progress(quant) !== null)) ?? false;
  useEffect(() => {
    if (!downloading) return;
    const timer = setInterval(() => setTick((value) => value + 1), 2000);
    return () => clearInterval(timer);
  }, [downloading]);
  const choose = (next: { repo: string; label: string } | null) => { if (next) local.clear(); setSelected(next); };
  const model = local.file ? undefined : result?.models.find((item) => item.repo === selected?.repo);
  const quant = model?.quants.find((item) => item.label === selected?.label);
  const gpu = info.data?.gpus.reduce((best, item) => Math.max(best, gpuMemory(item) ?? 0), 0) || null;
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
  const models = useMemo(() => result?.models ?? [], [result]);
  const file = local.file;
  const checkpoint = quant?.kind === "checkpoint";
  const target: AddTarget | null = file ? file.artifact ? { artifact: file.artifact } : file.path ? { path: file.path } : null : checkpoint ? quant?.set ? { artifact_set: quant.set } : null : quant?.localPath ? { path: quant.localPath } : quant?.sha256 ? { artifact: quant.sha256 } : null;
  const targetId = file ? file.id.trim() : checkpoint ? model?.name ?? "" : quant?.kind === "bundle" && model?.nativeId ? model.nativeId : quant ? modelId(quant.file) : "";
  const gatedNote = model?.gated && !quant?.present ? result?.authenticated ? "Gated on Hugging Face: accept its license there first. Juiced downloads with the server's HF_TOKEN." : "Gated on Hugging Face. Set HF_TOKEN on the server and accept the license there to download it." : "";
  const ready = file ? !!target && !file.error : !!quant?.present;
  const resetKey = file ? `file:${file.name}` : `${selected?.repo}:${selected?.label}`;
  return <div className={`find-models ${model || file ? "has-detail" : ""}`}>
    <div className="find-main">
      <div className="toolbar">
        <input type="search" aria-label="Search models" placeholder="Search models, e.g. qwen3, llama, whisper" value={query} onChange={(event) => setQuery(event.target.value)} />
        <select aria-label="Engine" value={engine ?? ""} onChange={(event) => setEngine(event.target.value || null)}><option value="">Any engine</option>{searchEngines.map(([id, label]) => <option key={id} value={id}>{label}</option>)}</select>
        <div className="chip-filter" role="group" aria-label="Task"><button aria-pressed={task === null} onClick={() => setTask(null)}>All</button>{tasks.map(([id, label]) => <button key={id} aria-pressed={task === id} onClick={() => setTask(task === id ? null : id)}>{label}</button>)}</div>
        <Menu label="Add a model file" trigger={<><Icon name="upload" size={14} /> Add file</>}>
          <button role="menuitem" disabled={!online} onClick={() => picker.current?.click()}>Upload from this computer…</button>
          <button role="menuitem" disabled={!online} onClick={() => setFolderOpen(true)}>Pick from the server…</button>
          <button role="menuitem" disabled={!online} onClick={() => setUrlOpen(true)}>Download from a URL…</button>
        </Menu>
        <input ref={picker} type="file" hidden accept=".gguf,.bin,.safetensors,.onnx" aria-label="Model file" onChange={(event) => { const picked = event.target.files?.[0]; event.target.value = ""; if (picked) { setSelected(null); void local.upload(picked); } }} />
      </div>
      {urlOpen && <UrlDownload client={client} onClose={() => setUrlOpen(false)} onStarted={() => { setUrlOpen(false); notify("Download started. Follow it under Downloads."); onDownloads(); }} />}
      {folderOpen && <ServerFolder client={client} onClose={() => setFolderOpen(false)} onPick={(entry, mode) => { setFolderOpen(false); setSelected(null); void local.pick(entry, mode); }} />}
      {error && <div className="action-row"><p className="inline-error" role="alert">{error}</p><Button onClick={() => setTick((value) => value + 1)}>Retry</Button></div>}
      {(task === null || task === "chat") && engine === null && <FlmModels client={client} online={online} query={query} onAdded={onRegistered} />}
      {result && hubPending && <p className="searching muted" role="status">Searching Hugging Face…</p>}
      {result && !hubPending && result.hub !== "ok" && result.hub !== "skipped" && <p className="field-help">{result.offline ? "Offline mode: showing recommended and downloaded models only." : "Hugging Face is unreachable right now; showing recommended and downloaded models."}</p>}
      {!result ? <p className="muted" role="status">{loading ? "Searching…" : ""}</p> : models.length === 0 ? <Empty title="No models found">Try a shorter search or another task.</Empty> : <ul className={`model-results ${loading ? "stale" : ""}`} aria-label="Search results" aria-busy={loading}>
        {models.map((item) => <li key={`${item.source}:${item.repo}`} className={`model-result ${item.repo === selected?.repo ? "selected" : ""}`}>
          <div className="result-head">
            <strong className="break-word">{item.name}</strong>
            {item.source === "catalog" && <Badge tone="good">Recommended</Badge>}
            {item.quants.some((entry) => entry.present) && <Badge tone="good">✓ On this machine</Badge>}
            <span className="muted result-meta">{[item.publisher, item.task, compact(item.downloads) && `${compact(item.downloads)} downloads`].filter(Boolean).join(" · ")}</span>
          </div>
          <div className="quant-chips" role="group" aria-label={`${item.name} quants`}>
            {item.quants.length === 0 ? <span className="muted">No downloadable files found</span> : featured(item.quants).map((entry) => {
              const percent = progress(entry);
              return <button key={entry.label} className={`quant-chip ${entry.present ? "present" : ""}`} aria-pressed={item.repo === selected?.repo && entry.label === selected?.label} onClick={() => choose({ repo: item.repo, label: entry.label })} title={entry.kind === "checkpoint" ? `${entry.files.length} files for ${engineNames[entry.engine ?? ""] ?? entry.engine}` : entry.file}>
                {entry.present && <span aria-label="Downloaded">✓ </span>}{entry.label}{entry.kind === "checkpoint" && entry.engine && <span className="quant-engine"> · {engineNames[entry.engine] ?? entry.engine}</span>}<span className="muted"> {bytes(entry.bytes)}</span>{entry.recommended && <span className="quant-star" aria-label="Suggested">★</span>}
                {percent !== null && <span className="quant-progress" style={{ width: `${percent}%` }} />}
              </button>;
            })}
            {item.quants.length > featured(item.quants).length && <button className="quant-chip more" onClick={() => choose({ repo: item.repo, label: (item.quants.find((entry) => entry.recommended) ?? featured(item.quants)[0] ?? item.quants[0])!.label })}>+{item.quants.length - featured(item.quants).length} more</button>}
          </div>
        </li>)}
      </ul>}
    </div>
    {file && <aside className="inspector find-detail" aria-label="Model detail">
      <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={local.clear}><Icon name="back" /> Back to results</Button><Button variant="ghost" className="close-inspector" aria-label="Close detail" onClick={local.clear}><Icon name="close" /></Button></div>
      <div className="inspector-body">
        <div className="inspector-title"><h2 className="break-word">{file.name}</h2></div>
        <dl className="facts">
          <div><dt>Size</dt><dd>{bytes(file.bytes)}</dd></div>
          <div><dt>Stored</dt><dd>{file.path ? "Used where it is" : file.job ? "Copied to library" : "Uploaded to library"}</dd></div>
          <div><dt>This machine</dt><dd><Badge tone={fit(file.bytes, gpu, ram)[1]}>{fit(file.bytes, gpu, ram)[0]}</Badge></dd></div>
        </dl>
        <LocalProgress file={file} onCancel={local.clear} />
        {ready && <div className="add-name"><label htmlFor="local-model-id">Model name</label><input id="local-model-id" value={file.id} maxLength={128} onChange={(event) => local.rename(event.target.value)} /></div>}
        <AddModel client={client} online={online} target={target} id={targetId} ready={ready} resetKey={resetKey} onRegistered={onRegistered} onOpenModel={onOpenModel} />
      </div>
    </aside>}
    {model && quant && <aside className="inspector find-detail" aria-label="Model detail">
      <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={() => choose(null)}><Icon name="back" /> Back to results</Button><Button variant="ghost" className="close-inspector" aria-label="Close detail" onClick={() => choose(null)}><Icon name="close" /></Button></div>
      <div className="inspector-body">
        <div className="inspector-title"><h2 className="break-word">{model.name}</h2></div>
        <p className="field-help">{[model.publisher, model.license, model.gated ? "Gated" : null].filter(Boolean).join(" · ")}</p>
        <div ref={quantList} className="quant-list" role="listbox" aria-label="Quants">{[...model.quants].sort((left, right) => (left.bytes ?? 0) - (right.bytes ?? 0)).map((entry) => {
          const [label, tone] = fit(entry.bytes, gpu, ram, entry.engine);
          return <button key={entry.label} role="option" aria-selected={entry.label === quant.label} className="quant-option" onClick={() => choose({ repo: model.repo, label: entry.label })}>
            <span className={`status-dot ${tone}`} title={label} /><strong>{entry.label}</strong>{entry.recommended && <span className="quant-star" aria-label="Suggested">★</span>}{entry.present && <span className="muted">✓ downloaded</span>}<span className="numeric muted">{bytes(entry.bytes)}</span>
          </button>;
        })}</div>
        <dl className="facts">
          <div><dt>Selected</dt><dd>{quant.label}{checkpoint ? ` · ${quant.files.length} files` : quant.parts > 1 ? ` · ${quant.parts} files` : ""}</dd></div>
          {quant.engine && <div><dt>Runs on</dt><dd>{engineNames[quant.engine] ?? quant.engine}</dd></div>}
          <div><dt>This machine</dt><dd><Badge tone={fit(quant.bytes, gpu, ram, quant.engine)[1]}>{fit(quant.bytes, gpu, ram, quant.engine)[0]}</Badge></dd></div>
          {quant.present && <div><dt>Stored</dt><dd>{quant.localPath ? `Hugging Face cache${quant.readOnly ? " · read-only" : ""}` : "Juiced storage"}</dd></div>}
        </dl>
        {checkpoint && <div className="checkpoint-files"><span className="field-label">Files <HelpTip label="Checkpoint files">The weights, config and tokenizer files the engine reads as one folder. READMEs, images and other formats are skipped.</HelpTip></span><ul aria-label="Checkpoint files">{quant.files.map((part) => <li key={part.file}><span className="break-word">{part.member ?? part.file}</span><span className="numeric muted">{bytes(part.bytes)}</span></li>)}</ul></div>}
        {progress(quant) !== null && <><progress aria-label="Download progress" max={100} value={progress(quant)!} /><p className="field-help">Downloading… {Math.round(progress(quant)!)}%</p></>}
        <AddModel client={client} online={online} target={target} id={targetId} engine={quant.engine === "thenoise" ? "thenoise" : undefined} ready={ready} resetKey={resetKey} blocked={!checkpoint && !quant.localPath && quant.parts > 1 ? "Split models can't be added yet" : undefined} onRegistered={onRegistered} onOpenModel={onOpenModel} fallback={progress(quant) === null && <Button variant="primary" disabled={!online || !quant.pullable || (model.gated && !result?.authenticated)} onClick={() => void download(model, quant)}>Download</Button>} />
        {gatedNote && <p className="field-help">{gatedNote}</p>}
      </div>
    </aside>}
  </div>;
}
