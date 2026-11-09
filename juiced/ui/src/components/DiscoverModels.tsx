import { useEffect, useRef, useState } from "react";
import { Download, ExternalLink, LoaderCircle, Search, X } from "lucide-react";
import { api, bytes, type Model } from "../api";
import { Button } from "./t3/button";
import { HelpTip } from "./HelpTip";
import { CapabilityIcons, serviceNames } from "./ModelLibrary";
import { runtimeName, useSystemInfo } from "./runtimeInfo";
import { catalogEntries, groupRepositories, matchesModelSearch, modelQuant, quantization, type Repository } from "./modelDiscovery";
import { discoveryContextLabel } from "./effectiveSettings";
import "./DiscoverModels.css";

const capabilities = [["vision", "Vision"], ["tool-calling", "Tools"], ["reasoning", "Reasoning"], ["coding", "Coding"], ["mtp", "MTP"], ["realtime-transcription", "Streaming audio"], ["edit", "Image editing"]];
export function DiscoverModels({ initialSearch, models, jobs, busy, onRepository, onDownload, onInspect, onDownloads, onTry }: {
  initialSearch?: { query?: string; runtime?: string; repo?: string };
  models: Model[]; jobs: any[]; busy: boolean;
  onRepository: (repository: string) => void;
  onDownload: (model: string) => void;
  onInspect: (model: Model) => void;
  onDownloads: () => void;
  onTry: (model: Model) => void;
}) {
  const [query, setQuery] = useState(initialSearch?.query || initialSearch?.repo || "");
  const [task, setTask] = useState("");
  const [capability, setCapability] = useState("");
  const [runtime, setRuntime] = useState(initialSearch?.runtime || "");
  const [size, setSize] = useState("");
  const [downloaded, setDownloaded] = useState("");
  const [recommended, setRecommended] = useState(false);
  const [sort, setSort] = useState("available");
  const [limit, setLimit] = useState(30);
  const [filtersOpen, setFiltersOpen] = useState(!!initialSearch?.runtime);
  const [searchedRuntime, setSearchedRuntime] = useState("");
  const [publisher, setPublisher] = useState("");
  const [results, setResults] = useState<Repository[]>([]);
  const [searched, setSearched] = useState("");
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState("");
  const controller = useRef<AbortController | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const { info } = useSystemInfo();
  useEffect(() => () => controller.current?.abort(), []);
  useEffect(() => setLimit(30), [query, task, capability, runtime, size, downloaded, recommended, sort]);
  useEffect(() => {
    const value = initialSearch?.query || initialSearch?.repo;
    if (!value) return;
    changeQuery(value); reset(); setFiltersOpen(!!initialSearch?.runtime);
    setRuntime(initialSearch?.runtime || "");
    if (initialSearch?.query) void search(value, initialSearch.runtime || "");
  }, [initialSearch]);
  const catalog = catalogEntries(models);
  const picks = (m: Model) => m.labels.includes("hot");
  const visible = catalog.filter((m) =>
    matchesModelSearch(query, [m.id, m.recipe, runtimeName(m.recipe, info?.recipes[m.recipe]), m.service, ...m.labels, ...Object.values(m.checkpoints || {}), m.checkpoint || ""]) &&
    (!task || m.service === task) && (!capability || m.labels.includes(capability)) &&
    (!runtime || m.recipe === runtime) && (!recommended || picks(m)) &&
    (!size || !!m.size && m.size * 1e9 <= Number(size) * 1024 ** 3) &&
    (!downloaded || (downloaded === "yes" ? m.downloaded : !m.downloaded)),
  ).sort((a, b) => {
    if (sort === "smallest" || sort === "largest") {
      if (!a.size || !b.size) return Number(!a.size) - Number(!b.size) || a.id.localeCompare(b.id);
      return (sort === "smallest" ? 1 : -1) * (a.size - b.size) || a.id.localeCompare(b.id);
    }
    return (sort === "available" ? Number(b.loaded) - Number(a.loaded) || Number(b.downloaded) - Number(a.downloaded) : 0) || a.id.localeCompare(b.id);
  });
  const groups = groupRepositories(results).filter((group) => !publisher || group.publisher === publisher);
  const filtered = !!(task || capability || runtime || size || downloaded || recommended || sort !== "available");
  function reset() { setTask(""); setCapability(""); setRuntime(""); setSize(""); setDownloaded(""); setRecommended(false); setSort("available"); }
  function changeQuery(value: string) {
    controller.current?.abort();
    setQuery(value); setSearching(false); setError(""); setSearched(""); setResults([]); setPublisher("");
  }
  async function search(value = query, targetRuntime = runtime) {
    const submitted = value.trim();
    let searchQuery = submitted;
    if (submitted.length < 3) { setError("Enter at least three characters or paste a Hugging Face repository URL."); return; }
    controller.current?.abort();
    if (/^https:\/\/huggingface\.co\//i.test(submitted)) {
      const parts = new URL(submitted).pathname.split("/").filter(Boolean);
      if (parts.length >= 2) {
        searchQuery = parts.slice(0, 2).join("/");
        if (!targetRuntime || targetRuntime === "llamacpp") { setError(""); onRepository(searchQuery); return; }
      }
    }
    if (/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(submitted) && (!targetRuntime || targetRuntime === "llamacpp")) { setError(""); onRepository(submitted); return; }
    const request = new AbortController(); controller.current = request;
    setSearching(true); setError(""); setDownloaded("");
    try {
      const response = await api(`registry/search?source=huggingface&query=${encodeURIComponent(searchQuery)}&format=${!targetRuntime || targetRuntime === "llamacpp" ? "gguf" : "all"}&limit=50`, undefined, request.signal);
      if (request.signal.aborted) return;
      setResults(response.results || []); setSearched(submitted); setSearchedRuntime(targetRuntime); setPublisher("");
    } catch (e) { if (!request.signal.aborted) setError((e as Error).message); }
    finally { if (!request.signal.aborted) setSearching(false); }
  }
  return <section className="model-discovery" aria-label="Find and run models">
    <form className="discover-toolbar" aria-label="Library search" onSubmit={(e) => { e.preventDefault(); void search(); }}>
      <label className="search-input"><Search size={16} /><input ref={input} aria-label="Search models" placeholder="Model name, publisher, quant or Hugging Face URL…" value={query} onChange={(e) => changeQuery(e.target.value)} />{query && <Button type="button" variant="ghost" size="icon-xs" aria-label="Clear model search" onClick={() => { changeQuery(""); input.current?.focus(); }}><X size={14} /></Button>}</label>
      <Button type="submit" disabled={busy} aria-busy={searching}>{searching && <LoaderCircle size={16} className="spin" />}Search published models</Button>
    </form>
    <p className="page-note">Search your models and the catalog as you type. Search published models to find other repositories and quants.</p>
    <details className="library-filters" open={filtersOpen} onToggle={(event) => setFiltersOpen(event.currentTarget.open)}>
      <summary>Filter known models{filtered ? " · filters applied" : ""}</summary>
      <div className="discover-filters" aria-label="Known model filters">
        <select aria-label="Filter by service" value={task} onChange={(e) => setTask(e.target.value)}><option value="">All services</option>{[...new Set(catalog.map((m) => m.service))].sort().map((s) => <option key={s} value={s}>{serviceNames[s] || s}</option>)}</select>
        <select aria-label="Filter by capability" value={capability} onChange={(e) => setCapability(e.target.value)}><option value="">All capabilities</option>{capabilities.filter(([key]) => catalog.some((m) => m.labels.includes(key))).map(([key, label]) => <option key={key} value={key}>{label}</option>)}</select>
        <select aria-label="Filter by runtime" value={runtime} onChange={(e) => setRuntime(e.target.value)}><option value="">All runtimes</option>{[...new Set([...catalog.map((m) => m.recipe), "llamacpp", "vllm", "flm"])].sort().map((r) => <option key={r} value={r}>{runtimeName(r, info?.recipes[r])}</option>)}</select>
        <select aria-label="Filter by download size" value={size} onChange={(e) => setSize(e.target.value)}><option value="">Any weight size</option>{[2, 4, 8, 16, 32, 64].map((n) => <option key={n} value={n}>Up to {n} GiB</option>)}</select>
        <select aria-label="Filter by availability" value={downloaded} onChange={(e) => setDownloaded(e.target.value)}><option value="">Any availability</option><option value="yes">On this host</option><option value="no">Not downloaded</option></select>
      </div>
      <div className="discover-result-bar">
        <Button size="sm" variant={recommended ? "secondary" : "ghost"} aria-pressed={recommended} onClick={() => setRecommended(!recommended)}>Catalog picks</Button>
        <HelpTip label="Catalog picks">These are publisher catalog picks, not measured recommendations for your host. Weight size excludes runtime, context and workspace memory. Hardware fit remains unverified until the selected profile is checked.</HelpTip>
        {filtered && <Button size="sm" variant="ghost" onClick={reset}>Reset filters</Button>}
      </div>
    </details>
    {error && <div className="inline-error" role="alert">{error}<Button variant="outline" size="sm" onClick={() => void search()}>Retry search</Button></div>}
    {searching && <p role="status" className="discover-search-state">Searching published repositories… Your model list stays available below.</p>}
    {!!searched && <section className="published-models" aria-label="Published model results">
      <div className="discover-result-bar"><h2>Published variants</h2><span>{groups.length} model groups for “{searched}”</span>{!!results.length && <select aria-label="Filter search by publisher" value={publisher} onChange={(e) => setPublisher(e.target.value)}><option value="">All publishers</option>{[...new Set(results.map((r) => r.repository_id.split("/")[0]))].sort().map((p) => <option key={p}>{p}</option>)}</select>}</div>
      <p className="page-note">Publisher metadata groups these repositories. Review exact files and companions before downloading. Download popularity does not establish quality or host compatibility.</p>
      {!groups.length && <div className="discover-empty"><h3>No matching repositories</h3><p>Try a shorter model name or another publisher. Your query and local models are still here.</p></div>}
      <div className="discovery-results" aria-label="Model search results">{groups.map((group) => <article className="discovery-model" key={group.key}>
        <header><div><h3>{group.name}</h3><p>{group.publisher}</p></div></header>
        <div className="published-repositories">{group.repositories.map((repo) => {
          const gguf = !searchedRuntime || searchedRuntime === "llamacpp";
          const local = models.filter((model) => Object.values(model.checkpoints || { main: model.checkpoint || "" }).some((checkpoint) => checkpoint.startsWith(repo.repository_id + ":")));
          return <div className="published-repository" key={repo.repository_id}>
            <div><strong>{repo.repository_id}</strong><small>{quantization(repo.repository_id.split("/").pop() || "") || "Several quants may be available"}{local.some((m) => m.downloaded) ? " · Variants on this host" : " · Not checked on this host"}</small></div>
            {gguf ? <Button variant="outline" size="compact" disabled={busy} onClick={() => onRepository(repo.repository_id)}>Choose files</Button> : <a className="repository-source" href={`https://huggingface.co/${repo.repository_id}/tree/main`} target="_blank" rel="noreferrer">Review publisher files <ExternalLink size={13} /></a>}
            <a className="repository-source" href={`https://huggingface.co/${repo.repository_id}`} target="_blank" rel="noreferrer" aria-label={`Open ${repo.repository_id} on Hugging Face`}><ExternalLink size={14} /></a>
            {!gguf && <p className="page-note">Source review only. This repository format cannot be downloaded through the GGUF file picker.</p>}
          </div>;
        })}</div>
      </article>)}</div>
    </section>}
    <div className="discover-result-bar"><h2>Your models & catalog</h2><span role="status">{visible.length} models</span><select aria-label="Sort models" value={sort} onChange={(e) => setSort(e.target.value)}><option value="available">On this host first</option><option value="name">Name</option><option value="smallest">Smallest weights first</option><option value="largest">Largest weights first</option></select></div>
    <div className="discover-table" aria-label="Known models">
      <div className="discover-columns"><span>Model / availability</span><span>Service / runtime</span><span title="Estimated weight size; not runtime memory">Weight size</span><span>Next step</span></div>
      {visible.slice(0, limit).map((model) => {
        const job = jobs.find((j) => j.running && (j.model_name === model.id || j.model_name === "user." + model.id));
        const pending = !!model.operation?.pending;
        const action = job ? "View download" : model.available === false ? "Review requirements" : !model.downloaded ? "Review download" : model.loaded ? "Try model" : "Set up & run";
        return <article className="discover-row" key={model.id}>
          <div className="discover-identity"><button className="discover-model-name" title="View model details" onClick={() => onInspect(model)}>{model.id}</button><div className="discover-model-meta">{modelQuant(model) && <span className="quant-badge">{modelQuant(model)}</span>}<CapabilityIcons model={model} />{model.service === "Chat" && <span>{discoveryContextLabel(model) || "Context not reported"}</span>}<span>{pending ? model.operation!.state : job ? "Downloading" : model.loaded ? "Running" : model.downloaded ? "On this host" : "In catalog"}</span>{model.downloaded && <span>{model.policy === "disabled" ? "Disabled" : model.policy === "hot" ? "Hot" : "On demand"}</span>}</div>{model.operation?.error && <p className="inline-error">{model.operation.error}</p>}</div>
          <div className="discover-runtime"><span>{serviceNames[model.service] || model.service}</span><small>{runtimeName(model.recipe, info?.recipes[model.recipe])}</small></div>
          <span className="discover-size">{model.size ? bytes(model.size * 1e9) : "Unknown"}</span>
          <Button size="compact" variant="outline" disabled={busy || pending} onClick={() => job ? onDownloads() : model.available === false ? onInspect(model) : !model.downloaded ? onDownload(model.id) : model.loaded ? onTry(model) : onInspect(model)}>{(job || pending) && <LoaderCircle size={13} className="spin" />}{pending ? model.operation!.state : action}</Button>
        </article>;
      })}
    </div>
    {!visible.length && <div className="discover-empty"><h3>No known models match</h3><p>Adjust the filters, or use Search published models above. Import a GGUF if the weights are already on your computer.</p></div>}
    {visible.length > limit && <Button className="discover-more" variant="outline" size="sm" onClick={() => setLimit(limit + 30)}>Show more models ({visible.length - limit} remaining)</Button>}
  </section>;
}
