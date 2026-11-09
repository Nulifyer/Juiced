import {
  RuntimeMetrics,
  useRuntimeTelemetry,
} from "./components/RuntimeMetrics";
import { Diagnostics } from "./components/Diagnostics";
import { RuntimeActivity } from "./components/RuntimeLogs";
import { useLogStream } from "./logs/useLogStream";
import { ModelWorkspace } from "./components/ModelWorkspace";
import { useCallback, useEffect, useRef, useState } from "react";
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
  Github,
} from "lucide-react";
import { Button } from "./components/t3/button";
import {
  SidebarHeader,
  SidebarGroup,
  SidebarMenu,
  SidebarMenuItem,
} from "./components/t3/sidebar";
import { ModelLibrary } from "./components/ModelLibrary";
import { Acquisition } from "./components/Acquisition";
import { CacheWorkspace } from "./components/CacheWorkspace";
import { Playground } from "./components/Playground";
import { api, type Model } from "./api";
import { ServiceEndpoints } from "./components/ServiceEndpoints";
import { BackendInventory } from "./components/BackendInventory";
const destinations = [
  ["models", "Models", Server],
  ["policy", "Loading policy", SlidersHorizontal],
  ["library", "Library", Library],
  ["backends", "Backends & hardware", Server],
  ["cache", "Cache", Database],
  ["services", "Service APIs", Server],
  ["diagnostics", "Diagnostics", Terminal],
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
    [serviceVersion, setServiceVersion] = useState<string>(),
    [ready, setReady] = useState(false),
    [mobile, setMobile] = useState(false),
    [libraryTab, setLibraryTab] = useState("discover"),
    [inspected, setInspected] = useState<Model | null>(null),
    [acquireRepo, setAcquireRepo] = useState<{ repo?: string; query?: string; runtime?: string }>(),
    [cache, setCache] = useState<any>(),
    [removalRequested, setRemovalRequested] = useState(false),
    [preferredModel, setPreferredModel] = useState("");
  const [diagnosticTab, setDiagnosticTab] = useState("performance");
  const navigationGuard = useRef<((action: () => void) => void) | undefined>(undefined);
  const registerNavigationGuard = useCallback((guard?: (action: () => void) => void) => { navigationGuard.current = guard; }, []);
  function navigate(action: () => void) { setMobile(false); if (navigationGuard.current) navigationGuard.current(action); else action(); }
  function chooseScreen(next: string) { navigate(() => { setScreen(next); setMobile(false); }); }
  const inspectable = ["models", "policy", "library"].includes(screen);
  useEffect(() => {
    if (!notice) return;
    const timer = setTimeout(() => setNotice(""), 8000);
    return () => clearTimeout(timer);
  }, [notice]);
  const notify = useCallback((s: string) => setNotice(s), []);
  const refresh = useCallback(async () => {
    const [catalog, transfers, state] = await Promise.all([
      api("juiced/catalog"),
      api("juiced/transfers").catch(() => api("downloads")),
      api("juiced/state").catch(() => undefined),
    ]);
    setServiceVersion(typeof state?.version === "string" && state.version.trim() ? state.version : undefined);
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
    if (mobile) {
      const close = (e: KeyboardEvent) => {
        if (e.key === "Escape") setMobile(false);
      };
      document.addEventListener("keydown", close);
      return () => document.removeEventListener("keydown", close);
    }
  }, [mobile]);
  const activeJobs = jobs.filter(
    (j) => j.running || ["queued", "interrupted", "paused", "error"].includes(j.status),
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
              navigate(() => { setInspected(null); setLibraryTab("transfers"); setScreen("library"); });
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
                  onClick={() => chooseScreen(id)}
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
        <footer className="sidebar-footer">
          <div className="sidebar-footer-summary">
            <span aria-label="Juiced server version" title={serviceVersion ? `Juiced ${serviceVersion}` : "Juiced version unavailable"}>
              Juiced {serviceVersion || "—"}
            </span>
            <span title="Currently loaded models / downloaded models">
              {models.filter((m) => m.loaded).length} loaded ·{" "}
              {models.filter((m) => m.downloaded).length} downloaded
            </span>
          </div>
          <button
            aria-label="Refresh catalog"
            title="Refresh catalog"
            onClick={() => void refresh().catch((e) => notify(e.message))}
          >
            <RefreshCw size={14} />
          </button>
          <a
            href="https://github.com/Nulifyer/Juiced"
            target="_blank"
            rel="noreferrer"
            aria-label="Source and releases"
            title="Source and releases"
          >
            <Github size={14} />
          </a>
        </footer>
      </aside>
      <main
        className={
          "app-main " + (screen === "playground" ? "playground-main" : "") + (inspected && inspectable ? " has-inspector" : "")
        }
      >
        <div className="workspace-primary">
        {(screen === "models" || screen === "policy") && (
          <ModelLibrary
            activity={
              <RuntimeActivity logs={logs} openLogs={() => navigate(() => { setDiagnosticTab("logs"); setScreen("diagnostics"); })} />
            }
            models={models}
            policy={screen === "policy"}
            service={service}
            notify={notify}
            refresh={refresh}
            onInspect={(m) => {
              navigate(() => { setRemovalRequested(false); setInspected(m); });
            }}
            onRemove={(m) => {
              navigate(() => { setRemovalRequested(true); setInspected(m); });
            }}
            onPolicy={() => chooseScreen("policy")}
            onPlay={(m) => navigate(() => {
              setPreferredModel(m.id);
              setTask(m.service);
              setScreen("playground");
            })}
          />
        )}
        {screen === "library" && (
          <Acquisition
            models={models}
            onBackends={() => chooseScreen("backends")}
            initialRepository={acquireRepo}
            onRepositoryOpened={() => setAcquireRepo(undefined)}
            service={service}
            jobs={jobs}
            notify={notify}
            refresh={refresh}
            tab={libraryTab}
            setTab={setLibraryTab}
            onInspect={(m) => {
              navigate(() => { setRemovalRequested(false); setInspected(m); });
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
        {["logs", "diagnostics"].includes(screen) && <Diagnostics initialTab={screen === "logs" ? "logs" : diagnosticTab} data={telemetry} logs={logs} models={models} inspect={(model) => { setInspected(model); setScreen("models"); }} />}
        {screen === "backends" && <BackendInventory models={models} jobs={jobs} openDownloads={() => { setInspected(null); setLibraryTab("transfers"); setScreen("library"); }} />}
        {screen === "services" && (
          <ServiceEndpoints models={models} notify={notify} />
        )}
        {screen === "cache" && (
          <CacheWorkspace
            registerNavigationGuard={registerNavigationGuard}
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
        </div>
      {inspected && inspectable && (
        <ModelWorkspace
          key={inspected.id}
          model={models.find((m) => m.id === inspected.id) || inspected}
          models={models}
          removalRequested={removalRequested}
          registerNavigationGuard={registerNavigationGuard}
          onClose={() => {
            setInspected(null);
            setRemovalRequested(false);
          }}
          onInspect={(m) => {
            setRemovalRequested(false);
            setInspected(m);
          }}
          onFindVariant={(model, runtime) => {
            setInspected(null);
            setAcquireRepo({ query: model.id.replace(/[-_](?:IQ[0-9][A-Z0-9_]*|Q[0-9][A-Z0-9_]*|GGUF)$/ig, ""), runtime });
            setLibraryTab("discover"); setScreen("library");
          }}
          onBrowse={(repo) => {
            setInspected(null);
            setAcquireRepo({ repo });
            setLibraryTab("discover");
            setScreen("library");
          }}
          onTest={(m) => {
            setPreferredModel(m.id);
            setTask(m.service);
            setScreen("playground");
          }}
          refresh={refresh}
          notify={notify}
        />
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
