import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Client } from "./api";
import { bytes, normalizeBase, workerReady, type Snapshot } from "./domain";
import { Inspector, ModelList, type Mutate } from "./Models";
import { ServiceTest } from "./ServiceTest";
import { Library } from "./Library";
import { Cache } from "./Cache";
import { ServiceApis } from "./ServiceApis";
import { RuntimeCatalog } from "./RuntimeCatalog";
import { InstalledRuntimes } from "./InstalledRuntimes";
import { Badge, Button, Dialog, Empty, Icon, type IconName } from "./ui";
import logo from "../../../ui/public/juiced-icon.svg";

const destinations = [
  ["models", "Models", "models"], ["policy", "Loading policy", "policy"], ["library", "Library", "library"],
  ["backend", "Backends & hardware", "backend"], ["cache", "Cache", "cache"], ["api", "Service APIs", "api"],
  ["diagnostics", "Diagnostics", "diagnostics"], ["test", "Test services", "test"],
] as const satisfies readonly (readonly [string, string, IconName])[];
type Screen = typeof destinations[number][0];
const descriptions: Record<Screen, string> = {
  models: "Inspect readiness and manage shared model workers.", policy: "Choose which models are available to your clients.",
  library: "Acquire validated weights and register their model engines.", backend: "Inspect configured workers and runtime integration boundaries.",
  cache: "Save and restore explicit, session-bound engine snapshots.", api: "Connect external clients to this appliance.",
  diagnostics: "Inspect reservations and recent request outcomes.", test: "Verify one model with a temporary request.",
};

function readLocal(key: string): string {
  try { return localStorage.getItem(key) ?? ""; } catch { return ""; }
}
function writeLocal(key: string, value: string) {
  try { localStorage.setItem(key, value); } catch { /* Storage can be unavailable in private browser sessions. */ }
}

export function App() {
  const [screen, setScreen] = useState<Screen>(() => {
    const saved = readLocal("juiced.rewrite.screen");
    return destinations.some(([id]) => id === saved) ? saved as Screen : "models";
  });
  const [base, setBase] = useState(() => { try { return normalizeBase(readLocal("juiced.rewrite.base")); } catch { return ""; } });
  const [key, setKey] = useState("");
  const [connectionOpen, setConnectionOpen] = useState(false);
  const [mobileOpen, setMobileOpen] = useState(false);
  const [selected, setSelected] = useState("");
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [online, setOnline] = useState(false);
  const [connectionError, setConnectionError] = useState("");
  const [notice, setNotice] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [loadingModel, setLoadingModel] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [epoch, setEpoch] = useState(0);
  const [pending, setPending] = useState<(() => void) | null>(null);
  const client = useMemo(() => new Client(base, key), [base, key]);
  const workspace = useRef<HTMLElement>(null);
  useLayoutEffect(() => { workspace.current?.scrollTo(0, 0); }, [screen, client]);
  const poll = useRef<AbortController | null>(null);
  const mutation = useRef<{ controller: AbortController; loadModel: string | null } | null>(null);
  const refresh = useCallback(async () => {
    poll.current?.abort();
    const controller = new AbortController(); poll.current = controller;
    setRefreshing(true);
    try {
      const next = await client.snapshot(controller.signal);
      if (!controller.signal.aborted) { setSnapshot(next); setOnline(true); setConnectionError(""); }
    } catch (error) {
      if (!controller.signal.aborted) { setOnline(false); setConnectionError(error instanceof Error ? error.message : "The service is unavailable. Check the connection, then retry."); }
    } finally { if (!controller.signal.aborted) { setRefreshing(false); if (poll.current === controller) poll.current = null; } }
  }, [client]);
  useEffect(() => {
    setSnapshot(null); setOnline(false); setSelected(""); setDirty(false); setError(""); setNotice(""); setBusy(false); setLoadingModel(null);
    void refresh();
    const timer = setInterval(() => { if (!poll.current) void refresh(); }, 3000);
    return () => { clearInterval(timer); poll.current?.abort(); mutation.current?.controller.abort(); };
  }, [refresh]);
  useEffect(() => { writeLocal("juiced.rewrite.screen", screen); setMobileOpen(false); }, [screen]);
  useEffect(() => {
    if (!mobileOpen) return;
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") setMobileOpen(false); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mobileOpen]);

  const leave = (action: () => void) => { if (dirty) setPending(() => action); else action(); };
  const mutate: Mutate = async (path, body, success) => {
    if (mutation.current && !mutation.current.controller.signal.aborted) return false;
    const loadModel = path === "load" && body && typeof body === "object" && "model" in body && typeof body.model === "string" ? body.model : null;
    const controller = new AbortController(); mutation.current = { controller, loadModel };
    setLoadingModel(loadModel);
    setBusy(true); setError(""); setNotice("");
    try {
      const result = await client.request<{ artifact_reference_retained?: boolean }>(path, body, controller.signal);
      if (controller.signal.aborted) return false;
      await refresh();
      if (controller.signal.aborted) return false;
      setNotice(success + (result.artifact_reference_retained ? " A storage reference remains retained. Inspect Library before deleting weights." : ""));
      return true;
    } catch (error) {
      if (!controller.signal.aborted) { setError(error instanceof Error ? error.message : "The operation failed. Refresh and retry."); await refresh(); }
      return false;
    } finally { if (!controller.signal.aborted) setBusy(false); if (mutation.current?.controller === controller) { mutation.current = null; setLoadingModel(null); } }
  };
  const stopLoad = () => {
    const operation = mutation.current;
    if (!operation?.loadModel) return;
    operation.controller.abort(); mutation.current = null;
    setBusy(false); setLoadingModel(null); setError("");
    setNotice(`Stopped waiting for ${operation.loadModel}. Refresh observes any remaining startup or cleanup.`);
    void refresh();
  };
  const model = snapshot?.models.find((model) => model.id === selected);
  const hasInspector = !!model && ["models", "policy"].includes(screen);
  const resources = snapshot?.resources;
  const readyWorkers = snapshot?.models.reduce((count, model) => count + Number(workerReady(model)), 0) ?? 0;
  const hotModels = snapshot?.models.reduce((count, model) => count + Number(model.policy === "hot"), 0) ?? 0;
  const title = destinations.find(([id]) => id === screen)![1];

  return <div className={`app-shell ${hasInspector ? "has-inspector" : ""}`}>
    <a className="skip-link" href="#workspace">Skip to workspace</a>
    <header className="topbar"><div className="brand"><Button variant="ghost" className="mobile-menu" aria-label="Toggle navigation" aria-expanded={mobileOpen} onClick={() => setMobileOpen(!mobileOpen)}><Icon name="menu" /></Button><img src={logo} alt="" width="24" height="24" /><span>Juiced</span><span className="brand-divider" /><span className="muted">Model appliance</span></div><div className="topbar-actions"><Badge tone="warning">Rewrite preview</Badge><Button variant="ghost" aria-label="Connection settings" onClick={() => leave(() => setConnectionOpen(true))}><Icon name="settings" /></Button></div></header>
    <aside className={`sidebar ${mobileOpen ? "open" : ""}`} aria-label="Navigation">
      <div className="sidebar-label">Appliance</div>
      <nav>{destinations.map(([id, label, icon]) => <button key={id} aria-current={screen === id ? "page" : undefined} onClick={() => leave(() => { setScreen(id); setSelected(""); })}><Icon name={icon} /><span>{label}</span>{id === "models" && snapshot && <span className="nav-count">{snapshot.models.length}</span>}</button>)}</nav>
      <div className="sidebar-bottom"><div><span className={`status-dot ${online ? "good" : "bad"}`} /><span>{online ? "Connected" : refreshing && !snapshot ? "Connecting…" : "Disconnected"}</span></div><span className="muted">Service {snapshot?.version ?? "unknown"}</span><Button variant="ghost" onClick={() => leave(() => setConnectionOpen(true))}>Connection settings</Button></div>
    </aside>
    {mobileOpen && <button className="nav-scrim" aria-label="Close navigation" onClick={() => setMobileOpen(false)} />}
    <main ref={workspace} id="workspace" className={`workspace ${["models", "policy"].includes(screen) ? "model-screen" : ""}`} tabIndex={-1}>
      <div className="workspace-header">
        <div className="page-header"><div><h1>{title}</h1><p className="muted">{descriptions[screen]}</p></div><Button aria-label="Refresh service state" disabled={refreshing} onClick={() => void refresh()}><Icon name="refresh" /><span>{refreshing ? "Refreshing…" : "Refresh"}</span></Button></div>
        {connectionError && <div className="notice error" role="alert"><span>{snapshot ? "Showing the last observation. " : ""}{connectionError}</span><Button onClick={() => leave(() => setConnectionOpen(true))}>Connection settings</Button></div>}
        {error && <div className="notice error" role="alert"><span>{error}</span><Button variant="ghost" aria-label="Dismiss error" onClick={() => setError("")}><Icon name="close" /></Button></div>}
        {notice && <div className="notice success" role="status"><span>{notice}</span><Button variant="ghost" aria-label="Dismiss notification" onClick={() => setNotice("")}><Icon name="close" /></Button></div>}
        {snapshot && ["models", "policy"].includes(screen) && <div className="summary-band"><div><strong>{readyWorkers}</strong><span>{readyWorkers === 1 ? "worker ready" : "workers ready"}</span></div><div><strong>{hotModels}</strong><span>{hotModels === 1 ? "Hot model" : "Hot models"}</span></div><div><strong>{resources?.active_requests ?? "Unavailable"}</strong><span>active requests</span></div><div><strong>{bytes(resources?.reserved_bytes ?? null)}</strong><span>declared reservation</span></div></div>}
      </div>
      {!snapshot ? <Empty title={refreshing ? "Connecting to Juiced" : "Service unavailable"}>{refreshing ? "Reading model inventory and service state…" : "Open Connection settings to check the service URL and API key, then refresh."}</Empty> : <>
        {["models", "policy"].includes(screen) && <>
          <div className="model-workspace"><ModelList models={snapshot.models} selected={selected} onSelect={(id) => leave(() => setSelected(id))} mode={screen as "models" | "policy" | "library"} />{hasInspector && <Inspector key={`${model.id}:${epoch}`} model={model} busy={busy} online={online} onClose={() => leave(() => setSelected(""))} onDirty={setDirty} mutate={mutate} client={client} stopLoad={loadingModel === model.id ? stopLoad : undefined} />}</div>
        </>}
        {!hasInspector && <div className={`maintenance-bar ${resources?.paused ? "paused" : ""}`} role="region" aria-label="Maintenance admission"><div><strong>{resources?.paused ? "New requests paused" : "Accepting new requests"}</strong><p className="field-help">Existing requests can finish. Saved loading policies stay unchanged.</p></div><Button disabled={!online || busy} onClick={() => void mutate("juiced/admission", { paused: !resources?.paused }, resources?.paused ? "Resumed new requests." : "Paused new requests for maintenance.")}>{resources?.paused ? "Resume requests" : "Pause new requests"}</Button></div>}
        {screen === "library" && <Library key={epoch} client={client} online={online} models={snapshot.models} refreshModels={refresh} onInspect={(id) => { setScreen("models"); setSelected(id); }} />}
        {screen === "diagnostics" && <section className="content-pane"><h2>Resource reservations</h2><p className="muted">Configured reservations control admission. They are not measured memory use.</p><dl className="facts resource-facts"><div><dt>Declared budget</dt><dd>{bytes(resources?.resident_budget_bytes ?? null)}</dd></div><div><dt>Reserved</dt><dd>{bytes(resources?.reserved_bytes ?? null)}</dd></div>{resources?.cache_control_memory?.state === "accounted" && <><div><dt>Cache metadata reservation</dt><dd>{bytes(resources.cache_control_memory.metadata_bytes)}</dd></div><div><dt>Cache work and responses</dt><dd>{bytes(resources.cache_control_memory.scratch_bytes)}</dd></div></>}{resources?.runtime_control_memory?.state === "accounted" && <><div><dt>Runtime metadata reservation</dt><dd>{bytes(resources.runtime_control_memory.metadata_bytes)}</dd></div><div><dt>Retained runtime bindings</dt><dd>{bytes(resources.runtime_control_memory.binding_bytes)}</dd></div><div><dt>Runtime work and results</dt><dd>{bytes(resources.runtime_control_memory.scratch_bytes)}</dd></div></>}<div><dt>Measured memory</dt><dd>Unavailable</dd></div><div><dt>Active requests</dt><dd>{resources?.active_requests ?? "Unavailable"}</dd></div></dl>{resources?.cache_control_memory?.state === "accounted" && <p className="field-help">Cache memory is included in Reserved. Metadata keeps its reservation while the cache is open. Unloading a worker preserves that reservation.</p>}{resources?.cache_control_memory?.state === "unavailable" && <p className="inline-warning">Cache metadata could not be admitted and remains unopened. Restore host capacity and restart the service to use snapshots.</p>}{resources?.runtime_control_memory?.state === "accounted" && <p className="field-help">Runtime package metadata and retained bindings share Reserved. They stay charged independently of loaded workers. These ceilings are not measured process memory.</p>}<div className="section-heading"><h2>Recent requests</h2><span className="muted">Latest 128 · memory only</span></div><p className="field-help">Timing, model, operation and outcome only. No prompts, responses or credentials.</p>{snapshot.requests.length === 0 ? <Empty title="No completed requests">Run a temporary service test or send a request from an external client.</Empty> : <div className="request-list">{snapshot.requests.map((record) => <div className="request-row" key={record.id}><div><strong>{record.model}</strong><code>{record.operation}</code></div><Badge tone={record.outcome === "completed" ? "good" : record.outcome === "cancelled" ? "quiet" : "bad"}>{record.outcome}</Badge><span className="numeric">{record.status ?? "—"}</span><span className="numeric muted">{record.elapsed_ms} ms</span></div>)}</div>}</section>}
        {screen === "test" && <ServiceTest key={epoch} models={snapshot.models} client={client} online={online} />}
        {screen === "api" && <ServiceApis key={epoch} base={base} client={client} />}
        {screen === "backend" && <section className="content-pane"><h2>Configured HTTP workers</h2><p className="muted">Each model runs in a separate subprocess with its configured engine. Device compatibility and hardware telemetry are unavailable in this preview.</p>{snapshot.models.length === 0 ? <Empty title="No workers configured">Register an engine and model in the isolated service configuration.</Empty> : snapshot.models.map((model) => <div className="backend-row" key={model.id}><div><h3>{model.id}</h3><span className="muted">{model.max_inflight} request slots · {bytes(model.resident_bytes)} declared reservation</span></div><Badge tone={model.loaded ? "good" : "quiet"}>{model.loaded ? `Running · PID ${model.pid}` : "Not running"}</Badge></div>)}<InstalledRuntimes key={epoch} client={client} online={online} paused={resources?.paused === true} memory={resources?.runtime_control_memory} /><RuntimeCatalog key={epoch} client={client} online={online} /></section>}
        {screen === "cache" && <Cache key={epoch} client={client} online={online} models={snapshot.models} />}
      </>}
    </main>
    <footer className="global-footer"><div><span className={`status-dot ${online ? "good" : "bad"}`} />{online ? resources?.paused ? "New requests paused" : "Service online" : "Service unavailable"}</div><span>Hardware fit unqualified</span><span>{resources ? `${resources.active_requests} active calls` : "Active calls unavailable"}</span><span className="footer-reservation">{bytes(resources?.reserved_bytes ?? null)} reserved</span></footer>
    {connectionOpen && <ConnectionDialog base={base} apiKey={key} onClose={() => setConnectionOpen(false)} onSave={(nextBase, nextKey) => { setBase(nextBase); setKey(nextKey); setEpoch((value) => value + 1); writeLocal("juiced.rewrite.base", nextBase); setConnectionOpen(false); }} />}
    {pending && <Dialog title="Discard unsaved changes?" onClose={() => setPending(null)}><p>Your model policy draft has not been saved.</p><div className="dialog-actions"><Button onClick={() => setPending(null)}>Keep editing</Button><Button variant="danger" onClick={() => { setDirty(false); setEpoch(epoch + 1); pending(); setPending(null); }}>Discard changes</Button></div></Dialog>}
  </div>;
}

function ConnectionDialog({ base, apiKey, onClose, onSave }: { base: string; apiKey: string; onClose: () => void; onSave: (base: string, key: string) => void }) {
  const [draftBase, setDraftBase] = useState(base);
  const [draftKey, setDraftKey] = useState(apiKey);
  const [error, setError] = useState("");
  return <Dialog title="Connection settings" onClose={onClose}><form onSubmit={(event) => { event.preventDefault(); try { onSave(normalizeBase(draftBase), draftKey); } catch (error) { setError((error as Error).message); } }}>
    <label htmlFor="service-origin">Service origin</label><input id="service-origin" type="text" inputMode="url" value={draftBase} placeholder="Same origin" onChange={(event) => setDraftBase(event.target.value)} /><p className="field-help">Leave empty when the console is served by this appliance. Use the isolated Rust service, usually http://127.0.0.1:13306.</p>
    <label htmlFor="api-key">API key</label><input id="api-key" type="password" autoComplete="off" value={draftKey} onChange={(event) => setDraftKey(event.target.value)} /><p className="field-help">Stored only in this tab's memory. Use an operator key to inspect private inventories and manage workers.</p>
    {error && <p className="inline-error" role="alert">{error}</p>}<div className="dialog-actions"><Button onClick={onClose}>Cancel</Button><Button variant="primary" type="submit">Connect</Button></div>
  </form></Dialog>;
}

