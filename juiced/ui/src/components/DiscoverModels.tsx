import { useEffect, useRef, useState } from "react";
import { Download, ExternalLink, LoaderCircle, Search, X } from "lucide-react";
import { api, bytes, type Model } from "../api";
import { Button } from "./t3/button";
import { HelpTip } from "./HelpTip";
import { CapabilityIcons, serviceNames } from "./ModelLibrary";
import { runtimeName, useSystemInfo } from "./ExecutionTarget";
import { catalogEntries, groupRepositories, matchesModelSearch, modelQuant, quantization, type Repository } from "./modelDiscovery";
import "./DiscoverModels.css";

const capabilities = [["vision", "Vision"], ["tool-calling", "Tools"], ["reasoning", "Reasoning"], ["coding", "Coding"], ["mtp", "MTP"], ["realtime-transcription", "Streaming audio"], ["edit", "Image editing"]];
export function DiscoverModels({ models, jobs, busy, onRepository, onDownload, onInspect, onDownloads }: {
  models: Model[]; jobs: any[]; busy: boolean;
  onRepository: (repository: string) => void;
  onDownload: (model: string) => void;
  onInspect: (model: Model) => void;
  onDownloads: () => void;
}) {
  const [source, setSource] = useState("catalog");
  const [query, setQuery] = useState("");
  const [task, setTask] = useState("");
  const [capability, setCapability] = useState("");
  const [runtime, setRuntime] = useState("");
  const [size, setSize] = useState("");
  const [downloaded, setDownloaded] = useState("");
  const [recommended, setRecommended] = useState(false);
  const [sort, setSort] = useState("recommended");
  const [limit, setLimit] = useState(30);
  const [remoteQuery, setRemoteQuery] = useState("");
  const [publisher, setPublisher] = useState("");
  const [remoteSort, setRemoteSort] = useState("relevance");
  const [results, setResults] = useState<Repository[]>([]);
  const [searched, setSearched] = useState("");
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState("");
  const controller = useRef<AbortController | null>(null);
  const catalogInput = useRef<HTMLInputElement>(null);
  const { info } = useSystemInfo();
  useEffect(() => () => controller.current?.abort(), []);
  useEffect(() => setLimit(30), [query, task, capability, runtime, size, downloaded, recommended, sort]);
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
    return (sort === "recommended" ? Number(picks(b)) - Number(picks(a)) : 0) || a.id.localeCompare(b.id);
  });
  const groups = groupRepositories(results).filter((group) => !publisher || group.publisher === publisher);
  if (remoteSort === "downloads") groups.sort((a, b) => b.repositories.reduce((n, r) => n + (r.downloads || 0), 0) - a.repositories.reduce((n, r) => n + (r.downloads || 0), 0));
  const filtered = !!(query || task || capability || runtime || size || downloaded || recommended || sort !== "recommended");
  function reset() { setQuery(""); setTask(""); setCapability(""); setRuntime(""); setSize(""); setDownloaded(""); setRecommended(false); setSort("recommended"); }
  async function search() {
    const value = remoteQuery.trim();
    if (value.length < 3) { setError("Enter at least three characters or a Hugging Face model URL."); return; }
    if (/^https:\/\/huggingface\.co\//i.test(value)) {
      const parts = new URL(value).pathname.split("/").filter(Boolean);
      if (parts.length >= 2) { onRepository(parts.slice(0, 2).join("/")); return; }
    }
    controller.current?.abort();
    const request = new AbortController(); controller.current = request;
    setSearching(true); setError("");
    try {
      const response = await api(`registry/search?source=huggingface&query=${encodeURIComponent(value)}&format=gguf&limit=50`, undefined, request.signal);
      if (request.signal.aborted) return;
      setResults(response.results || []); setSearched(value); setPublisher("");
    } catch (e) { if (!request.signal.aborted) setError((e as Error).message); }
    finally { if (!request.signal.aborted) setSearching(false); }
  }
  return <section className="model-discovery" aria-label="Discover model catalog">
    <nav className="workspace-tabs" aria-label="Discovery sources">
      <button className={source === "catalog" ? "active" : ""} aria-current={source === "catalog" ? "page" : undefined} onClick={() => setSource("catalog")}>Model catalog <span>{catalog.length}</span></button>
      <button className={source === "huggingface" ? "active" : ""} aria-current={source === "huggingface" ? "page" : undefined} onClick={() => setSource("huggingface")}><img src="/huggingface.svg" width="17" height="17" alt="" /> Hugging Face</button>
    </nav>
    {source === "catalog" ? <>
      <div className="discover-toolbar">
        <label className="search-input"><Search size={16} /><input ref={catalogInput} aria-label="Search model catalog" placeholder="Search models, publishers, capabilities…" value={query} onChange={(e) => setQuery(e.target.value)} />{query && <Button variant="ghost" size="icon-xs" aria-label="Clear catalog search" onClick={() => { setQuery(""); catalogInput.current?.focus(); }}><X size={14} /></Button>}</label>
        <select aria-label="Sort model catalog" value={sort} onChange={(e) => setSort(e.target.value)}><option value="recommended">Recommended first</option><option value="name">Name</option><option value="smallest">Smallest first</option><option value="largest">Largest first</option></select>
      </div>
      <div className="discover-filters" aria-label="Catalog filters">
        <select aria-label="Filter by service" value={task} onChange={(e) => setTask(e.target.value)}><option value="">All services</option>{[...new Set(catalog.map((m) => m.service))].sort().map((s) => <option key={s} value={s}>{serviceNames[s] || s}</option>)}</select>
        <select aria-label="Filter by capability" value={capability} onChange={(e) => setCapability(e.target.value)}><option value="">All capabilities</option>{capabilities.filter(([key]) => catalog.some((m) => m.labels.includes(key))).map(([key, label]) => <option key={key} value={key}>{label}</option>)}</select>
        <select aria-label="Filter by runtime" value={runtime} onChange={(e) => setRuntime(e.target.value)}><option value="">All runtimes</option>{[...new Set(catalog.map((m) => m.recipe))].sort().map((r) => <option key={r} value={r}>{runtimeName(r, info?.recipes[r])}</option>)}</select>
        <select aria-label="Filter by download size" value={size} onChange={(e) => setSize(e.target.value)}><option value="">Any size</option>{[2, 4, 8, 16, 32, 64].map((n) => <option key={n} value={n}>Up to {n} GiB</option>)}</select>
        <select aria-label="Filter by download status" value={downloaded} onChange={(e) => setDownloaded(e.target.value)}><option value="">Any status</option><option value="no">Not downloaded</option><option value="yes">Downloaded</option></select>
      </div>
      <div className="discover-result-bar">
        <span role="status">{visible.length} models</span>
        <Button size="sm" variant={recommended ? "secondary" : "ghost"} aria-pressed={recommended} onClick={() => setRecommended(!recommended)}>Recommended</Button>
        <HelpTip label="Catalog recommendations">The bundled Lemonade catalog's hot label supplies recommendations. This is separate from your Hot loading policy. Catalog entries and download sizes do not establish exact hardware compatibility or RAM requirements.</HelpTip>
        {filtered && <Button size="sm" variant="ghost" onClick={reset}>Reset filters</Button>}
      </div>
      <div className="discover-table" aria-label="Catalog models">
        <div className="discover-columns"><span>Model</span><span>Service / runtime</span><span title="Estimated weight download size; not runtime memory">Download size</span><span /></div>
        {visible.slice(0, limit).map((model) => {
          const job = jobs.find((j) => j.running && (j.model_name === model.id || j.model_name === "user." + model.id));
          return <article className="discover-row" key={model.id}>
            <div className="discover-identity"><button className="discover-model-name" title="View model details" onClick={() => onInspect(model)}>{model.id}</button><div className="discover-model-meta">{picks(model) && <span className="recommendation-label" title="Recommended in the Lemonade catalog">Recommended</span>}{modelQuant(model) && <span className="quant-badge">{modelQuant(model)}</span>}<CapabilityIcons model={model} />{model.downloaded && <span>Downloaded</span>}</div></div>
            <div className="discover-runtime"><span>{serviceNames[model.service] || model.service}</span><small>{runtimeName(model.recipe, info?.recipes[model.recipe])}</small></div>
            <span className="discover-size">{model.size ? bytes(model.size * 1e9) : "—"}</span>
            <Button size="compact" variant="outline" disabled={busy} onClick={() => job ? onDownloads() : model.downloaded ? onInspect(model) : onDownload(model.id)}>{job ? <><LoaderCircle size={13} className="spin" />Downloading</> : model.downloaded ? "Open" : <><Download size={13} />Download</>}</Button>
          </article>;
        })}
      </div>
      {!visible.length && <div className="discover-empty"><h2>{catalog.length ? "No models match these filters" : "Catalog not available yet"}</h2>{(!task || task === "Chat") && <Button variant="ghost" size="sm" onClick={() => { setRemoteQuery(query); setSource("huggingface"); }}>Search Hugging Face</Button>}</div>}
      {visible.length > limit && <Button className="discover-more" variant="outline" size="sm" onClick={() => setLimit(limit + 30)}>Show more models ({visible.length - limit} remaining)</Button>}
    </> : <>
      <form className="discover-toolbar" aria-label="Search Hugging Face" onSubmit={(e) => { e.preventDefault(); void search(); }}>
        <label className="search-input"><Search size={16} /><input aria-label="Search Hugging Face models" placeholder="Model name or Hugging Face URL…" value={remoteQuery} onChange={(e) => setRemoteQuery(e.target.value)} /></label>
        <Button type="submit" disabled={busy} aria-busy={searching}>{searching && <LoaderCircle size={16} className="spin" />}Search</Button>
      </form>
      <div className="discover-result-bar"><span>GGUF chat models</span><HelpTip label="Hugging Face search">Searches up to 50 matching repositories. Publisher filters and sorting apply to returned results. Review files and the runtime before downloading. Other services are available in Model catalog.</HelpTip>{!!results.length && <><select aria-label="Filter search by publisher" value={publisher} onChange={(e) => setPublisher(e.target.value)}><option value="">All publishers</option>{[...new Set(results.map((r) => r.repository_id.split("/")[0]))].sort().map((p) => <option key={p}>{p}</option>)}</select><select aria-label="Sort Hugging Face results" value={remoteSort} onChange={(e) => setRemoteSort(e.target.value)}><option value="relevance">Relevance</option><option value="downloads">Downloads</option></select></>}</div>
      {error && <div className="inline-error" role="alert">{error}<Button variant="outline" size="sm" onClick={() => void search()}>Retry search</Button></div>}
      {searching && <p role="status" className="discover-search-state">Searching Hugging Face…</p>}
      {!!searched && <p className="discover-search-state">{groups.length} models for “{searched}”{searching ? " · previous results" : ""}</p>}
      {!searching && !groups.length && <div className="discover-empty"><h2>{searched ? "No matching repositories" : "Search beyond the catalog"}</h2><p>{searched ? "Try a shorter model name or another publisher." : "Find chat GGUFs or paste a Hugging Face model URL to browse its quants."}</p></div>}
      <div className="discovery-results" aria-label="Model search results">{groups.map((group) => <article className="discovery-model" key={group.key}>
        <header><div><h3>{group.name}</h3><p>{group.publisher}</p></div><span className="discovery-downloads"><Download size={13} />{group.repositories.reduce((n, r) => n + (r.downloads || 0), 0).toLocaleString()}</span></header>
        <div className="discovery-quants" aria-label={`Quantizations for ${group.name}`}>{group.repositories.map((repo) => <Button key={repo.repository_id} variant="outline" size="compact" disabled={busy} title={repo.repository_id} onClick={() => onRepository(repo.repository_id)}>{quantization(repo.repository_id.split("/").pop() || "") || "Browse quants"}</Button>)}{group.repositories.map((repo) => <a className="repository-source" key={repo.repository_id} href={`https://huggingface.co/${repo.repository_id}`} target="_blank" rel="noreferrer" aria-label={`Open ${repo.repository_id} on Hugging Face`} title={repo.repository_id}><ExternalLink size={14} /></a>)}</div>
      </article>)}</div>
    </>}
  </section>;
}
