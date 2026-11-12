import { useMemo, useState } from "react";
import type { Client } from "./api";
import { backendNames, parseSystemInfo, type BackendRow } from "./hardware";
import { gib } from "./live";
import { activeJob, installEngine, parseEngines, type EngineJob } from "./engines";
import { Badge, Button, Dialog, Empty } from "./ui";
import { useResource } from "./useResource";

export function Hardware({ client }: { client: Client }) {
  const info = useResource(client, "system-info", parseSystemInfo, 30000);
  const [all, setAll] = useState(false);
  const data = info.data;
  const engines = useMemo(() => {
    const groups = new Map<string, BackendRow[]>();
    for (const row of data?.backends ?? []) if (all || row.state === "available") groups.set(row.recipe, [...(groups.get(row.recipe) ?? []), row]);
    return [...groups.values()].sort((left, right) => Number(right.some((row) => row.recommended)) - Number(left.some((row) => row.recommended)) || left[0]!.display_name.localeCompare(right[0]!.display_name));
  }, [data, all]);
  if (!data) return info.error ? <div className="action-row"><p className="inline-error" role="alert">{info.error}</p><Button onClick={() => void info.reload()}>Retry</Button></div> : <p className="muted">Detecting hardware…</p>;
  const available = data.backends.filter((row) => row.state === "available").length;
  return <div className="page-body">
    {info.error && <p className="inline-warning">Showing the last detection. {info.error}</p>}
    <section className="device-grid" aria-label="Detected hardware">
      <article className="device-card"><span className="device-kind">CPU</span><strong>{data.cpu?.name ?? "Unknown CPU"}</strong><span className="muted">{data.cpu?.physical_cores ?? "?"} cores · {data.cpu?.logical_threads ?? "?"} threads{data.cpu?.isa.length ? ` · ${data.cpu.isa.filter((flag) => /avx|amx|neon/.test(flag)).join(", ")}` : ""}</span></article>
      <article className="device-card"><span className="device-kind">Memory</span><strong>{gib(data.memory?.total_bytes)}</strong><span className="muted">{gib(data.memory?.available_bytes)} free</span></article>
      {data.gpus.map((gpu, index) => <article className="device-card" key={index}><span className="device-kind">{gpu.integrated ? "Integrated GPU" : "GPU"}</span><strong>{gpu.name ?? `${gpu.vendor.toUpperCase()} GPU`}</strong><span className="muted">{gib(gpu.vram_total_bytes)} VRAM{gpu.family ? ` · ${gpu.family}` : ""}{gpu.driver_version ? ` · driver ${gpu.driver_version}` : ""}</span>{gpu.render_node_access && gpu.render_node_access !== "read_write" && <span className="inline-warning">No access to the GPU device</span>}</article>)}
      {data.npus.map((npu, index) => <article className="device-card" key={`npu-${index}`}><span className="device-kind">NPU</span><strong>{npu.name ?? `${npu.vendor.toUpperCase()} NPU`}</strong><span className="muted">{npu.family ?? "Unknown generation"}</span></article>)}
      {data.gpus.length === 0 && <article className="device-card"><span className="device-kind">GPU</span><strong>None detected</strong><span className="muted">Models run on the CPU.</span></article>}
    </section>
    <section aria-label="Backends">
      <div className="toolbar"><h2 className="toolbar-title">Backends</h2><span className="muted">{available} available on this machine</span><div className="segmented" role="group" aria-label="Backend filter"><button aria-pressed={!all} onClick={() => setAll(false)}>Available</button><button aria-pressed={all} onClick={() => setAll(true)}>All</button></div></div>
      {engines.length === 0 ? <Empty title="No backends available">Juiced couldn't find a usable backend on this machine.</Empty> : <div className="data-table backend-table" role="table" aria-label="Backends">
        <div role="row" className="data-head"><span role="columnheader">Engine</span><span role="columnheader">Backends</span></div>
        {engines.map((rows) => <div role="row" className="data-row" key={rows[0]!.recipe}>
          <span role="cell"><strong>{rows[0]!.display_name}</strong>{rows.some((row) => row.tier === "experimental") && rows.every((row) => row.tier === "experimental") && <Badge tone="warning">Experimental</Badge>}</span>
          <span role="cell" className="backend-chips">{rows.map((row) => <span key={row.backend} className={`backend-chip ${row.state}`} title={row.reasons.map((reason) => reason.message).join(" ") || (row.recommended ? "Best choice for this machine" : "Available")}>{row.recommended && <span aria-label="Recommended">★ </span>}{backendNames[row.backend] ?? row.backend}{row.state === "unavailable" && all ? <span className="sr-only"> unavailable: {row.reasons.map((reason) => reason.message).join(" ")}</span> : null}</span>)}</span>
        </div>)}
      </div>}
      <p className="field-help">★ marks the backend Juiced picks for new models on this machine. Hover a backend to see why it is or isn't available.</p>
    </section>
  </div>;
}

export function EngineList({ client, online, children }: { client: Client; online: boolean; children: React.ReactNode }) {
  const engines = useResource(client, "juiced/engines", parseEngines, 3000);
  const [installing, setInstalling] = useState<Record<string, EngineJob>>({});
  const [error, setError] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const data = engines.data;
  async function install(engine: string, backend: string) {
    const key = `${engine}:${backend}`;
    setError("");
    try {
      await installEngine(client, engine, backend, (job) => setInstalling((current) => ({ ...current, [key]: job })));
      await engines.reload();
    } catch (failure) { setError(failure instanceof Error ? failure.message : "The engine didn't install."); }
    finally { setInstalling((current) => { const next = { ...current }; delete next[key]; return next; }); }
  }
  if (!data) return engines.error ? <div className="action-row"><p className="inline-error" role="alert">{engines.error}</p><Button onClick={() => void engines.reload()}>Retry</Button></div> : <p className="muted">Loading engines…</p>;
  const rows = data.installable.filter((row) => row.availability === "available" || row.installed_package);
  return <div className="page-body">
    {error && <p className="inline-error" role="alert">{error}</p>}
    <div className="data-table engine-table" role="table" aria-label="Engines">
      <div role="row" className="data-head"><span role="columnheader">Engine</span><span role="columnheader">Backend</span><span role="columnheader" className="numeric">Download</span><span role="columnheader">Status</span></div>
      {rows.map((row) => {
        const key = `${row.engine}:${row.backend}`;
        const job = installing[key] ?? data.jobs.find((entry) => entry.engine === row.engine && entry.backend === row.backend && activeJob(entry));
        const installed = data.installed.find((entry) => entry.package === row.installed_package);
        return <div role="row" className="data-row" key={key} aria-label={`${row.engine} ${backendNames[row.backend] ?? row.backend}`}>
          <span role="cell"><strong>{row.engine}</strong></span>
          <span role="cell">{row.recommended && <span aria-label="Recommended">★ </span>}{backendNames[row.backend] ?? row.backend}</span>
          <span role="cell" className="numeric muted">{row.archive_bytes ? gib(row.archive_bytes) : "—"}</span>
          <span role="cell" className="row-end">{row.installed_package ? <span className="installed-note"><Badge tone="good">Installed</Badge>{installed?.models.length ? <span className="muted"> used by {installed.models.length}</span> : null}</span>
            : job ? <span className="install-progress"><progress aria-label={`Installing ${row.engine} ${row.backend}`} max={job.total_bytes ?? 1} value={job.total_bytes ? job.downloaded_bytes : undefined} /><span className="muted">{job.state}</span></span>
            : <Button variant={row.recommended ? "primary" : "outline"} disabled={!online} onClick={() => void install(row.engine, row.backend)}>Install</Button>}</span>
        </div>;
      })}
    </div>
    <p className="field-help">★ marks the engine Juiced uses for new models on this machine. Engines download from pinned releases and are checked against a published checksum.</p>
    <div className="action-row"><Button onClick={() => setAdvanced(true)}>Import a local engine bundle</Button></div>
    {advanced && <Dialog title="Engine packages" onClose={() => setAdvanced(false)}>{children}</Dialog>}
  </div>;
}
