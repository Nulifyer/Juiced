import {
  RuntimeMetrics,
  useRuntimeTelemetry,
} from "./components/RuntimeMetrics";
import { RuntimeLogs, RuntimeActivity } from "./components/RuntimeLogs";
import { useLogStream } from "./logs/useLogStream";
import { ModelRemoval } from "./components/ModelRemoval";
import { useCallback, useEffect, useState } from "react";
import {
  MessageSquare,
  Server,
  SlidersHorizontal,
  Library,
  Database,
  Mic,
  Volume2,
  Image,
  Columns2,
  Menu as MenuIcon,
  X,
  RefreshCw,
  AlertCircle,
  Search,
  Download,
  Upload,
  HardDrive,
  Terminal,
} from "lucide-react";
import { Dialog } from "@base-ui/react/dialog";
import { Button } from "./components/t3/button";
import {
  SidebarHeader,
  SidebarGroup,
  SidebarMenu,
  SidebarMenuItem,
} from "./components/t3/sidebar";
import { ModelLibrary, policyNames } from "./components/ModelLibrary";
import { Acquisition } from "./components/Acquisition";
import { CacheWorkspace } from "./components/CacheWorkspace";
import { Playground } from "./components/Playground";
import { api, type Model } from "./api";
import { ServiceEndpoints } from "./components/ServiceEndpoints";
const destinations = [
  ["models", "Models", Server],
  ["policy", "Loading policy", SlidersHorizontal],
  ["library", "Library", Library],
  ["cache", "Cache", Database],
  ["services", "Service APIs", Server],
  ["logs", "Logs", Terminal],
  ["playground", "Test services", MessageSquare],
] as const;
const tasks = [
  ["Chat", MessageSquare],
  ["Transcribe", Mic],
  ["Speak", Volume2],
  ["Images", Image],
  ["Compare", Columns2],
] as const;
export default function App() {
  const telemetry = useRuntimeTelemetry();
  const logs = useLogStream();
  const [screen, setScreen] = useState(
      () => localStorage.getItem("juiced.screen") || "models",
    ),
    [models, setModels] = useState<Model[]>([]),
    [service, setService] = useState("All services"),
    [task, setTask] = useState("Chat"),
    [jobs, setJobs] = useState<any[]>([]),
    [notice, setNotice] = useState(""),
    [online, setOnline] = useState(false),
    [ready, setReady] = useState(false),
    [mobile, setMobile] = useState(false),
    [libraryTab, setLibraryTab] = useState("discover"),
    [inspected, setInspected] = useState<Model | null>(null),
    [options, setOptions] = useState(""),
    [cache, setCache] = useState<any>(),
    [saving, setSaving] = useState(false),
    [removalRequested, setRemovalRequested] = useState(false),
    [preferredModel, setPreferredModel] = useState("");
  useEffect(() => {
    if (!notice) return;
    const timer = setTimeout(() => setNotice(""), 8000);
    return () => clearTimeout(timer);
  }, [notice]);
  const notify = useCallback((s: string) => setNotice(s), []);
  const refresh = useCallback(async () => {
    const [catalog, transfers] = await Promise.all([
      api("juiced/catalog"),
      api("juiced/transfers").catch(() => api("downloads")),
    ]);
    setModels(catalog.data || []);
    setJobs(Array.isArray(transfers) ? transfers : transfers.data || []);
    setOnline(true);
    setReady(Boolean(catalog.data?.length));
  }, []);
  useEffect(() => {
    void refresh().catch((e) => {
      notify(e.message);
      setOnline(false);
    });
    void api("juiced/cache")
      .then(setCache)
      .catch((e) => notify(e.message));
    const timer = setInterval(() => {
      void refresh().catch(() => setOnline(false));
    }, 3000);
    return () => clearInterval(timer);
  }, [refresh, notify]);
  useEffect(() => {
    localStorage.setItem("juiced.screen", screen);
    setMobile(false);
  }, [screen]);
  useEffect(() => {
    if (screen !== "cache") return;
    const load = () =>
      void api("juiced/cache")
        .then(setCache)
        .catch((e) => notify(e.message));
    load();
    const timer = setInterval(load, 3000);
    return () => clearInterval(timer);
  }, [screen, notify]);
  useEffect(() => {
    if (!inspected) return;
    setOptions(
      JSON.stringify((inspected as any).recipe_options || {}, null, 2),
    );
  }, [inspected]);
  useEffect(() => {
    if (mobile) {
      const close = (e: KeyboardEvent) => {
        if (e.key === "Escape") setMobile(false);
      };
      document.addEventListener("keydown", close);
      return () => document.removeEventListener("keydown", close);
    }
  }, [mobile]);
  async function saveOptions() {
    if (!inspected) return;
    setSaving(true);
    try {
      await api(
        "models/" + encodeURIComponent(inspected.id) + "/options",
        JSON.parse(options),
      );
      notify(
        "Saved deployment options. Unload/reload to apply load-time changes.",
      );
      await refresh();
    } catch (e) {
      notify((e as Error).message);
    } finally {
      setSaving(false);
    }
  }
  const activeJobs = jobs.filter(
    (j) => j.running || j.status === "interrupted",
  );
  return (
    <div className="app-shell">
      <header className="app-topbar">
        <div className="brand-row">
          <Button
            className="mobile-trigger"
            variant="ghost"
            size="icon-sm"
            aria-label="Toggle navigation"
            aria-expanded={mobile}
            onClick={() => setMobile(!mobile)}
          >
            <MenuIcon size={18} />
          </Button>
          <img
            className="brand-mark"
            src="/juiced-icon.svg"
            alt=""
            width="26"
            height="26"
          />
          <strong>Juiced</strong>
        </div>
        <div className="topbar-right">
          <span className={"connection " + (online ? "" : "offline")}>
            <span className="state-dot loaded" />
            {online
              ? ready
                ? "Catalog ready"
                : "Preparing catalog…"
              : "Reconnecting…"}
          </span>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              setLibraryTab("transfers");
              setScreen("library");
            }}
          >
            <DownloadActivity />
            Downloads{activeJobs.length ? " · " + activeJobs.length : ""}
          </Button>
        </div>
      </header>
      {mobile && (
        <button
          className="sidebar-scrim"
          aria-label="Close navigation"
          onClick={() => setMobile(false)}
        />
      )}
      <aside className={"app-sidebar " + (mobile ? "mobile-open" : "")}>
        <SidebarHeader>
          <SidebarMenu>
            {destinations.map(([id, label, Icon]) => (
              <SidebarMenuItem key={id}>
                <button
                  className={"sidebar-link " + (screen === id ? "active" : "")}
                  aria-current={screen === id ? "page" : undefined}
                  onClick={() => setScreen(id)}
                >
                  <Icon size={16} />
                  <span>{label}</span>
                </button>
              </SidebarMenuItem>
            ))}
          </SidebarMenu>
        </SidebarHeader>
        {(screen === "playground" ||
          screen === "models" ||
          screen === "policy") && (
          <SidebarGroup>
            <div className="sidebar-label">
              {screen === "playground" ? "Try a service" : "Service filters"}
            </div>
            {(screen === "playground"
              ? tasks
              : ([
                  ["All services", Server],
                  ...tasks.filter(
                    (t) =>
                      t[0] !== "Compare" &&
                      models.some(
                        (m) =>
                          m.downloaded &&
                          m.available !== false &&
                          m.service === t[0],
                      ),
                  ),
                  ...[
                    ...new Set(
                      models
                        .filter((m) => m.downloaded && m.available !== false)
                        .map((m) => m.service),
                    ),
                  ]
                    .filter((s) => !tasks.some((t) => t[0] === s))
                    .map((s) => [s, Database]),
                ] as [string, typeof Server][])
            ).map(([label, Icon]) => (
              <button
                key={label}
                className={
                  "sidebar-link " +
                  ((screen === "playground" ? task : service) === label
                    ? "active"
                    : "")
                }
                onClick={() => {
                  if (screen === "playground") setTask(label);
                  else setService(label);
                  setMobile(false);
                }}
              >
                <Icon size={15} />
                <span>{label}</span>
                {screen !== "playground" && (
                  <span className="count">
                    {
                      models.filter(
                        (m) =>
                          m.downloaded &&
                          m.available !== false &&
                          (label === "All services" || m.service === label),
                      ).length
                    }
                  </span>
                )}
              </button>
            ))}
          </SidebarGroup>
        )}
        {screen === "library" && (
          <SidebarGroup>
            <div className="sidebar-label">Your library</div>
            {[
              ["discover", "Discover models", Search],
              ["downloaded", "Downloaded", HardDrive],
              ["transfers", "Downloads", Download],
              ["import", "Import GGUF", Upload],
            ].map(([id, label, Icon]: any) => (
              <button
                key={id}
                className={
                  "sidebar-link " + (libraryTab === id ? "active" : "")
                }
                onClick={() => {
                  setLibraryTab(id);
                  setService("All services");
                  setMobile(false);
                }}
              >
                <Icon size={15} />
                <span>{label}</span>
                {id === "downloaded" && (
                  <span className="count">
                    {models.filter((m) => m.downloaded).length}
                  </span>
                )}
                {id === "transfers" && activeJobs.length > 0 && (
                  <span className="count">{activeJobs.length}</span>
                )}
              </button>
            ))}
          </SidebarGroup>
        )}
        {screen === "cache" && (
          <SidebarGroup>
            <div className="sidebar-label">Saved computation</div>
            <div className="sidebar-context-note">
              <Database size={20} />
              <strong>
                {cache?.settings?.mode === "persistent"
                  ? "Disk snapshots enabled"
                  : cache?.settings?.mode === "memory"
                    ? "Memory caching"
                    : cache?.settings?.mode === "off"
                      ? "Caching off"
                      : "Loading settings…"}
              </strong>
              <p>
                Consumer session state can be reused independently of model
                residency.
              </p>
              <span>
                {cache?.settings?.retention_days || 7} day expiry ·{" "}
                {cache?.settings?.disk_budget_gib || 8} GiB budget
              </span>
            </div>
          </SidebarGroup>
        )}
        {screen === "playground" && (
          <SidebarGroup>
            <p className="sidebar-context-note">
              Temporary service tests. Results are cleared when you leave or
              reload this page.
            </p>
          </SidebarGroup>
        )}
        <footer className="sidebar-footer">
          <span>
            {models.filter((m) => m.loaded).length} loaded ·{" "}
            {models.filter((m) => m.downloaded).length} downloaded
          </span>
          <button
            onClick={() => void refresh().catch((e) => notify(e.message))}
          >
            <RefreshCw size={12} />
            Refresh
          </button>
          <a
            href="https://github.com/Nulifyer/Juiced"
            target="_blank"
            rel="noreferrer"
          >
            Source & release notes
          </a>
        </footer>
      </aside>
      <main
        className={
          "app-main " + (screen === "playground" ? "playground-main" : "")
        }
      >
        {(screen === "models" || screen === "policy") && (
          <ModelLibrary
            activity={
              <RuntimeActivity logs={logs} openLogs={() => setScreen("logs")} />
            }
            models={models}
            policy={screen === "policy"}
            service={service}
            notify={notify}
            refresh={refresh}
            onInspect={(m) => {
              setRemovalRequested(false);
              setInspected(m);
            }}
            onRemove={(m) => {
              setRemovalRequested(true);
              setInspected(m);
            }}
            onPolicy={() => setScreen("policy")}
            onPlay={(m) => {
              setPreferredModel(m.id);
              setTask(m.service);
              setScreen("playground");
            }}
          />
        )}
        {screen === "library" && (
          <Acquisition
            models={models}
            service={service}
            jobs={jobs}
            notify={notify}
            refresh={refresh}
            tab={libraryTab}
            setTab={setLibraryTab}
            onInspect={(m) => {
              setRemovalRequested(false);
              setInspected(m);
            }}
          />
        )}
        {screen === "playground" && (
          <Playground
            models={models}
            task={task}
            initialModel={preferredModel}
            notify={notify}
            openModels={() => setScreen("models")}
          />
        )}
        {screen === "logs" && <RuntimeLogs logs={logs} />}
        {screen === "services" && (
          <ServiceEndpoints models={models} notify={notify} />
        )}
        {screen === "cache" && (
          <CacheWorkspace
            cache={cache}
            models={models}
            setCache={setCache}
            notify={notify}
          />
        )}
        {!ready && screen !== "playground" && (
          <p className="page-note">
            {online
              ? "The native model catalog is initializing. This page refreshes automatically."
              : "The model service is reconnecting. Existing work remains saved."}
          </p>
        )}
      </main>
      <RuntimeMetrics data={telemetry} />
      {notice && (
        <div className="notice" role="status">
          <AlertCircle size={15} />
          <span>{notice}</span>
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="Dismiss notification"
            onClick={() => setNotice("")}
          >
            <X size={14} />
          </Button>
        </div>
      )}
      <Dialog.Root
        open={!!inspected}
        onOpenChange={(open) => {
          if (!open) {
            setInspected(null);
            setRemovalRequested(false);
          }
        }}
      >
        <Dialog.Portal>
          <Dialog.Backdrop className="dialog-backdrop" />
          <Dialog.Popup className="model-drawer">
            <div className="dialog-heading">
              <Dialog.Title>{inspected?.id}</Dialog.Title>
              <Dialog.Close
                render={
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Close model details"
                  />
                }
              >
                <X size={17} />
              </Dialog.Close>
            </div>
            <Dialog.Description>
              Native model identity, deployment and saved load options.
            </Dialog.Description>
            {inspected && (
              <>
                <dl className="model-facts">
                  <dt>Service</dt>
                  <dd>{inspected.service}</dd>
                  <dt>Backend</dt>
                  <dd>{inspected.recipe}</dd>
                  <dt>Policy</dt>
                  <dd>{policyNames[inspected.policy]}</dd>
                  <dt>State</dt>
                  <dd>{inspected.operation?.state || inspected.state}</dd>
                  <dt>Context</dt>
                  <dd>
                    {inspected.context_length?.toLocaleString() || "Unknown"}{" "}
                    tokens
                  </dd>
                </dl>
                <h2>Saved deployment options</h2>
                <p className="page-note">
                  These options affect the next load. Saving does not restart a
                  running model.
                </p>
                <textarea
                  className="options-editor"
                  aria-label="Deployment options JSON"
                  value={options}
                  onChange={(e) => setOptions(e.target.value)}
                  spellCheck={false}
                />
                <Button disabled={saving} onClick={() => void saveOptions()}>
                  Save deployment options
                </Button>
                <ModelRemoval
                  model={inspected}
                  focusRequested={removalRequested}
                  notify={notify}
                  onRemoved={async () => {
                    setInspected(null);
                    await refresh();
                  }}
                />
                <details className="raw-details">
                  <summary>Complete catalog record</summary>
                  <pre>{JSON.stringify(inspected, null, 2)}</pre>
                </details>
              </>
            )}
          </Dialog.Popup>
        </Dialog.Portal>
      </Dialog.Root>
    </div>
  );
}
function DownloadActivity() {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
    >
      <path d="M12 3v12m-5-5 5 5 5-5M4 17v4h16v-4" />
    </svg>
  );
}
