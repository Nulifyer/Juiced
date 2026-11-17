import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ApiError, type Client } from "./api";
import { bytes, policyNames, type Model, type Policy } from "./domain";
import { gpuMemory, parseSystemInfo } from "./hardware";
import { AddModel, type AddTarget } from "./AddModel";
import { UrlDownload } from "./UrlDownload";
import { parseArtifactInventory } from "./management";
import { engineNames, featured, parseSearch, progress, quantKey, quantNames, searchEngines, tasks, type LibraryModel, type Quant, type SearchResult } from "./libraryData";
import { Badge, Button, Empty, HelpTip, Icon, Menu } from "./ui";
import { LocalProgress, ProjectorChoice, ServerFolder, localTarget, useLocalFile } from "./LocalFiles";
import { splitPart } from "./localFilesData";
import { FlmModels, useFlm } from "./FlmModels";
import { parsePulls, type LocalInventory, type PullView } from "./localLibraryData";
import { plainError } from "./modelErrors";
import { QuantList, policyWord, quantWords, type QuantItem } from "./QuantList";
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
const compact = (value: number | null) => value === null ? null : value >= 1e6 ? `${(value / 1e6).toFixed(1)}M` : value >= 1e3 ? `${Math.round(value / 1e3)}k` : String(value);

const taskNames: Record<string, string> = Object.fromEntries(tasks);
const npu = "flm";
const searchTimeout = 45000;
function modelId(file: string): string {
  const name = file.split(/[\\/]/).pop() ?? file;
  return (splitPart(name)?.prefix ?? name.replace(/\.(gguf|bin|safetensors|onnx)$/, "")).trim();
}
/** A repository to open on arrival, with one of its quants when `file` or `label` is set. */
export type DiscoverTarget = { repo: string; file?: string; label?: string };

const baseName = (path: string) => path.split(/[\\/]/).pop() ?? path;
/** The models that run a hub quant's files, so Discover can say what already uses it. */
export function quantUsers(local: LocalInventory | null | undefined, repo: string, quant: Pick<Quant, "file" | "kind" | "present">): string[] {
  const group = local?.groups.find((item) => item.repo?.toLowerCase() === repo.toLowerCase());
  if (!group) return [];
  if (quant.kind === "checkpoint") return quant.present ? group.usedBy : [];
  const name = baseName(quant.file);
  return group.quants.find((item) => item.files.some((id) => group.files.find((file) => file.id === id)?.name === name))?.usedBy ?? [];
}

/** The state word a hub quant shows, matching the lists under Installed. */
export function hubQuantState(quant: Quant, users: string[], models: Pick<Model, "id" | "policy">[]): string {
  const percent = progress(quant);
  if (percent !== null) return quantWords.downloading(percent);
  if (users.length) return policyWord(models.find((model) => model.id === users[0])?.policy, users.length);
  if (quant.present) return quantWords.unadded;
  if (quant.parts > 1 && quant.presentFiles > 0) return quantWords.parts(quant.presentFiles, quant.parts);
  return "";
}

export function FindModels({ client, online, onRegistered, onOpenModel, onDownloads, initial, local: inventory, models: registered = [] }: { client: Client; online: boolean; onRegistered: () => Promise<void>; onOpenModel: (id: string) => void; onDownloads: () => void; initial?: DiscoverTarget; local?: LocalInventory | null; models?: Pick<Model, "id" | "policy">[] }) {
  const quantList = useRef<HTMLDivElement>(null);
  const [urlOpen, setUrlOpen] = useState(false);
  const [query, setQuery] = useState(initial?.repo ?? "");
  const [task, setTask] = useState<string | null>(null);
  const [engine, setEngine] = useState<string | null>(null);
  const [result, setResult] = useState<SearchResult | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [hubPending, setHubPending] = useState(false);
  const [selected, setSelected] = useState<{ repo: string; quant: string } | null>(null);
  const wanted = useRef(initial);
  const [notice, setNotice] = useState("");
  const local = useLocalFile(client);
  const [folderOpen, setFolderOpen] = useState(false);
  const picker = useRef<HTMLInputElement>(null);
  const listed = useRef("");
  useLayoutEffect(() => {
    const list = quantList.current, option = list?.querySelector<HTMLElement>('[aria-selected="true"]');
    if (!list || !option) { listed.current = ""; return; }
    const top = option.offsetTop - list.offsetTop, bottom = top + option.offsetHeight;
    // A row the operator clicked stays under the pointer; only a newly opened list centers its selection.
    if (listed.current !== selected?.repo) list.scrollTop = top - list.clientHeight / 2 + option.offsetHeight / 2;
    else if (top < list.scrollTop) list.scrollTop = top;
    else if (bottom > list.scrollTop + list.clientHeight) list.scrollTop = bottom - list.clientHeight;
    listed.current = selected?.repo ?? "";
  }, [selected?.repo, selected?.quant]);
  const [tick, setTick] = useState(0);
  const info = useResource(client, "system-info", parseSystemInfo, 0);
  const flm = useFlm(client);
  const npuOnly = engine === npu;
  const request = useRef<AbortController | null>(null);
  const lastKey = useRef("");
  useEffect(() => {
    const key = `${query.trim()}\u0000${task ?? ""}\u0000${engine ?? ""}`;
    const refreshOnly = key === lastKey.current;
    lastKey.current = key;
    if (engine === npu) { request.current?.abort(); request.current = null; setLoading(false); setHubPending(false); return; }
    if (refreshOnly && request.current) return;
    const timer = setTimeout(async () => {
      request.current?.abort();
      const controller = new AbortController(); request.current = controller;
      const limit = setTimeout(() => controller.abort(), searchTimeout);
      if (!refreshOnly) { setLoading(true); setHubPending(true); }
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
        if (!controller.signal.aborted) { setResult(next); setError(""); open(next); }
      } catch (failure) {
        if (request.current === controller) setError(controller.signal.aborted ? "Hugging Face didn't answer in 45 seconds. Recommended and downloaded models are still listed." : plainError(failure, "Search failed."));
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
  const choose = (next: { repo: string; quant: string } | null) => { if (next) local.clear(); setSelected(next); };
  const pick = (item: LibraryModel, entry: Quant | undefined) => { if (entry) choose({ repo: item.repo, quant: quantKey(entry) }); };
  function open(found: SearchResult) {
    const target = wanted.current;
    const item = target && found.models.find((entry) => entry.repo.toLowerCase() === target.repo.toLowerCase());
    if (!target || !item) return;
    wanted.current = undefined;
    pick(item, item.quants.find((entry) => entry.file === target.file) ?? (target.label ? item.quants.find((entry) => entry.label === target.label) : undefined) ?? item.quants.find((entry) => entry.recommended) ?? featured(item.quants)[0]);
  }
  const model = local.file || npuOnly ? undefined : result?.models.find((item) => item.repo === selected?.repo);
  const quant = model?.quants.find((item) => quantKey(item) === selected?.quant);
  const pulls = useResource(client, "juiced/library/pulls", parsePulls, 2000, true, !!quant);
  const pull: PullView | undefined = quant && model ? pulls.data?.find((item) => item.then?.kind === "add" && item.repo.toLowerCase() === model.repo.toLowerCase() && (item.label ?? "Unknown") === quant.label && (!item.files.length || item.files.some((part) => part.file === quant.file))) : undefined;
  const pendingAdd = pull?.then?.state === "pending" ? pull.then : null;
  const seen = useRef(new Set<string>());
  if (pull && pendingAdd) seen.current.add(pull.id);
  const added = pull && seen.current.has(pull.id) && pull.state === "complete" && pull.then?.state === "done" ? pull.then.model : null;
  const settled = useRef(new Set<string>());
  useEffect(() => {
    if (!pull || !added || settled.current.has(pull.id)) return;
    settled.current.add(pull.id);
    void onRegistered();
  }, [pull, added, onRegistered]);
  const gpu = info.data?.gpus.reduce((best, item) => Math.max(best, gpuMemory(item) ?? 0), 0) || null;
  const ram = info.data?.memory?.total_bytes ?? null;
  async function download(target: LibraryModel, item: Quant, policy: Policy, then: { id: string; engine?: string }) {
    if (!result) return;
    setError(""); setNotice("");
    try {
      const pull = (generation: number) => client.request<{ pull?: string }>("juiced/library/pull", { repo: target.repo, file: item.file, expected_generation: generation, then: { policy, ...(then.id ? { id: then.id } : {}), ...(then.engine ? { engine: then.engine } : {}) } });
      // Another download finishing a part moves the generation; starting this one doesn't depend on that, and a repeated pull joins the first.
      const started = await pull(result.generation).catch(async (failure) => {
        if (!(failure instanceof ApiError) || failure.code !== "artifact_revision_conflict") throw failure;
        return pull(parseArtifactInventory(await client.request("juiced/artifacts?section=jobs&limit=1")).generation);
      });
      if (started.pull) seen.current.add(started.pull);
      notify(`Downloading ${target.name} ${item.label}. It's added as ${policyNames[policy]} when done.`);
      setTick((value) => value + 1);
      await pulls.reload();
    } catch (failure) { setError(plainError(failure, "The download didn't start.")); }
  }
  const models = useMemo(() => result?.models ?? [], [result]);
  const names = useMemo(() => quantNames(model?.quants ?? []), [model]);
  const file = local.file;
  const checkpoint = quant?.kind === "checkpoint";
  const split = !checkpoint && (quant?.parts ?? 1) > 1;
  const target: AddTarget | null = file ? localTarget(file) : checkpoint || split ? quant?.set ? { artifact_set: quant.set } : split && quant?.localPath ? { path: quant.localPath } : null : quant?.localPath ? { path: quant.localPath } : quant?.sha256 ? { artifact: quant.sha256 } : null;
  const targetId = file ? file.id.trim() : checkpoint ? model?.name ?? "" : quant?.kind === "bundle" && model?.nativeId ? model.nativeId : quant ? modelId(quant.file) : "";
  const gatedNote = model?.gated && !quant?.present ? result?.authenticated ? "Gated. Accept its license on Hugging Face first." : "Gated. Set HF_TOKEN on the server and accept its license on Hugging Face." : "";
  const users = model && quant ? quantUsers(inventory, model.repo, quant) : [];
  const partial = !!quant && !quant.present && quant.parts > 1 && quant.presentFiles > 0;
  const ready = file ? !!target && !file.error : !!quant?.present;
  const resetKey = file ? `file:${file.name}` : `${selected?.repo}:${selected?.quant}`;
  return <div className={`find-models ${model || file ? "has-detail" : ""}`}>
    <div className="find-main">
      <div className="toolbar">
        <input type="search" aria-label="Search models" placeholder="Search models, e.g. qwen3, llama, whisper" value={query} onChange={(event) => setQuery(event.target.value)} />
        <select aria-label="Engine" value={engine ?? ""} onChange={(event) => { setEngine(event.target.value || null); setSelected(null); }}><option value="">Any engine</option>{searchEngines.map(([id, label]) => <option key={id} value={id}>{label}</option>)}{flm.data?.available && <option value={npu}>FastFlowLM (NPU)</option>}</select>
        <div className="chip-filter" role="group" aria-label="Task"><button aria-pressed={task === null} onClick={() => setTask(null)}>All</button>{tasks.map(([id, label]) => <button key={id} aria-pressed={task === id} onClick={() => setTask(task === id ? null : id)}>{label}</button>)}</div>
        <Menu label="Add a model file" trigger={<><Icon name="upload" size={14} /> Add file</>}>
          <button role="menuitem" disabled={!online} onClick={() => picker.current?.click()}>Upload from this computer…</button>
          <button role="menuitem" disabled={!online} onClick={() => setFolderOpen(true)}>Pick from the server…</button>
          <button role="menuitem" disabled={!online} onClick={() => setUrlOpen(true)}>Download from a URL…</button>
        </Menu>
        <input ref={picker} type="file" multiple hidden accept=".gguf,.bin,.safetensors,.onnx" aria-label="Model file" onChange={(event) => { const picked = Array.from(event.target.files ?? []); event.target.value = ""; if (picked.length) { setSelected(null); void local.upload(picked); } }} />
      </div>
      {urlOpen && <UrlDownload client={client} onClose={() => setUrlOpen(false)} onStarted={() => { setUrlOpen(false); notify("Download started. Follow it under Downloads."); onDownloads(); }} />}
      {folderOpen && <ServerFolder client={client} onClose={() => setFolderOpen(false)} onPick={(entry, mode) => { setFolderOpen(false); setSelected(null); void local.pick(entry, mode); }} />}
      {error && <div className="action-row"><p className="inline-error" role="alert">{error}</p><Button onClick={() => setTick((value) => value + 1)}>Retry</Button></div>}
      {npuOnly ? <FlmModels client={client} online={online} query={query} flm={flm} onAdded={onRegistered} /> : <>
      {result && <p className="searching muted" role="status">{hubPending ? "Searching Hugging Face…" : result.hub !== "ok" && result.hub !== "skipped" ? result.offline ? "Offline mode. Only recommended and downloaded models are listed." : "Hugging Face is unreachable. Only recommended and downloaded models are listed." : ""}</p>}
      {!result ? <p className="muted" role="status">{loading ? "Searching…" : ""}</p> : models.length === 0 ? <Empty title="No models found">Try a shorter search or another task.</Empty> : <ul className={`model-results ${loading ? "stale" : ""}`} aria-label="Search results" aria-busy={loading}>
        {models.map((item) => <li key={`${item.source}:${item.repo}`} className={`model-result ${item.repo === selected?.repo ? "selected" : ""}`}>
          <div className="result-head">
            <strong className="break-word">{item.name}</strong>
            {item.source === "catalog" && <Badge tone="good">Recommended</Badge>}
            {item.quants.some((entry) => entry.present) && <Badge tone="good">✓ On this machine</Badge>}
            <span className="muted result-meta">{[item.publisher, item.task && (taskNames[item.task] ?? item.task), compact(item.downloads) && `${compact(item.downloads)} downloads`].filter(Boolean).join(" · ")}</span>
          </div>
          <div className="quant-chips" role="group" aria-label={`${item.name} quants`}>
            {item.quants.length === 0 ? <span className="muted">No downloadable files found</span> : featured(item.quants).map((entry) => {
              const percent = progress(entry);
              return <button key={quantKey(entry)} className={`quant-chip ${entry.present ? "present" : ""}`} aria-pressed={item.repo === selected?.repo && quantKey(entry) === selected?.quant} onClick={() => pick(item, entry)} title={entry.kind === "checkpoint" ? `${entry.files.length} files for ${engineNames[entry.engine ?? ""] ?? entry.engine}` : entry.file}>
                {entry.present && <span aria-label="Downloaded">✓ </span>}{entry.label}{entry.kind === "checkpoint" && entry.engine && <span className="quant-engine"> · {engineNames[entry.engine] ?? entry.engine}</span>}<span className="muted"> {bytes(entry.bytes)}</span>{entry.recommended && <span className="quant-star" aria-label="Suggested">★</span>}
                {percent !== null && <span className="quant-progress" style={{ width: `${percent}%` }} />}
              </button>;
            })}
            {item.quants.length > featured(item.quants).length && <button className="quant-chip more" onClick={() => pick(item, item.quants.find((entry) => entry.recommended) ?? featured(item.quants)[0] ?? item.quants[0])}>+{item.quants.length - featured(item.quants).length} more</button>}
          </div>
        </li>)}
      </ul>}
      {(task === null || task === "chat") && engine === null && <FlmModels client={client} online={online} query={query} flm={flm} onAdded={onRegistered} />}
      </>}
    </div>
    {file && <aside className="inspector find-detail" aria-label="Model detail">
      <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={local.clear}><Icon name="back" /> Back to results</Button><Button variant="ghost" className="close-inspector" aria-label="Close detail" onClick={local.clear}><Icon name="close" /></Button></div>
      <div className="inspector-body">
        <div className="inspector-title"><h2 className="break-word">{file.name}</h2></div>
        <dl className="facts">
          <div><dt>Size</dt><dd>{bytes(file.bytes)}{file.partCount > 1 ? ` · ${file.partCount} parts` : ""}</dd></div>
          <div><dt>Stored</dt><dd>{file.path ? "Used where it is" : file.job ? "Copied to library" : "Uploaded to library"}</dd></div>
          {!file.projectors?.length && file.projector && <div><dt>Vision projector</dt><dd className="break-word">{file.projector.name}</dd></div>}
          <div><dt>This machine</dt><dd><Badge tone={fit(file.bytes, gpu, ram)[1]}>{fit(file.bytes, gpu, ram)[0]}</Badge></dd></div>
        </dl>
        <LocalProgress file={file} onCancel={local.clear} />
        {ready && <div className="add-name"><label htmlFor="local-model-id">Model name</label><input id="local-model-id" value={file.id} maxLength={128} onChange={(event) => local.rename(event.target.value)} /></div>}
        {ready && <ProjectorChoice file={file} onChoose={local.chooseProjector} />}
        <AddModel client={client} online={online} target={target} id={targetId} ready={ready} resetKey={resetKey} onRegistered={onRegistered} onOpenModel={onOpenModel} />
      </div>
    </aside>}
    {model && quant && <aside className="inspector find-detail" aria-label="Model detail">
      <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={() => choose(null)}><Icon name="back" /> Back to results</Button><Button variant="ghost" className="close-inspector" aria-label="Close detail" onClick={() => choose(null)}><Icon name="close" /></Button></div>
      <div className="inspector-body">
        <div className="inspector-title"><h2 className="break-word">{model.name}</h2></div>
        <p className="field-help">{[model.publisher, model.license, model.gated ? "Gated" : null].filter(Boolean).join(" · ")}</p>
        <QuantList listRef={quantList} label="Quants" selected={quantKey(quant)} onSelect={(key) => pick(model, model.quants.find((entry) => quantKey(entry) === key))} items={[...model.quants].sort((left, right) => (left.bytes ?? 0) - (right.bytes ?? 0)).map((entry): QuantItem => {
          const [label, tone] = fit(entry.bytes, gpu, ram, entry.engine);
          return { key: quantKey(entry), label: names.get(entry) ?? entry.label, bytes: entry.bytes, title: entry.file, suggested: entry.recommended, fit: { tone, label }, progress: progress(entry), state: hubQuantState(entry, quantUsers(inventory, model.repo, entry), registered) };
        })} />
        <dl className="facts">
          <div><dt>This machine</dt><dd><Badge tone={fit(quant.bytes, gpu, ram, quant.engine)[1]}>{fit(quant.bytes, gpu, ram, quant.engine)[0]}</Badge></dd></div>
          {quant.engine && <div><dt>Runs on</dt><dd>{engineNames[quant.engine] ?? quant.engine}</dd></div>}
          {(checkpoint || quant.parts > 1) && <div><dt>Files</dt><dd>{checkpoint ? quant.files.length : partial ? `${quantWords.parts(quant.presentFiles, quant.parts)} on disk` : `${quant.parts} parts`}</dd></div>}
          {quant.present && <div><dt>Stored</dt><dd>{quant.localPath ? `Hugging Face cache${quant.readOnly ? " · read-only" : ""}` : "Juiced storage"}</dd></div>}
          {users.length > 0 && <div><dt>Used by</dt><dd><span className="used-by">{users.map((id) => <button key={id} className="link-button" onClick={() => onOpenModel(id)}>{id}</button>)}</span></dd></div>}
        </dl>
        {checkpoint && <div className="checkpoint-files"><span className="field-label">Files <HelpTip label="Checkpoint files">The weights, config and tokenizer files the engine reads as one folder. READMEs, images and other formats are skipped.</HelpTip></span><ul aria-label="Checkpoint files">{quant.files.map((part) => <li key={part.file}><span className="break-word">{part.member ?? part.file}</span><span className="numeric muted">{bytes(part.bytes)}</span></li>)}</ul></div>}
        {progress(quant) !== null && <><progress aria-label="Download progress" max={100} value={progress(quant)!} /><p className="field-help">{quantWords.downloading(progress(quant)!)}{pendingAdd?.policy ? ` · then added as ${policyNames[pendingAdd.policy as Policy] ?? pendingAdd.policy}` : ""}</p></>}
        {pendingAdd && progress(quant) === null && <p className="muted" role="status">Adding the model…</p>}
        {added ? <div className="added"><p className="inline-success" role="status">Added {added}.</p><div className="action-row"><Button variant="primary" onClick={() => onOpenModel(added)}>Open model</Button></div></div>
          : !pendingAdd && <>
            {pull && seen.current.has(pull.id) && pull.then?.state === "failed" && <p className="inline-warning">{pull.then.message ?? "The model wasn't added."} Add it here.</p>}
            <AddModel client={client} online={online} target={target} id={targetId} engine={quant.engine === "thenoise" ? "thenoise" : undefined} ready={ready} resetKey={resetKey} blocked={split && !target ? "Every part must finish downloading first" : undefined} downloadLabel={partial ? "Finish download" : undefined} onRegistered={onRegistered} onOpenModel={onOpenModel} download={progress(quant) === null && !ready ? { disabled: !quant.pullable || (model.gated && !result?.authenticated), start: (policy) => download(model, quant, policy, { id: targetId, engine: quant.engine === "thenoise" ? "thenoise" : undefined }) } : undefined} />
          </>}
        {gatedNote && <p className="field-help">{gatedNote}</p>}
      </div>
    </aside>}
  </div>;
}
