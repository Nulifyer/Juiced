import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ApiError, type Client } from "./api";
import { bytes, policyNames, type Model, type Policy } from "./domain";
import { gpuMemory, parseSystemInfo } from "./hardware";
import { AddModel, type AddTarget } from "./AddModel";
import { UrlDownload } from "./UrlDownload";
import { parseArtifactInventory } from "./management";
import { engineNames, featured, filterResults, fitLabel, flmJob, flmResults, npuEngine as npu, parseFlm, parseSearch, progress, quantFit, quantKey, quantNames, searchEngines, sizeBuckets, sorts, tasks, type FitFilter, type LibraryModel, type Machine, type Quant, type SearchResult, type SizeBucket, type Sort } from "./libraryData";
import { Badge, Button, Empty, HelpTip, Icon, Menu } from "./ui";
import { LocalProgress, ProjectorChoice, ServerFolder, localTarget, useLocalFile } from "./LocalFiles";
import { splitPart } from "./localFilesData";
import { parsePulls, type LocalInventory, type LocalQuant, type PullView } from "./localLibraryData";
import { plainError, thenFailure } from "./modelErrors";
import { RemoveModel } from "./RemoveModel";
import { QuantList, policyWord, quantWords, type QuantItem } from "./QuantList";
import { notify } from "./toast";
import { useResource } from "./useResource";

const fit = (size: number | null, machine: Machine, engine?: string | null) => fitLabel(quantFit(size, engine, machine), engine, machine);
const compact = (value: number | null) => value === null ? null : value >= 1e6 ? `${(value / 1e6).toFixed(1)}M` : value >= 1e3 ? `${Math.round(value / 1e3)}k` : String(value);

const taskNames: Record<string, string> = Object.fromEntries(tasks);
const searchTimeout = 45000;
function modelId(file: string): string {
  const name = file.split(/[\\/]/).pop() ?? file;
  return (splitPart(name)?.prefix ?? name.replace(/\.(gguf|bin|safetensors|onnx)$/, "")).trim();
}
/** A repository to open on arrival, with one of its quants when `file` or `label` is set. */
export type DiscoverTarget = { repo: string; file?: string; label?: string };

const baseName = (path: string) => path.split(/[\\/]/).pop() ?? path;
/** The quant on disk that holds a hub quant's files. */
export function diskQuant(local: LocalInventory | null | undefined, repo: string, quant: Pick<Quant, "file">): LocalQuant | undefined {
  const group = local?.groups.find((item) => item.repo?.toLowerCase() === repo.toLowerCase());
  const name = baseName(quant.file);
  return group?.quants.find((item) => item.files.some((id) => group.files.find((file) => file.id === id)?.name === name));
}

/** The models that run a hub quant's files, so Discover can say what already uses it. */
export function quantUsers(local: LocalInventory | null | undefined, repo: string, quant: Pick<Quant, "file" | "kind" | "present"> & { users?: string[] | null }): string[] {
  if (quant.users) return quant.users;
  const group = local?.groups.find((item) => item.repo?.toLowerCase() === repo.toLowerCase());
  if (!group) return [];
  if (quant.kind === "checkpoint") return quant.present ? group.usedBy : [];
  return diskQuant(local, repo, quant)?.usedBy ?? [];
}

/** The state word a hub quant shows, matching the lists under Installed. */
export function hubQuantState(quant: Quant, users: string[], models: Pick<Model, "id" | "policy">[], onDisk?: LocalQuant): string {
  const percent = progress(quant);
  if (percent !== null) return quantWords.downloading(percent);
  if (users.length) return policyWord(models.find((model) => model.id === users[0])?.policy, users.length);
  if (quant.present) return onDisk?.install && onDisk.install.state !== "pending" ? quantWords.failed : quantWords.installing;
  if (quant.parts > 1 && quant.presentFiles > 0) return quantWords.parts(quant.presentFiles, quant.parts);
  return "";
}

export function FindModels({ client, online, onRegistered, onOpenModel, onDownloads, initial, local: inventory, models: registered = [] }: { client: Client; online: boolean; onRegistered: () => Promise<void>; onOpenModel: (id: string) => void; onDownloads: () => void; initial?: DiscoverTarget; local?: LocalInventory | null; models?: Pick<Model, "id" | "policy">[] }) {
  const quantList = useRef<HTMLDivElement>(null);
  const [urlOpen, setUrlOpen] = useState(false);
  const [query, setQuery] = useState(initial?.repo ?? "");
  const [task, setTask] = useState<string | null>(null);
  const [engine, setEngine] = useState<string | null>(null);
  const [fitFilter, setFitFilter] = useState<FitFilter | null>(null);
  const [size, setSize] = useState<SizeBucket | null>(null);
  const [sort, setSort] = useState<Sort>("match");
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
  const [npuBusy, setNpuBusy] = useState(false);
  const flm = useResource(client, "juiced/flm", parseFlm, npuBusy ? 2000 : 10000, true);
  const npuWorking = flm.data?.jobs.some((job) => job.state === "pulling" || (job.state === "complete" && job.then?.state === "pending")) ?? false;
  useEffect(() => setNpuBusy(npuWorking), [npuWorking]);
  const npuAdded = flm.data?.jobs.filter((job) => job.then?.state === "done" && job.then.model).map((job) => job.then!.model).join("\u0000") ?? "";
  const npuSeen = useRef<string | null>(null);
  useEffect(() => {
    if (!flm.data) return;
    if (npuSeen.current !== null && npuSeen.current !== npuAdded) void onRegistered();
    npuSeen.current = npuAdded;
  }, [flm.data, npuAdded, onRegistered]);
  const addedHere = useRef(new Set<string>());
  const installedHere = useRef(new Set<string>());
  const npuOnly = engine === npu || fitFilter === "npu";
  const request = useRef<AbortController | null>(null);
  const lastKey = useRef("");
  useEffect(() => {
    const key = `${query.trim()}\u0000${task ?? ""}\u0000${engine ?? ""}\u0000${npuOnly}`;
    const refreshOnly = key === lastKey.current;
    lastKey.current = key;
    if (npuOnly) { request.current?.abort(); request.current = null; setLoading(false); setHubPending(false); return; }
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
  }, [client, query, task, engine, npuOnly, tick]);
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
  const gpu = info.data?.gpus.reduce((best, item) => Math.max(best, gpuMemory(item) ?? 0), 0) || null;
  const ram = info.data?.memory?.total_bytes ?? null;
  const machine = useMemo<Machine>(() => ({ gpu, ram }), [gpu, ram]);
  const npuModels = useMemo(() => engine === null || engine === npu ? flmResults(flm.data, query, task) : [], [flm.data, query, task, engine]);
  const found = useMemo(() => [...(npuOnly ? [] : result?.models ?? []), ...npuModels], [npuOnly, result, npuModels]);
  const views = useMemo(() => filterResults(found, { fit: fitFilter, size, sort }, machine), [found, fitFilter, size, sort, machine]);
  const filtering = task !== null || engine !== null || fitFilter !== null || size !== null;
  const clear = () => { setTask(null); setEngine(null); setFitFilter(null); setSize(null); };
  const model = local.file ? undefined : found.find((item) => item.repo === selected?.repo);
  const quant = model?.quants.find((item) => quantKey(item) === selected?.quant);
  const onNpu = model?.source === "flm";
  const job = onNpu && quant && flm.data ? flmJob(flm.data, quant.file) : undefined;
  const pulls = useResource(client, "juiced/library/pulls", parsePulls, 2000, true, !!quant && !onNpu);
  const pull: PullView | undefined = quant && model ? pulls.data?.find((item) => item.then?.kind === "add" && item.repo.toLowerCase() === model.repo.toLowerCase() && (item.label ?? "Unknown") === quant.label && (!item.files.length || item.files.some((part) => part.file === quant.file))) : undefined;
  const pendingAdd = pull?.then?.state === "pending" ? pull.then : null;
  const failedAdd = pull?.state === "complete" && pull.then?.state === "failed" ? pull : null;
  // A finished download leaves the list once its model exists, so the model list is read again then.
  const following = useRef(false);
  const waiting = !!pendingAdd;
  useEffect(() => {
    if (waiting) following.current = true;
    else if (following.current) { following.current = false; void onRegistered(); }
  }, [waiting, onRegistered]);
  async function retryInstall(id: string) {
    setError("");
    try { await client.request(`juiced/library/pulls/${encodeURIComponent(id)}`, { action: "retry" }); await pulls.reload(); }
    catch (failure) { setError(plainError(failure, "The retry didn't start.")); }
  }
  async function pullNpu(item: Quant, policy: Policy) {
    setError("");
    try {
      await client.request("juiced/flm/pull", { checkpoint: item.file, then: { policy } });
      notify(`Downloading ${item.file}. It's installed as ${policyNames[policy]} when done.`);
      await flm.reload();
    } catch (failure) { setError(plainError(failure, "The download didn't start.")); }
  }
  async function download(target: LibraryModel, item: Quant, policy: Policy, then: { id: string; engine?: string }) {
    if (!result) return;
    setError(""); setNotice("");
    try {
      const pull = (generation: number) => client.request("juiced/library/pull", { repo: target.repo, file: item.file, expected_generation: generation, then: { policy, ...(then.id ? { id: then.id } : {}), ...(then.engine ? { engine: then.engine } : {}) } });
      // Another download finishing a part moves the generation; starting this one doesn't depend on that, and a repeated pull joins the first.
      await pull(result.generation).catch(async (failure) => {
        if (!(failure instanceof ApiError) || failure.code !== "artifact_revision_conflict") throw failure;
        return pull(parseArtifactInventory(await client.request("juiced/artifacts?section=jobs&limit=1")).generation);
      });
      notify(`Downloading ${target.name} ${item.label}. It's installed as ${policyNames[policy]} when done.`);
      setTick((value) => value + 1);
      await pulls.reload();
    } catch (failure) { setError(plainError(failure, "The download didn't start.")); }
  }
  const names = useMemo(() => quantNames(model?.quants ?? []), [model]);
  const file = local.file;
  const checkpoint = quant?.kind === "checkpoint";
  const split = !checkpoint && (quant?.parts ?? 1) > 1;
  const target: AddTarget | null = file ? localTarget(file) : null;
  const targetId = file ? file.id.trim() : checkpoint ? model?.name ?? "" : quant?.kind === "bundle" && model?.nativeId ? model.nativeId : quant ? modelId(quant.file) : "";
  const gatedNote = model?.gated && !quant?.present ? result?.authenticated ? "Gated. Accept its license on Hugging Face first." : "Gated. Set HF_TOKEN on the server and accept its license on Hugging Face." : "";
  const users = model && quant ? quantUsers(inventory, model.repo, quant) : [];
  const partial = !!quant && !quant.present && quant.parts > 1 && quant.presentFiles > 0;
  const ready = file ? !!target && !file.error : !!quant?.present;
  const resetKey = file ? `file:${file.name}` : `${selected?.repo}:${selected?.quant}`;
  const thenPolicy = pendingAdd?.policy ?? (job?.state === "pulling" ? job.then?.policy : undefined);
  const settling = (!!pendingAdd && !!quant && progress(quant) === null) || (job?.state === "complete" && job.then?.state === "pending");
  const npuDone = job?.then?.state === "done" && job.then.model && users.includes(job.then.model) ? job.then.model : null;
  const onDisk = model && quant && !onNpu ? diskQuant(inventory, model.repo, quant) : undefined;
  const fileUsers = file?.artifact ? inventory?.groups.flatMap((group) => group.files).find((item) => item.artifact === file.artifact)?.usedBy ?? [] : [];
  const status = [result || npuOnly ? `${views.length} model${views.length === 1 ? "" : "s"}` : null, npuOnly || !result ? null : hubPending ? "Searching Hugging Face…" : result.hub !== "ok" && result.hub !== "skipped" ? result.offline ? "Offline mode. Only recommended and downloaded models are listed." : "Hugging Face is unreachable. Only recommended and downloaded models are listed." : null].filter(Boolean).join(" · ");
  return <div className={`find-models ${model || file ? "has-detail" : ""}`}>
    <div className="find-main">
      <div className="toolbar find-filters">
        <input type="search" aria-label="Search models" placeholder="Search models, e.g. qwen3, llama, whisper" value={query} onChange={(event) => setQuery(event.target.value)} />
        <select aria-label="Type" data-active={task !== null} value={task ?? ""} onChange={(event) => setTask(event.target.value || null)}><option value="">Any type</option>{tasks.map(([id, label]) => <option key={id} value={id}>{label}</option>)}</select>
        <select aria-label="Engine" data-active={engine !== null} value={engine ?? ""} onChange={(event) => { setEngine(event.target.value || null); setSelected(null); }}><option value="">Any engine</option>{searchEngines.map(([id, label]) => <option key={id} value={id}>{label}</option>)}{flm.data?.available && <option value={npu}>FastFlowLM (NPU)</option>}</select>
        <select aria-label="Hardware" data-active={fitFilter !== null} value={fitFilter ?? ""} onChange={(event) => setFitFilter((event.target.value || null) as FitFilter | null)}><option value="">Any hardware</option>{gpu !== null && <option value="gpu">Fits GPU</option>}<option value="memory">Fits memory</option>{flm.data?.available && <option value="npu">NPU</option>}</select>
        <select aria-label="Size" data-active={size !== null} value={size ?? ""} onChange={(event) => setSize((event.target.value || null) as SizeBucket | null)}><option value="">Any size</option>{sizeBuckets.map(([id, label]) => <option key={id} value={id}>{label}</option>)}</select>
        <select aria-label="Sort" data-active={sort !== "match"} value={sort} onChange={(event) => setSort(event.target.value as Sort)}>{sorts.map(([id, label]) => <option key={id} value={id}>{label}</option>)}</select>
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
      {flm.data?.available && flm.data.listError && (engine === null || engine === npu) && <p className="inline-error" role="alert">FastFlowLM couldn't list its models.</p>}
      {npuOnly && flm.data?.problems.map((problem) => <p className="inline-warning" key={problem}>{problem}</p>)}
      <div className="result-line"><p className="searching muted" role="status">{status}</p>{filtering && <button className="link-button" onClick={clear}>Clear filters</button>}</div>
      {!npuOnly && !result ? <p className="muted" role="status">{loading ? "Searching…" : ""}</p> : views.length === 0 ? <Empty title="No models found">{filtering ? "No model matches every filter." : "Try a shorter search."}</Empty> : <ul className={`model-results ${loading ? "stale" : ""}`} aria-label="Search results" aria-busy={loading}>
        {views.map(({ model: item, quants }) => { const shown = featured(quants); return <li key={`${item.source}:${item.repo}`} className={`model-result ${item.repo === selected?.repo ? "selected" : ""}`}>
          <div className="result-head">
            <strong className="break-word">{item.name}</strong>
            {item.source === "catalog" && <Badge tone="good">Recommended</Badge>}
            {item.quants.some((entry) => entry.present) && <Badge tone="good">✓ On this machine</Badge>}
            <span className="muted result-meta">{[item.publisher, item.task && (taskNames[item.task] ?? item.task), compact(item.downloads) && `${compact(item.downloads)} downloads`, item.source === "flm" ? "NPU" : null].filter(Boolean).join(" · ")}</span>
          </div>
          <div className="quant-chips" role="group" aria-label={`${item.name} quants`}>
            {item.quants.length === 0 ? <span className="muted">No downloadable files found</span> : shown.map((entry) => {
              const percent = progress(entry);
              return <button key={quantKey(entry)} className={`quant-chip ${entry.present ? "present" : ""}`} aria-pressed={item.repo === selected?.repo && quantKey(entry) === selected?.quant} onClick={() => pick(item, entry)} title={entry.kind === "checkpoint" ? `${entry.files.length} files for ${engineNames[entry.engine ?? ""] ?? entry.engine}` : entry.file}>
                {entry.present && <span aria-label="Downloaded">✓ </span>}{entry.label}{entry.kind === "checkpoint" && entry.engine && <span className="quant-engine"> · {engineNames[entry.engine] ?? entry.engine}</span>}<span className="muted"> {bytes(entry.bytes)}</span>{entry.recommended && <span className="quant-star" aria-label="Suggested">★</span>}
                {percent !== null && <span className="quant-progress" style={{ width: `${percent}%` }} />}
              </button>;
            })}
            {item.quants.length > shown.length && <button className="quant-chip more" onClick={() => pick(item, quants.find((entry) => entry.recommended) ?? shown[0] ?? item.quants[0])}>+{item.quants.length - shown.length} more</button>}
          </div>
        </li>; })}
      </ul>}
    </div>
    {file && <aside className="inspector find-detail" aria-label="Model detail">
      <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={local.clear}><Icon name="back" /> Back to results</Button><Button variant="ghost" className="close-inspector" aria-label="Close detail" onClick={local.clear}><Icon name="close" /></Button></div>
      <div className="inspector-body">
        <div className="inspector-title"><h2 className="break-word">{file.name}</h2></div>
        <dl className="facts">
          <div><dt>Size</dt><dd>{bytes(file.bytes)}{file.partCount > 1 ? ` · ${file.partCount} parts` : ""}</dd></div>
          <div><dt>Stored</dt><dd>{file.path ? "Used where it is" : file.job ? "Copied to library" : "Uploaded to library"}</dd></div>
          {!file.projectors?.length && file.projector && <div><dt>Vision projector</dt><dd className="break-word">{file.projector.name}</dd></div>}
          <div><dt>This machine</dt><dd><Badge tone={fit(file.bytes, machine)[1]}>{fit(file.bytes, machine)[0]}</Badge></dd></div>
        </dl>
        <LocalProgress file={file} onCancel={local.clear} />
        {ready && <div className="add-name"><label htmlFor="local-model-id">Model name</label><input id="local-model-id" value={file.id} maxLength={128} onChange={(event) => local.rename(event.target.value)} /></div>}
        {ready && <ProjectorChoice file={file} onChoose={local.chooseProjector} />}
        {fileUsers.length > 0 && !installedHere.current.has(resetKey) ? <div className="added"><p className="inline-success" role="status">Installed as {fileUsers[0]}.</p><div className="action-row"><Button variant="primary" onClick={() => onOpenModel(fileUsers[0]!)}>Open model</Button></div></div>
          : <AddModel client={client} online={online} target={target} id={targetId} ready={ready} resetKey={resetKey} onRegistered={async () => { installedHere.current.add(resetKey); await onRegistered(); }} onOpenModel={onOpenModel} />}
      </div>
    </aside>}
    {model && quant && <aside className="inspector find-detail" aria-label="Model detail">
      <div className="inspector-top"><Button variant="ghost" className="back-button" onClick={() => choose(null)}><Icon name="back" /> Back to results</Button><Button variant="ghost" className="close-inspector" aria-label="Close detail" onClick={() => choose(null)}><Icon name="close" /></Button></div>
      <div className="inspector-body">
        <div className="inspector-title"><h2 className="break-word">{model.name}</h2></div>
        <p className="field-help">{[model.publisher, model.license, model.gated ? "Gated" : null].filter(Boolean).join(" · ")}</p>
        <QuantList listRef={quantList} label={onNpu ? "Sizes" : "Quants"} selected={quantKey(quant)} onSelect={(key) => pick(model, model.quants.find((entry) => quantKey(entry) === key))} items={[...model.quants].sort((left, right) => (left.bytes ?? 0) - (right.bytes ?? 0)).map((entry): QuantItem => {
          const [label, tone] = fit(entry.bytes, machine, entry.engine);
          return { key: quantKey(entry), label: names.get(entry) ?? entry.label, bytes: entry.bytes, title: entry.file, suggested: entry.recommended, fit: { tone, label }, progress: progress(entry), state: hubQuantState(entry, quantUsers(inventory, model.repo, entry), registered, diskQuant(inventory, model.repo, entry)) };
        })} />
        <dl className="facts">
          <div><dt>This machine</dt><dd><Badge tone={fit(quant.bytes, machine, quant.engine)[1]}>{fit(quant.bytes, machine, quant.engine)[0]}</Badge></dd></div>
          {quant.engine && <div><dt>Runs on</dt><dd>{engineNames[quant.engine] ?? quant.engine}</dd></div>}
          {quant.traits.length > 0 && <div><dt>Features</dt><dd>{quant.traits.join(", ")}</dd></div>}
          {(checkpoint || quant.parts > 1) && <div><dt>Files</dt><dd>{checkpoint ? quant.files.length : partial ? `${quantWords.parts(quant.presentFiles, quant.parts)} on disk` : `${quant.parts} parts`}</dd></div>}
          {quant.present && <div><dt>Stored</dt><dd>{quant.localPath ? `Hugging Face cache${quant.readOnly ? " · read-only" : ""}` : "Juiced storage"}</dd></div>}
          {users.length > 0 && <div><dt>Used by</dt><dd><span className="used-by">{users.map((id) => <button key={id} className="link-button" onClick={() => onOpenModel(id)}>{id}</button>)}</span></dd></div>}
        </dl>
        {checkpoint && <div className="checkpoint-files"><span className="field-label">Files <HelpTip label="Checkpoint files">The weights, config and tokenizer files the engine reads as one folder. READMEs, images and other formats are skipped.</HelpTip></span><ul aria-label="Checkpoint files">{quant.files.map((part) => <li key={part.file}><span className="break-word">{part.member ?? part.file}</span><span className="numeric muted">{bytes(part.bytes)}</span></li>)}</ul></div>}
        {onNpu && flm.data?.problems.map((problem) => <p className="inline-warning" key={problem}>{problem}</p>)}
        {progress(quant) !== null && <><progress aria-label="Download progress" max={100} value={progress(quant)!} /><p className="field-help">{quantWords.downloading(progress(quant)!)}{thenPolicy ? ` · then installed as ${policyNames[thenPolicy as Policy] ?? thenPolicy}` : ""}</p></>}
        {settling && <p className="muted" role="status">Installing…</p>}
        {onNpu ? npuDone ? <div className="added"><p className="inline-success" role="status">Installed {npuDone}.</p><div className="action-row"><Button variant="primary" onClick={() => onOpenModel(npuDone)}>Open model</Button></div></div>
          : !settling && progress(quant) === null && <>
            {job?.state === "failed" && !ready && <p className="inline-error" role="alert">The download failed.{job.error ? ` ${job.error}` : ""}</p>}
            {job?.then?.state === "failed" && users.length === 0 && <p className="inline-warning">{job.then.message ?? "The model wasn't installed."}</p>}
            {users.length > 0 && !addedHere.current.has(quant.file) ? <div className="action-row"><Button onClick={() => onOpenModel(users[0]!)}>Open model</Button></div>
              : <AddModel client={client} online={online} target={null} id="" ready={ready} resetKey={resetKey} onOpenModel={onOpenModel}
                register={(policy) => client.request("juiced/flm/models/add", { checkpoint: quant.file, policy })}
                onRegistered={async () => { addedHere.current.add(quant.file); await flm.reload(); await onRegistered(); }}
                download={ready ? undefined : { disabled: false, start: (policy) => pullNpu(quant, policy) }} />}
          </>
          : !pendingAdd && (users.length > 0 ? <div className="action-row wrap on-disk-actions"><Button variant="primary" onClick={() => onOpenModel(users[0]!)}>Open model</Button>{users.length === 1 && <RemoveModel client={client} model={users[0]!} online={online} label="Remove" onRemoved={onRegistered} />}</div>
            : quant.present ? failedAdd ? <div className="action-row wrap"><p className="inline-warning">{thenFailure(failedAdd.then!) ?? "The model wasn't installed."}</p><Button variant="primary" disabled={!online} onClick={() => void retryInstall(failedAdd.id)}>Retry</Button></div>
              : onDisk?.install && onDisk.install.state !== "pending" ? <p className="inline-warning">On disk, but not a model yet. {onDisk.install.message}</p>
              : <p className="muted" role="status">Installing…</p>
            : <AddModel client={client} online={online} target={null} id={targetId} ready={false} resetKey={resetKey} downloadLabel={partial ? "Finish download" : undefined} onRegistered={onRegistered} onOpenModel={onOpenModel} download={progress(quant) === null ? { disabled: !quant.pullable || (model.gated && !result?.authenticated), start: (policy) => download(model, quant, policy, { id: targetId, engine: quant.engine === "thenoise" ? "thenoise" : undefined }) } : undefined} />)}
        {gatedNote && <p className="field-help">{gatedNote}</p>}
      </div>
    </aside>}
  </div>;
}
