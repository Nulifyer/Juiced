import { useMemo, useState, type ReactNode } from "react";
import type { Client } from "./api";
import { gpuMemory, parseSystemInfo, type SystemInfo } from "./hardware";
import { gib } from "./live";
import { downloadSize, engineGroups, installEngine, parseEngines, shownByDefault, type BackendChip, type EngineJob } from "./engines";
import { Button, Empty, HelpTip, Icon, Switch } from "./ui";
import { useResource } from "./useResource";

export function Engines({ client, online, importer }: { client: Client; online: boolean; importer: ReactNode }) {
  const info = useResource(client, "system-info", parseSystemInfo, 30000);
  const engines = useResource(client, "juiced/engines", parseEngines, 3000);
  const [installing, setInstalling] = useState<Record<string, EngineJob>>({});
  const [error, setError] = useState("");
  const [all, setAll] = useState(false);
  const groups = useMemo(() => engines.data ? engineGroups(engines.data, info.data?.backends ?? [], installing) : [], [engines.data, info.data, installing]);
  const visible = groups.map((group) => ({ ...group, chips: all ? group.chips : group.chips.filter(shownByDefault) })).filter((group) => group.chips.length > 0);
  async function install(chip: BackendChip) {
    setError("");
    try {
      await installEngine(client, chip.engine, chip.backend, (job) => setInstalling((current) => ({ ...current, [chip.key]: job })));
      await engines.reload();
    } catch (failure) { setError(failure instanceof Error ? failure.message : "The engine didn't install."); }
    finally { setInstalling((current) => { const next = { ...current }; delete next[chip.key]; return next; }); }
  }
  const failure = info.error || engines.error;
  if (!engines.data) return failure ? <div className="action-row"><p className="inline-error" role="alert">{failure}</p><Button onClick={() => { void info.reload(); void engines.reload(); }}>Retry</Button></div> : <p className="muted">Loading engines…</p>;
  const chips = groups.flatMap((group) => group.chips);
  const installed = chips.filter((chip) => chip.state === "installed").length, ready = chips.filter((chip) => chip.state === "installable").length;
  return <div className="page-body engines-page">
    {info.data ? <MachineStrip data={info.data} /> : <p className={info.error ? "inline-warning" : "muted"}>{info.error ? `Hardware detection failed. ${info.error}` : "Detecting hardware…"}</p>}
    {info.data && info.error && <p className="inline-warning">Showing the last detection. {info.error}</p>}
    {engines.error && <p className="inline-warning">Showing the last engine list. {engines.error}</p>}
    {error && <p className="inline-error" role="alert">{error}</p>}
    <section aria-label="Engines">
      <div className="toolbar">
        <h2 className="toolbar-title">Engines</h2>
        <HelpTip label="Engines">★ is the build Juiced picks for new models on this machine. Engines download from pinned releases and are checked against a published checksum. Choose an engine for a model in its Settings while it's unloaded.</HelpTip>
        <span className="muted">{installed} installed · {ready} ready to install</span>
        <span className="spacer" />
        <Switch checked={all} label="Show all engines" onChange={setAll}>Show all</Switch>
        <HelpTip label="Show all engines">Also lists builds that can't be installed here: they need other hardware or another OS, or Juiced has no installer for them yet. Hover a build to see why.</HelpTip>
      </div>
      {visible.length === 0 ? <Empty title="No engines for this machine">Turn on Show all to see what Juiced knows about.</Empty> : <div className="data-table engine-list" role="table" aria-label="Engines">
        {visible.map((group) => <div role="row" className={`data-row ${group.installable ? "" : "uninstallable"}`} key={group.key} aria-label={group.name}>
          <span role="rowheader" className="engine-name"><strong>{group.name}</strong><span className="muted">{group.purpose}</span></span>
          <span role="cell"><ul className="engine-chips" aria-label={`${group.name} builds`}>{group.chips.map((chip) => <li key={chip.key}><Chip chip={chip} name={group.name} online={online} onInstall={() => void install(chip)} /></li>)}</ul></span>
        </div>)}
      </div>}
      <div className="engines-foot">{importer}</div>
    </section>
  </div>;
}

function Chip({ chip, name, online, onInstall }: { chip: BackendChip; name: string; online: boolean; onInstall: () => void }) {
  const star = chip.recommended ? <span className="engine-star" aria-label="Recommended">★</span> : null;
  const version = chip.version ? <span className="engine-version" title={chip.version} aria-hidden="true">{chip.version}</span> : null;
  if (chip.state === "installable") return <button type="button" className={`engine-chip installable ${chip.replaces ? "update" : ""}`} disabled={!online} title={`${chip.replaces ? `Update from ${chip.replaces}` : "Install"}${chip.version ? ` ${chip.version}` : ""}${chip.download_bytes ? ` · ${downloadSize(chip.download_bytes)} download` : ""}${chip.replaces ? ". Models switch to it when you choose it in their Settings." : ""}`} aria-label={`${chip.replaces ? "Update" : "Install"} ${name} ${chip.label}${chip.download_bytes ? `, ${downloadSize(chip.download_bytes)}` : ""}${chip.recommended ? ", recommended" : ""}`} onClick={onInstall}>
    <Icon name={chip.replaces ? "upload" : "download"} size={12} />{star}{chip.replaces ? `Update ${chip.label}` : chip.label}{version}{chip.download_bytes ? <span className="muted">{downloadSize(chip.download_bytes)}</span> : null}
  </button>;
  if (chip.state === "installing") {
    const total = chip.job?.total_bytes, done = chip.job?.downloaded_bytes ?? 0;
    return <span className="engine-chip installing" role="status">{star}{chip.label}<progress aria-label={`Installing ${name} ${chip.label}`} max={total ?? 1} value={total ? done : undefined} /><span className="muted">{total ? `${Math.floor(done / total * 100)}%` : chip.job?.state ?? "queued"}</span></span>;
  }
  if (chip.state === "installed") return <span className="engine-chip installed" title={chip.models ? `Installed · used by ${chip.models} model${chip.models === 1 ? "" : "s"}` : "Installed · not used by any model yet"}>
    <span aria-hidden="true">✓</span><span className="sr-only">Installed: </span>{star}{chip.label}{version}{chip.models ? <span className="muted">{chip.models} model{chip.models === 1 ? "" : "s"}</span> : null}
  </span>;
  return <span className={`engine-chip ${chip.state}`} title={chip.reason || undefined}>{chip.label}{chip.reason ? <span className="sr-only">: {chip.reason}</span> : null}</span>;
}

function MachineStrip({ data }: { data: SystemInfo }) {
  const cpu = data.cpu, memory = data.memory;
  const isa = cpu?.isa.filter((flag) => /avx|amx|neon|sve/.test(flag)) ?? [];
  return <section className="machine-strip" aria-label="This machine">
    <Device kind="CPU" name={cpu?.name ?? "Unknown CPU"} fact={cpu?.physical_cores ? `${cpu.physical_cores} cores` : ""} tip={cpu && <>{cpu.physical_cores ?? "?"} cores, {cpu.logical_threads ?? "?"} threads.{isa.length ? ` Supports ${isa.join(", ")}.` : ""}</>} />
    <Device kind="Memory" name={gib(memory?.total_bytes)} fact={memory?.available_bytes != null ? `${gib(memory.available_bytes)} free` : ""} />
    {data.gpus.map((gpu, index) => {
      const blocked = !!gpu.render_node_access && gpu.render_node_access !== "read_write";
      return <Device key={index} kind="GPU" name={gpu.name ?? `${gpu.vendor.toUpperCase()} GPU`} fact={gpuMemory(gpu) ? `${gib(gpuMemory(gpu))}${gpu.integrated ? " shared" : ""}` : ""} warning={blocked ? "No access" : undefined}
        tip={<>{gpu.integrated ? `Integrated${gpu.gtt_total_bytes ? `: ${gib(gpu.vram_total_bytes)} dedicated plus ${gib(gpu.gtt_total_bytes)} mapped from system memory` : ""}` : "Dedicated"}{gpu.family ? ` · ${gpu.family}` : ""}{gpu.compute_capability ? ` · compute ${gpu.compute_capability}` : ""}{gpu.driver_version ? ` · driver ${gpu.driver_version}` : ""}.{blocked ? " The service can't open this GPU's device, so GPU builds won't run. Pass the device into the container." : ""}</>} />;
    })}
    {data.gpus.length === 0 && <Device kind="GPU" name="None" fact="models run on the CPU" />}
    {data.npus.map((npu, index) => <Device key={`npu-${index}`} kind="NPU" name={npu.name ?? `${npu.vendor.toUpperCase()} NPU`} fact={npu.name ? "" : npu.family ?? ""} warning={npu.access && npu.access !== "read_write" ? "No access" : undefined} />)}
  </section>;
}

function Device({ kind, name, fact, tip, warning }: { kind: string; name: string; fact: string; tip?: ReactNode; warning?: string }) {
  return <div className="machine-item" role="group" aria-label={kind}>
    <span className="device-kind">{kind}</span>
    <strong title={name}>{name}</strong>
    {fact && <span className="muted">{fact}</span>}
    {warning && <span className="inline-warning">{warning}</span>}
    {tip && <HelpTip label={`${kind} details`}>{tip}</HelpTip>}
  </div>;
}
