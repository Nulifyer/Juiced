import { useEffect, useMemo, useState } from "react";
import { Download, ExternalLink, RefreshCw, Search } from "lucide-react";
import { backendInstalled, backendLabel, backendReady, backendUpdate } from "./backendState";
import { api, type Model } from "../api";
import { useSystemInfo, runtimeName, acceleratorName } from "./ExecutionTarget";
import { Button } from "./t3/button";
import "./BackendInventory.css";

export function BackendInventory({ jobs = [], models, openDownloads }: { jobs?: any[]; models: Model[]; openDownloads: () => void }) {
  const { info, error, refresh } = useSystemInfo();
  const [filter, setFilter] = useState("all");
  const [query, setQuery] = useState("");
  const [installing, setInstalling] = useState("");
  const [operationError, setOperationError] = useState("");
  const [notice, setNotice] = useState("");
  const signature = jobs.filter((job) => job.type === "backend" || job.id?.startsWith("backend:")).map((job) => `${job.id}:${job.status}`).join("|");
  useEffect(() => { refresh(); }, [signature, refresh]);
  const devices = Object.entries(info?.devices || {}).flatMap(([kind, value]) => (Array.isArray(value) ? value : [value]).filter((device) => device?.available).map((device) => ({ ...device, kind })));
  const rows = useMemo(() => Object.entries(info?.recipes || {}).flatMap(([recipe, runtime]) => Object.entries(runtime.backends).map(([backend, variant]) => ({ recipe, runtime, backend, variant }))), [info]);
  const category = (state: string) => backendInstalled(state) ? "installed" : state === "installable" ? "available" : "unavailable";
  const visible = rows.filter((row) => (filter === "all" || (filter === "updates" ? backendUpdate(row.variant.state) : category(row.variant.state) === filter)) && `${row.recipe} ${row.runtime.display_name} ${row.backend} ${row.runtime.modality}`.toLowerCase().includes(query.toLowerCase()));
  async function install(recipe: string, backend: string, update: boolean) {
    setInstalling(`${recipe}:${backend}`); setOperationError(""); setNotice("");
    try {
      const result = await api("install", { recipe, backend, force: update, stream: true, subscribe: false });
      if (result.action) throw Error("This runtime requires manual setup. Use its setup guide, then refresh the inventory.");
      setNotice(`Installation of ${runtimeName(recipe)} ${acceleratorName(backend)} is managed by the server. Follow its progress in Downloads.`);
      refresh();
    } catch (error) { setOperationError((error as Error).message); }
    finally { setInstalling(""); }
  }
  return <div className="backend-inventory"><header className="page-heading"><div><h1>Backends & hardware</h1><p>Check native runtime readiness before selecting an execution target.</p></div><Button variant="outline" size="sm" onClick={refresh}><RefreshCw size={14} /> Refresh inventory</Button></header>
    {error && <p className="inline-error" role="alert">Inventory may be stale: {error}. <Button size="sm" variant="outline" onClick={refresh}>Retry inventory</Button></p>}
    {operationError && <p className="inline-error" role="alert">{operationError}</p>}
    {notice && <div className="settings-message" role="status">{notice} <Button variant="outline" size="sm" onClick={openDownloads}>Open downloads</Button></div>}
    {!info && !error && <p role="status">Reading runtime inventory…</p>}
    {info && <><section className="hardware-overview" aria-label="Detected hardware">{devices.map((device, index) => <article key={index}><strong>{device.name || device.kind}</strong><small>{device.kind === "cpu" ? `${device.cores} cores · ${device.threads} threads` : device.family || "Detected; runtime compatibility is checked separately"}</small></article>)}</section>
      <p className="page-note">Detected hardware is not proof of a compatible model or engine. Installations do not reload your current workers. Version information is shown only when the runtime reports it.</p>
      <div className="backend-filters" role="group" aria-label="Backend inventory filter">{["all", "installed", "available", "updates", "unavailable"].map((name) => <Button key={name} size="sm" variant={filter === name ? "secondary" : "ghost"} aria-pressed={filter === name} onClick={() => setFilter(name)}>{name[0].toUpperCase() + name.slice(1)} <span>{rows.filter((row) => name === "all" || (name === "updates" ? backendUpdate(row.variant.state) : category(row.variant.state) === name)).length}</span></Button>)}</div>
      <label className="search-input backend-search"><Search size={15} /><input aria-label="Search backends" placeholder="Find a runtime, backend or capability…" value={query} onChange={(event) => setQuery(event.target.value)} /></label>
      <div className="backend-table">{visible.map(({ recipe, runtime, backend, variant }) => {
        const id = `${recipe}:${backend}`;
        const job = jobs.find((job) => job.id === `backend:${id}` || job.model_name === id);
        const used = models.some((model) => model.loaded && model.recipe === recipe);
        const updating = backendUpdate(variant.state);
        const setup = variant.action?.match(/https:\/\/[^\s"<>]+/)?.[0];
        return <article className="backend-entry" key={id}><div className="backend-entry-identity"><strong>{runtimeName(recipe, runtime)}</strong><span>{acceleratorName(backend)} · {runtime.modality}</span><small>{variant.version || "Version not reported"}</small></div><div className="backend-entry-state"><strong className={backendReady(variant.state) ? "is-ready" : ""}>{backendLabel(variant.state)}</strong><p>{variant.message || (variant.state === "installed" ? "Ready for compatible model registrations. Architecture support is validated when loading." : "No ready execution path.")}</p>{updating && used && <small>Unload workers using this runtime before updating.</small>}{job && <small>{job.running ? "Installation in progress" : job.status}{job.error ? `: ${job.error}` : ""}</small>}</div><div className="backend-entry-action">{job?.running ? <Button variant="outline" size="sm" onClick={openDownloads}>View progress</Button> : setup ? <a href={setup} target="_blank" rel="noreferrer">Setup guide <ExternalLink size={12} /></a> : (variant.state === "installable" || updating) ? <Button variant="outline" size="sm" disabled={!!installing || (updating && used)} title={updating && used ? "Unload workers using this runtime before updating its backend." : undefined} onClick={() => void install(recipe, backend, updating)}><Download size={14} />{installing === id ? "Starting…" : updating ? "Update backend" : "Install backend"}</Button> : null}</div></article>;
      })}</div>
      {!visible.length && <p className="backend-empty">No backends match this filter. Change the filter or search to see the inventory.</p>}
    </>}
  </div>;
}
