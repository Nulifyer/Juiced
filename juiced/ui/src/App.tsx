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
  ChevronDown,
  MoreHorizontal,
  Plus,
  RefreshCw,
  AlertCircle,
} from "lucide-react";
import { Menu } from "@base-ui/react/menu";
import { ContextMenu } from "@base-ui/react/context-menu";
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
import { Playground } from "./components/Playground";
import { api, type Model, type Thread } from "./api";
const destinations = [
  ["playground", "Playground", MessageSquare],
  ["models", "Models", Server],
  ["policy", "Loading policy", SlidersHorizontal],
  ["library", "Library", Library],
  ["cache", "Cache", Database],
] as const;
const tasks = [
  ["Chat", MessageSquare],
  ["Transcribe", Mic],
  ["Speak", Volume2],
  ["Images", Image],
  ["Compare", Columns2],
] as const;
export default function App() {
  const [screen, setScreen] = useState(
      () => localStorage.getItem("juiced.screen") || "models",
    ),
    [models, setModels] = useState<Model[]>([]),
    [service, setService] = useState("All services"),
    [task, setTask] = useState("Chat"),
    [jobs, setJobs] = useState<any[]>([]),
    [threads, setThreads] = useState<Thread[]>([]),
    [thread, setThread] = useState<Thread | null>(null),
    [notice, setNotice] = useState(""),
    [online, setOnline] = useState(false),
    [ready, setReady] = useState(false),
    [mobile, setMobile] = useState(false),
    [libraryTab, setLibraryTab] = useState("discover"),
    [inspected, setInspected] = useState<Model | null>(null),
    [options, setOptions] = useState(""),
    [cache, setCache] = useState<any>(),
    [saving, setSaving] = useState(false),
    [threadSearch, setThreadSearch] = useState("");
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
  const refreshThreads = useCallback(async () => {
    const j = await api("juiced/threads");
    setThreads(
      (j.data || []).sort(
        (a: Thread, b: Thread) => (b.updated || 0) - (a.updated || 0),
      ),
    );
  }, []);
  useEffect(() => {
    void refresh().catch((e) => {
      notify(e.message);
      setOnline(false);
    });
    void refreshThreads().catch((e) => notify(e.message));
    void api("juiced/cache")
      .then(setCache)
      .catch((e) => notify(e.message));
    const timer = setInterval(() => {
      void refresh().catch(() => setOnline(false));
    }, 3000);
    return () => clearInterval(timer);
  }, [refresh, refreshThreads, notify]);
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
  const updateThread = useCallback((t: Thread) => {
    setThread(t);
    setThreads((prev) =>
      [t, ...prev.filter((x) => x.id !== t.id)].sort(
        (a, b) => (b.updated || 0) - (a.updated || 0),
      ),
    );
  }, []);
  async function openThread(id: string) {
    try {
      const t = await api<Thread>("juiced/thread?id=" + encodeURIComponent(id));
      setThread(t);
      setScreen("playground");
      setTask("Chat");
      setMobile(false);
    } catch (e) {
      notify((e as Error).message);
    }
  }
  async function shelf(t: Thread, value: "working" | "settled") {
    try {
      const current = await api<Thread>(
        "juiced/thread?id=" + encodeURIComponent(t.id),
      );
      const updated = await api<Thread>("juiced/thread", {
        ...current,
        shelf: value,
      });
      if (thread?.id === t.id) setThread(updated);
      await refreshThreads();
    } catch (e) {
      notify((e as Error).message);
    }
  }
  function threadMenu(t: Thread, Item: any) {
    return (
      <>
        <Item className="menu-item" onClick={() => void openThread(t.id)}>
          Open thread
        </Item>
        <Item
          className="menu-item"
          onClick={() =>
            void shelf(t, t.shelf === "settled" ? "working" : "settled")
          }
        >
          Move to {t.shelf === "settled" ? "Working" : "Settled"}
        </Item>
        <Item className="menu-item" onClick={() => setScreen("cache")}>
          Cache settings…
        </Item>
      </>
    );
  }
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
  async function saveCache() {
    try {
      setCache(await api("juiced/cache", cache.settings));
      notify("Cache settings saved.");
    } catch (e) {
      notify((e as Error).message);
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
          <span className="brand-mark">◒</span>
          <strong>Juiced</strong>
          <span className="version">0.1.0</span>
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
            {activeJobs.length ? activeJobs.length + " active" : "Transfers"}
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
        <SidebarGroup>
          <div className="sidebar-label">Services</div>
          {(screen === "playground"
            ? tasks
            : ([
                ["All services", Server],
                ...tasks.filter((t) => t[0] !== "Compare"),
              ] as const)
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
                if (screen === "playground") {
                  setTask(label);
                  setMobile(false);
                } else setService(label);
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
                        (label === "All services" || m.service === label),
                    ).length
                  }
                </span>
              )}
            </button>
          ))}
        </SidebarGroup>
        {screen === "playground" && task === "Chat" && (
          <SidebarGroup>
            <Button
              variant="ghost"
              size="sm"
              className="new-chat"
              onClick={() => {
                setThread(null);
                setMobile(false);
              }}
            >
              <Plus size={14} />
              New chat
            </Button>
            <input
              className="thread-search"
              aria-label="Search threads"
              placeholder="Search threads…"
              value={threadSearch}
              onChange={(e) => setThreadSearch(e.target.value)}
            />
            {(["working", "settled"] as const).map((group) => (
              <details
                className="thread-shelf"
                open
                key={group}
                onDragOver={(e) => {
                  e.preventDefault();
                  e.currentTarget.classList.add("drop-over");
                }}
                onDragLeave={(e) =>
                  e.currentTarget.classList.remove("drop-over")
                }
                onDrop={(e) => {
                  e.preventDefault();
                  e.currentTarget.classList.remove("drop-over");
                  const t = threads.find(
                    (t) => t.id === e.dataTransfer.getData("juiced/thread"),
                  );
                  if (t) void shelf(t, group);
                }}
              >
                <summary>
                  <ChevronDown size={12} />
                  {group === "working" ? "Working" : "Settled"}
                  <span className="count">
                    {threads.filter((t) => t.shelf === group).length}
                  </span>
                </summary>
                {threads
                  .filter(
                    (t) =>
                      t.shelf === group &&
                      t.title
                        .toLowerCase()
                        .includes(threadSearch.toLowerCase()),
                  )
                  .map((t) => (
                    <ContextMenu.Root key={t.id}>
                      <ContextMenu.Trigger render={<div />}>
                        <div
                          className={
                            "thread-row " +
                            (thread?.id === t.id ? "selected" : "")
                          }
                          draggable
                          onDragStart={(e) =>
                            e.dataTransfer.setData("juiced/thread", t.id)
                          }
                        >
                          <button
                            className="thread-open"
                            title={t.title}
                            onClick={() => void openThread(t.id)}
                          >
                            {t.title}
                          </button>
                          <Menu.Root>
                            <Menu.Trigger
                              render={
                                <Button
                                  size="icon-micro"
                                  variant="ghost"
                                  aria-label={"Actions for " + t.title}
                                />
                              }
                            >
                              <MoreHorizontal size={14} />
                            </Menu.Trigger>
                            <Menu.Portal>
                              <Menu.Positioner align="end">
                                <Menu.Popup className="menu-popup">
                                  {threadMenu(t, Menu.Item)}
                                </Menu.Popup>
                              </Menu.Positioner>
                            </Menu.Portal>
                          </Menu.Root>
                        </div>
                      </ContextMenu.Trigger>
                      <ContextMenu.Portal>
                        <ContextMenu.Positioner>
                          <ContextMenu.Popup className="menu-popup">
                            {threadMenu(t, ContextMenu.Item)}
                          </ContextMenu.Popup>
                        </ContextMenu.Positioner>
                      </ContextMenu.Portal>
                    </ContextMenu.Root>
                  ))}
              </details>
            ))}
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
            models={models}
            policy={screen === "policy"}
            service={service}
            notify={notify}
            refresh={refresh}
            onInspect={setInspected}
          />
        )}
        {screen === "library" && (
          <Acquisition
            models={models}
            jobs={jobs}
            notify={notify}
            refresh={refresh}
            tab={libraryTab}
            setTab={setLibraryTab}
          />
        )}
        {screen === "playground" && (
          <Playground
            models={models}
            thread={thread}
            task={task}
            notify={notify}
            onThread={updateThread}
            openModels={() => setScreen("models")}
          />
        )}
        {screen === "cache" && (
          <>
            <header className="page-heading">
              <div>
                <h1>Cache</h1>
                <p>
                  Retained computation is separate from thread history and model
                  weights.
                </p>
              </div>
            </header>
            {cache && (
              <>
                <div className="settings-grid">
                  <label>
                    Cache mode
                    <select
                      value={cache.settings.mode}
                      onChange={(e) =>
                        setCache({
                          ...cache,
                          settings: { ...cache.settings, mode: e.target.value },
                        })
                      }
                    >
                      <option value="memory">Memory only</option>
                      <option value="off">Off for playground requests</option>
                      <option
                        value="persistent"
                        disabled={!cache.persistent_supported}
                      >
                        Persistent{" "}
                        {cache.persistent_supported ? "" : "· not qualified"}
                      </option>
                    </select>
                  </label>
                  <label>
                    Retention after last use
                    <input
                      type="number"
                      min="1"
                      max="365"
                      value={cache.settings.retention_days}
                      onChange={(e) =>
                        setCache({
                          ...cache,
                          settings: {
                            ...cache.settings,
                            retention_days: Number(e.target.value),
                          },
                        })
                      }
                    />
                    <small>
                      Days · applies when persistent snapshots are enabled
                    </small>
                  </label>
                  <label>
                    Snapshot disk budget
                    <input
                      type="number"
                      min="1"
                      max="256"
                      value={cache.settings.disk_budget_gib}
                      onChange={(e) =>
                        setCache({
                          ...cache,
                          settings: {
                            ...cache.settings,
                            disk_budget_gib: Number(e.target.value),
                          },
                        })
                      }
                    />
                    <small>GiB · separate from downloaded weights</small>
                  </label>
                </div>
                <Button onClick={() => void saveCache()}>
                  Save cache settings
                </Button>
                <div className="callout">
                  <Database size={17} />
                  <div>
                    <strong>
                      {cache.persistent_supported
                        ? "Persistent snapshots available"
                        : "Persistent resume is not enabled on this release"}
                    </strong>
                    <p>{cache.reason}</p>
                    <p>
                      Live prompt reuse can help while the same engine keeps
                      compatible state. Reopening a thread after restart may
                      require a cold prefill.
                    </p>
                  </div>
                </div>
              </>
            )}
            {cache?.settings?.last_error && (
              <p className="inline-error">
                Last snapshot: {cache.settings.last_error}
              </p>
            )}
            {threads.map((t) => {
              const snapshots = (cache?.snapshots || []).filter(
                (s: any) => s.thread === t.id,
              );
              const snapshot = snapshots.sort(
                (a: any, b: any) => b.last_used - a.last_used,
              )[0];
              return (
                <article className="simple-row" key={t.id}>
                  <div>
                    <strong>{t.title}</strong>
                    <p>{t.model || "No model selected"} · messages retained</p>
                    <p>
                      {snapshot
                        ? `${snapshot.retained ? "Retained" : "Saved"} · ${(snapshot.bytes / 1024 ** 2).toFixed(1)} MiB · ${snapshot.tokens?.toLocaleString() || 0} tokens`
                        : "No saved snapshot · live memory reuse may still apply"}
                    </p>
                  </div>
                  <div className="row-actions cache-actions">
                    <Button
                      variant="ghost"
                      size="compact"
                      onClick={() => void openThread(t.id)}
                    >
                      Open
                    </Button>
                    <Button
                      variant="ghost"
                      size="compact"
                      onClick={async () => {
                        try {
                          const current = await api<Thread>(
                            "juiced/thread?id=" + encodeURIComponent(t.id),
                          );
                          await api("juiced/thread", {
                            ...current,
                            retain_cache: !current.retain_cache,
                          });
                          await refreshThreads();
                          setCache(await api("juiced/cache"));
                          notify(
                            current.retain_cache
                              ? "Released snapshot protection."
                              : "This thread’s next compatible snapshot will be retained within its disk budget.",
                          );
                        } catch (e) {
                          notify((e as Error).message);
                        }
                      }}
                    >
                      {t.retain_cache ? "Release" : "Retain"}
                    </Button>
                    <Button
                      variant="ghost"
                      size="compact"
                      disabled={!snapshot}
                      onClick={async () => {
                        try {
                          await api("juiced/cache/forget", { thread: t.id });
                          setCache(await api("juiced/cache"));
                          notify(
                            "Saved computation forgotten. Messages and model weights remain; live engine prefixes may remain until replaced.",
                          );
                        } catch (e) {
                          notify((e as Error).message);
                        }
                      }}
                    >
                      Forget saved
                    </Button>
                  </div>
                </article>
              );
            })}
          </>
        )}
        {!ready && screen !== "playground" && (
          <p className="page-note">
            {online
              ? "The native model catalog is initializing. This page refreshes automatically."
              : "The model service is reconnecting. Existing work remains saved."}
          </p>
        )}
      </main>
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
          if (!open) setInspected(null);
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
