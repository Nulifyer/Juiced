import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Client } from "./api";
import { normalizeBase, type Snapshot } from "./domain";
import { Inspector, ModelList, type Mutate } from "./Models";
import { ServiceTest, type TestMode } from "./ServiceTest";
import { FindModels } from "./FindModels";
import { Downloading, useActiveDownloads } from "./Downloading";
import { FileInspector, modelFiles, notAdded, useLocalFiles } from "./LocalModels";
import { Cache } from "./Cache";
import { ServiceApis, type ConnectTab } from "./ServiceApis";
import { InstalledRuntimes } from "./InstalledRuntimes";
import { Dashboard } from "./Dashboard";
import { Engines } from "./Backends";
import { Diagnostics } from "./Diagnostics";
import { Aliases } from "./Aliases";
import { Barista, BaristaInvite, invitationKey, useBarista, useBaristaPanel } from "./Barista";
import { IdleDefault } from "./Residency";
import { gib, percent, rate, useSystemStats } from "./live";
import { clearToasts, dismiss, notify, useToasts } from "./toast";
import { Badge, Button, Dialog, Empty, Icon, Menu, Tabs, type IconName } from "./ui";
import logo from "../../../ui/public/juiced-icon.svg";

const destinations = [
  ["dashboard", "Dashboard", "dashboard"], ["models", "Models", "models"], ["policy", "Loading policy", "policy"],
  ["backend", "Engines & hardware", "backend"], ["cache", "Cache", "cache"], ["test", "Test services", "test"],
  ["diagnostics", "Diagnostics", "diagnostics"], ["connect", "Connect", "api"],
] as const satisfies readonly (readonly [string, string, IconName])[];
type Screen = typeof destinations[number][0];

function readLocal(key: string): string {
  try { return localStorage.getItem(key) ?? ""; } catch { return ""; }
}
function writeLocal(key: string, value: string) {
  try { localStorage.setItem(key, value); } catch { /* Storage can be unavailable in private browser sessions. */ }
}

export function App() {
  const [screen, setScreen] = useState<Screen>(() => {
    const saved = readLocal("juiced.rewrite.screen");
    return destinations.some(([id]) => id === saved) ? saved as Screen : saved === "api" ? "connect" : saved === "library" ? "models" : "dashboard";
  });
  const [base, setBase] = useState(() => { try { return normalizeBase(readLocal("juiced.rewrite.base")); } catch { return ""; } });
  const [key, setKey] = useState("");
  const [connectionOpen, setConnectionOpen] = useState(false);
  const [mobileOpen, setMobileOpen] = useState(false);
  const [selected, setSelected] = useState("");
  const [selectedFile, setSelectedFile] = useState("");
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [online, setOnline] = useState(false);
  const [connectionError, setConnectionError] = useState("");
  const [busy, setBusy] = useState(false);
  const [loadingModel, setLoadingModel] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [epoch, setEpoch] = useState(0);
  const [pending, setPending] = useState<(() => void) | null>(null);
  const [testMode, setTestMode] = useState<TestMode>("try");
  const [diagnosticsTab, setDiagnosticsTab] = useState<"performance" | "requests" | "logs">("performance");
  const [connectTab, setConnectTab] = useState<ConnectTab>("protocols");
  const [modelsTab, setModelsTab] = useState<"installed" | "discover" | "downloads" | "aliases">("installed");
  const client = useMemo(() => new Client(base, key), [base, key]);
  const toasts = useToasts();
  const live = useSystemStats(client, online);
  const barista = useBarista(client, online);
  const panel = useBaristaPanel();
  const baristaOpen = panel.open && !!barista.settings;
  const setBaristaOpen = panel.setOpen;
  const inviteKey = invitationKey(base);
  const [dismissed, setDismissed] = useState(() => readLocal(inviteKey) === "dismissed");
  useEffect(() => { setDismissed(readLocal(inviteKey) === "dismissed"); }, [inviteKey]);
  const workspace = useRef<HTMLElement>(null);
  useLayoutEffect(() => { workspace.current?.scrollTo(0, 0); }, [screen, client]);
  const poll = useRef<AbortController | null>(null);
  const adoptingKey = useRef(false);
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
    // Adopting a key the console just created keeps the page, so its one-time secret stays visible.
    if (adoptingKey.current) adoptingKey.current = false;
    else { setSnapshot(null); setOnline(false); setSelected(""); setSelectedFile(""); setDirty(false); clearToasts(); setBusy(false); setLoadingModel(null); }
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

  const closeBarista = useCallback(() => setBaristaOpen(false), [setBaristaOpen]);
  const leave = (action: () => void) => { if (dirty) setPending(() => action); else action(); };
  const mutate: Mutate = async (path, body, success) => {
    if (mutation.current && !mutation.current.controller.signal.aborted) return false;
    const loadModel = path === "load" && body && typeof body === "object" && "model" in body && typeof body.model === "string" ? body.model : null;
    const controller = new AbortController(); mutation.current = { controller, loadModel };
    setLoadingModel(loadModel);
    setBusy(true);
    try {
      const result = await client.request<{ artifact_reference_retained?: boolean }>(path, body, controller.signal);
      if (controller.signal.aborted) return false;
      await refresh();
      if (controller.signal.aborted) return false;
      notify(success + (result.artifact_reference_retained ? " A storage reference remains retained. Check its files before deleting them." : ""));
      return true;
    } catch (error) {
      if (!controller.signal.aborted) { notify(error instanceof Error ? error.message : "The operation failed. Refresh and retry.", "error"); await refresh(); }
      return false;
    } finally { if (!controller.signal.aborted) setBusy(false); if (mutation.current?.controller === controller) { mutation.current = null; setLoadingModel(null); } }
  };
  const stopLoad = () => {
    const operation = mutation.current;
    if (!operation?.loadModel) return;
    operation.controller.abort(); mutation.current = null;
    setBusy(false); setLoadingModel(null);
    notify(`Stopped waiting for ${operation.loadModel}.`);
    void refresh();
  };
  const model = snapshot?.models.find((model) => model.id === selected);
  const installed = screen === "models" && modelsTab === "installed";
  const modelScreen = installed || screen === "policy";
  const local = useLocalFiles(client, modelScreen && online);
  const downloads = useActiveDownloads(client, screen === "models" && online);
  const files = useMemo(() => notAdded(local.data), [local.data]);
  const fileGroup = installed && !model ? local.data?.groups.find((group) => group.id === selectedFile) : undefined;
  useEffect(() => { if (selectedFile && local.data && !local.data.groups.some((group) => group.id === selectedFile)) setSelectedFile(""); }, [local.data, selectedFile]);
  const hasInspector = (!!model || !!fileGroup) && modelScreen;
  const resources = snapshot?.resources;
  const title = destinations.find(([id]) => id === screen)![1];
  const togglePause = () => void mutate("juiced/admission", { paused: !resources?.paused }, resources?.paused ? "Resumed new requests." : "Paused new requests.");
  const openModel = (id: string) => leave(() => { setScreen("models"); setModelsTab("installed"); setSelected(id); setSelectedFile(""); });
  const modelsView = (tab: typeof modelsTab) => leave(() => { setScreen("models"); setModelsTab(tab); setSelected(""); setSelectedFile(""); });
  const status = !online ? "Offline" : resources?.paused ? "Paused" : "Online";
  const gpu = live.stats?.gpus[0];
  const tabs = screen === "models" ? <Tabs label="Models views" value={modelsTab} onChange={modelsView} items={[["installed", "Installed"], ["discover", "Discover"], ["downloads", downloads ? `Downloads ${downloads}` : "Downloads"], ["aliases", "Aliases"]]} />
    : screen === "test" ? <Tabs label="Test mode" value={testMode} onChange={setTestMode} items={[["try", "Try a model"], ["compare", "Compare"]]} />
    : screen === "connect" ? <Tabs label="Connect views" value={connectTab} onChange={setConnectTab} items={[["protocols", "Protocols"], ["keys", "API keys"], ["endpoints", "Endpoints"]]} />
    : screen === "diagnostics" ? <Tabs label="Diagnostics views" value={diagnosticsTab} onChange={setDiagnosticsTab} items={[["performance", "Performance"], ["requests", "Requests"], ["logs", "Logs"]]} />
    : null;

  return <div className={`app-shell ${hasInspector ? "has-inspector" : ""} ${baristaOpen && panel.docked ? "barista-docked" : ""}`}>
    <a className="skip-link" href="#workspace">Skip to workspace</a>
    <header className="topbar">
      <div className="brand"><Button variant="ghost" className="mobile-menu" aria-label="Toggle navigation" aria-expanded={mobileOpen} onClick={() => setMobileOpen(!mobileOpen)}><Icon name="menu" /></Button><img src={logo} alt="" width="22" height="22" /><span>Juiced</span><Badge tone="warning">Preview</Badge></div>
      <div className="topbar-actions">{barista.settings && (barista.settings.enabled || dismissed
        ? <Button variant="ghost" className="barista-toggle" aria-label="Barista" aria-expanded={baristaOpen} onClick={() => { setBaristaOpen(!baristaOpen); if (!baristaOpen) void barista.reload(); }}><Icon name="chat" size={16} /><span className="barista-toggle-label">Barista</span></Button>
        : <BaristaInvite onEnable={() => { setBaristaOpen(true); void barista.reload(); }} onDismiss={() => { writeLocal(inviteKey, "dismissed"); setDismissed(true); }} />)}
      <Menu label="Service status" trigger={<><span className={`status-dot ${status === "Online" ? "good" : status === "Paused" ? "warning" : "bad"}`} /><span>{status}</span><Icon name="chevron" size={14} /></>}>
        <div className="menu-note">{snapshot ? `Juiced ${snapshot.version}` : connectionError || "Not connected"}</div>
        <button role="menuitem" disabled={!online || busy} onClick={togglePause}>{resources?.paused ? "Resume requests" : "Pause new requests"}</button>
        <button role="menuitem" onClick={() => leave(() => setConnectionOpen(true))}>Connection settings</button>
      </Menu></div>
    </header>
    <aside className={`sidebar ${mobileOpen ? "open" : ""}`} aria-label="Navigation">
      <nav aria-label="Destinations">{destinations.map(([id, label, icon]) => <button key={id} aria-current={screen === id ? "page" : undefined} onClick={() => { setMobileOpen(false); leave(() => { setScreen(id); setSelected(""); setSelectedFile(""); }); }}><Icon name={icon} size={16} /><span>{label}</span>{id === "models" && snapshot && <span className="nav-count">{snapshot.models.length}</span>}</button>)}</nav>
      <div className="sidebar-footer">
        <p className="sidebar-version" aria-label="Service version" title={snapshot ? `${snapshot.models.filter((model) => model.loaded).length} loaded · ${snapshot.models.filter((model) => model.policy !== "disabled").length} available` : undefined}>{snapshot ? `Juiced ${snapshot.version} · ${snapshot.models.filter((model) => model.loaded).length} loaded` : "Not connected"}</p>
        <a href="https://github.com/Nulifyer/Juiced" target="_blank" rel="noreferrer" aria-label="Source and releases" title="Source and releases"><Icon name="github" size={15} /></a>
      </div>
    </aside>
    {mobileOpen && <button className="nav-scrim" aria-label="Close navigation" onClick={() => setMobileOpen(false)} />}
    <main ref={workspace} id="workspace" className={`workspace ${modelScreen ? "model-screen" : ""} ${screen === "test" ? "test-screen" : ""}`} tabIndex={-1}>
      {resources?.paused && <div className="paused-bar" role="region" aria-label="Maintenance admission"><span><strong>New requests paused.</strong> Running requests can finish.</span><Button disabled={!online || busy} onClick={togglePause}>Resume requests</Button></div>}
      {connectionError && <div className="notice error" role="alert"><span>{snapshot ? "Showing the last observation. " : ""}{connectionError}</span><Button onClick={() => leave(() => setConnectionOpen(true))}>Connection settings</Button></div>}
      <div className="page-header"><h1>{title}</h1>{tabs}</div>
      {!snapshot ? <Empty title={refreshing ? "Connecting to Juiced" : "Service unavailable"}>{refreshing ? "Reading models and service state…" : "Check the address and API key in Connection settings."}</Empty> : <>
        {screen === "dashboard" && <Dashboard snapshot={snapshot} stats={live.stats} history={live.history} onOpenModel={openModel} onTest={() => setScreen("test")} />}
        {modelScreen && <div className="model-workspace"><ModelList models={snapshot.models} selected={selected} onSelect={(id) => leave(() => { setSelected(id); setSelectedFile(""); })} mode={screen as "models" | "policy"} mutate={mutate} busy={busy} online={online} idleControl={screen === "policy" ? <IdleDefault client={client} online={online} /> : undefined} files={installed ? files : undefined} inventory={installed ? local.data : undefined} selectedFile={fileGroup?.id} onSelectFile={(id) => leave(() => { setSelected(""); setSelectedFile(id); })} onDiscover={() => modelsView("discover")} />
          {hasInspector && model && <Inspector key={`${model.id}:${epoch}`} model={model} busy={busy} online={online} onClose={() => leave(() => setSelected(""))} onDirty={setDirty} mutate={mutate} client={client} stopLoad={loadingModel === model.id ? stopLoad : undefined} inventory={local.data} onRemoved={() => { const { groups } = modelFiles(model, local.data); setSelected(""); if (installed && groups.length === 1 && groups[0]!.usedBy.every((id) => id === model.id)) setSelectedFile(groups[0]!.id); void local.reload(); }} />}
          {hasInspector && fileGroup && <FileInspector key={`${fileGroup.id}:${epoch}`} group={fileGroup} client={client} online={online} onClose={() => setSelectedFile("")} onRegistered={refresh} onOpenModel={openModel} onChanged={local.reload} />}</div>}
        {screen === "models" && modelsTab === "discover" && <FindModels key={epoch} client={client} online={online} onRegistered={refresh} onOpenModel={openModel} onDownloads={() => modelsView("downloads")} />}
        {screen === "models" && modelsTab === "downloads" && <Downloading key={epoch} client={client} online={online} onSearch={() => modelsView("discover")} />}
        {screen === "models" && modelsTab === "aliases" && <Aliases key={epoch} client={client} models={snapshot.models} online={online} onOpenModel={openModel} />}
        {screen === "diagnostics" && <Diagnostics client={client} snapshot={snapshot} stats={live.stats} history={live.history} tab={diagnosticsTab} />}
        {screen === "test" && <ServiceTest key={`${epoch}:${testMode}`} mode={testMode} models={snapshot.models} client={client} online={online} />}
        {screen === "connect" && <ServiceApis key={epoch} base={base} client={client} tab={connectTab} models={snapshot.models} online={online} consoleKey={key} onAdoptKey={(next) => { adoptingKey.current = true; setKey(next); }} />}
        {screen === "backend" && <Engines key={epoch} client={client} online={online} importer={<InstalledRuntimes client={client} online={online} paused={resources?.paused === true} />} />}
        {screen === "cache" && <Cache key={epoch} client={client} online={online} models={snapshot.models} />}
      </>}
    </main>
    <footer className="global-footer" aria-label="Live usage">
      <span><Icon name="cpu" size={13} />CPU <strong>{percent(live.stats?.cpu_percent)}</strong></span>
      <span>RAM <strong>{gib(live.stats?.memory?.used_bytes)}</strong></span>
      {gpu && <><span>GPU <strong>{percent(gpu.gpu_percent)}</strong></span><span>VRAM <strong>{gib(gpu.memory_used_bytes)}</strong></span></>}
      {live.stats?.npu_percent != null && <span>NPU <strong>{percent(live.stats.npu_percent)}</strong></span>}
      <span className="footer-right">Last decode <strong>{rate(live.stats?.last_inference?.decode_tokens_per_second)}</strong></span>
      <span>Active <strong>{resources?.active_requests ?? "—"}</strong></span>
    </footer>
    <div className="toasts" aria-live="polite">
      {toasts.map((toast) => <div key={toast.id} className={`toast ${toast.tone}`} role={toast.tone === "error" ? "alert" : "status"}><span>{toast.text}</span><Button variant="ghost" aria-label={toast.tone === "error" ? "Dismiss error" : "Dismiss notification"} onClick={() => dismiss(toast.id)}><Icon name="close" size={14} /></Button></div>)}
    </div>
    {barista.settings && <Barista open={baristaOpen} onClose={closeBarista} models={snapshot?.models ?? []} ready={!!snapshot} layout={panel.layout} client={client} online={online} settings={barista.settings} onSettings={barista.setSettings} onChanged={() => { if (!dirty) setEpoch((value) => value + 1); void refresh(); }} />}
    {connectionOpen && <ConnectionDialog base={base} apiKey={key} version={snapshot?.version} onClose={() => setConnectionOpen(false)} onSave={(nextBase, nextKey) => { setBase(nextBase); setKey(nextKey); setEpoch((value) => value + 1); writeLocal("juiced.rewrite.base", nextBase); setConnectionOpen(false); }} />}
    {pending && <Dialog title="Discard unsaved changes?" onClose={() => setPending(null)}><p>Your model changes have not been saved.</p><div className="dialog-actions"><Button onClick={() => setPending(null)}>Keep editing</Button><Button variant="danger" onClick={() => { setDirty(false); setEpoch(epoch + 1); pending(); setPending(null); }}>Discard changes</Button></div></Dialog>}
  </div>;
}

function ConnectionDialog({ base, apiKey, version, onClose, onSave }: { base: string; apiKey: string; version?: string; onClose: () => void; onSave: (base: string, key: string) => void }) {
  const [draftBase, setDraftBase] = useState(base);
  const [draftKey, setDraftKey] = useState(apiKey);
  const [error, setError] = useState("");
  return <Dialog title="Connection settings" onClose={onClose}><form onSubmit={(event) => { event.preventDefault(); try { onSave(normalizeBase(draftBase), draftKey); } catch (error) { setError((error as Error).message); } }}>
    <label htmlFor="service-origin">Service origin</label><input id="service-origin" type="text" inputMode="url" value={draftBase} placeholder="Same origin" onChange={(event) => setDraftBase(event.target.value)} /><p className="field-help">Leave empty to use the service that served this page.</p>
    <label htmlFor="api-key">API key</label><input id="api-key" type="password" autoComplete="off" value={draftKey} onChange={(event) => setDraftKey(event.target.value)} /><p className="field-help">Kept in this tab only. Use an operator key to manage models.</p>
    {error && <p className="inline-error" role="alert">{error}</p>}<p className="field-help">Connected service: {version ?? "unknown version"}</p><div className="dialog-actions"><Button onClick={onClose}>Cancel</Button><Button variant="primary" type="submit">Connect</Button></div>
  </form></Dialog>;
}

