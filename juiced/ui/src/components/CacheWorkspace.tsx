import { useEffect, useState } from "react";
import {
  Database,
  Search,
  Pin,
  ArrowUpRight,
  History,
  Settings2,
  Clock3,
  Trash2,
  ChevronRight,
} from "lucide-react";
import { Dialog } from "@base-ui/react/dialog";
import { api, bytes, type Thread } from "../api";
import { Button } from "./t3/button";
import "./CacheWorkspace.css";
function age(value?: number) {
  if (!value) return "Not used yet";
  const days = Math.floor((Date.now() / 1000 - value) / 86400);
  return days > 1 ? `${days} days ago` : days === 1 ? "Yesterday" : "Today";
}
export function CacheWorkspace({
  cache,
  setCache,
  threads,
  refreshThreads,
  openThread,
  notify,
}: {
  cache: any;
  setCache: (value: any) => void;
  threads: Thread[];
  refreshThreads: () => Promise<void>;
  openThread: (id: string) => void;
  notify: (s: string) => void;
}) {
  const [q, setQ] = useState(""),
    [filter, setFilter] = useState("all"),
    [selected, setSelected] = useState<string>(),
    [draft, setDraft] = useState<any>(),
    [dirty, setDirty] = useState(false),
    [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!dirty) setDraft(cache?.settings);
  }, [cache?.settings, dirty]);
  const snapshots: any[] = cache?.snapshots || [],
    total = snapshots.reduce((sum, s) => sum + (s.bytes || 0), 0),
    budget = (cache?.settings?.disk_budget_gib || 0) * 1024 ** 3;
  const rows = threads.map((t) => ({
    t,
    saved: snapshots
      .filter((s) => s.thread === t.id)
      .sort((a, b) => b.last_used - a.last_used)[0],
  }));
  const visible = rows.filter(
    ({ t, saved }) =>
      [t.title, t.model].join(" ").toLowerCase().includes(q.toLowerCase()) &&
      (filter === "all" ||
        (filter === "saved" && saved) ||
        (filter === "retained" && t.retain_cache) ||
        (filter === "history" && !saved)),
  );
  const current = rows.find(({ t }) => t.id === selected);
  async function refresh() {
    setCache(await api("juiced/cache"));
    await refreshThreads();
  }
  async function retain(t: Thread) {
    try {
      const full = await api<Thread>(
        "juiced/thread?id=" + encodeURIComponent(t.id),
      );
      await api("juiced/thread", { ...full, retain_cache: !full.retain_cache });
      await refresh();
      notify(
        full.retain_cache
          ? "Snapshot protection released."
          : "Compatible saved state will be retained within the disk budget.",
      );
    } catch (e) {
      notify((e as Error).message);
    }
  }
  async function forget(t: Thread) {
    setBusy(true);
    try {
      await api("juiced/cache/forget", { thread: t.id });
      await refresh();
      notify("Saved computation removed. Conversation messages are preserved.");
    } catch (e) {
      notify((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  async function save() {
    setBusy(true);
    try {
      const result = await api("juiced/cache", draft);
      setCache(result);
      setDraft(result.settings);
      setDirty(false);
      notify("Cache settings saved.");
    } catch (e) {
      notify((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  function edit(key: string, value: unknown) {
    setDraft((d: any) => ({ ...d, [key]: value }));
    setDirty(true);
  }
  return (
    <div className="cache-workspace">
      <header className="page-heading">
        <div>
          <h1>Thread cache</h1>
          <p>Keep useful computation for the conversations you come back to.</p>
        </div>
        <Button size="compact" variant="outline" onClick={() => void refresh()}>
          Refresh
        </Button>
      </header>
      <div className="cache-layout">
        <section className="cache-inventory">
          <div className="storage-overview">
            <div className="storage-title">
              <span className="storage-icon">
                <Database size={24} />
              </span>
              <div>
                <h2>
                  {bytes(total)} <span>saved computation</span>
                </h2>
                <p>
                  {rows.filter((r) => r.saved).length} conversations with saved
                  state · {cache?.settings?.disk_budget_gib || 0} GiB budget
                </p>
              </div>
            </div>
            <progress
              aria-label="Snapshot storage used"
              max={budget || 1}
              value={Math.min(total, budget)}
            />
            <div className="storage-caption">
              <span>Model weights and messages use separate storage.</span>
              <span>
                {budget ? Math.round((100 * total) / budget) : 0}% used
              </span>
            </div>
          </div>
          <div className="cache-filters" aria-label="Thread cache filters">
            {[
              ["all", "All threads"],
              ["saved", "Saved"],
              ["retained", "Retained"],
              ["history", "History only"],
            ].map(([id, label]) => (
              <button
                key={id}
                aria-pressed={filter === id}
                onClick={() => setFilter(id)}
              >
                {label}
                <span>
                  {
                    rows.filter(
                      (r) =>
                        id === "all" ||
                        (id === "saved" && r.saved) ||
                        (id === "retained" && r.t.retain_cache) ||
                        (id === "history" && !r.saved),
                    ).length
                  }
                </span>
              </button>
            ))}
          </div>
          <label className="search-input cache-search">
            <Search size={15} />
            <input
              aria-label="Search cached threads"
              placeholder="Find a conversation or model…"
              value={q}
              onChange={(e) => setQ(e.target.value)}
            />
          </label>
          <div className="cache-table-heading">
            <span>Conversation</span>
            <span>Saved state</span>
            <span>Last activity</span>
          </div>
          <div className="cache-thread-list">
            {visible.map(({ t, saved }) => (
              <button
                key={t.id}
                className={
                  "cache-thread " + (selected === t.id ? "selected" : "")
                }
                onClick={() => setSelected(t.id)}
              >
                <span className="cache-thread-name">
                  <strong>{t.title}</strong>
                  <small title={t.model}>
                    {t.model || "No model selected"}
                  </small>
                </span>
                <span className="cache-thread-state">
                  {t.retain_cache && <Pin size={12} />}
                  {saved ? bytes(saved.bytes) : "History only"}
                  <small>
                    {saved
                      ? `${(saved.tokens || 0).toLocaleString()} tokens`
                      : "Rebuild when reopened"}
                  </small>
                </span>
                <span className="cache-thread-age">
                  {age(t.updated)}
                  <ChevronRight size={14} />
                </span>
              </button>
            ))}
            {!visible.length && (
              <div className="workspace-empty">
                <History size={25} />
                <h3>
                  {q || filter !== "all"
                    ? "No conversations match"
                    : "Your conversations will appear here"}
                </h3>
                <p>
                  Start a chat in Playground. Messages are kept even without a
                  disk snapshot.
                </p>
              </div>
            )}
          </div>
          {current && (
            <Dialog.Root
              open
              onOpenChange={(open) => {
                if (!open) setSelected(undefined);
              }}
            >
              <Dialog.Portal>
                <Dialog.Backdrop className="dialog-backdrop" />
                <Dialog.Popup className="model-drawer cache-thread-drawer">
                  <section className="cache-selection">
                    <div className="section-heading">
                      <Dialog.Title>{current.t.title}</Dialog.Title>
                      <Button
                        variant="ghost"
                        size="compact"
                        onClick={() => setSelected(undefined)}
                      >
                        Close
                      </Button>
                    </div>
                    <p>
                      {current.saved
                        ? `Saved state can be reused with its unchanged model profile. Last cache use: ${age(current.saved.last_used)}.`
                        : "The messages are saved. Reopening this conversation may require the model to process them again."}
                    </p>
                    <div className="cache-selection-actions">
                      <Button
                        size="compact"
                        onClick={() => openThread(current.t.id)}
                      >
                        Open conversation
                        <ArrowUpRight size={13} />
                      </Button>
                      <Button
                        size="compact"
                        variant="outline"
                        onClick={() => void retain(current.t)}
                      >
                        <Pin size={13} />
                        {current.t.retain_cache
                          ? "Release protection"
                          : "Retain cache"}
                      </Button>
                      <Button
                        size="compact"
                        variant="ghost"
                        disabled={!current.saved || busy}
                        onClick={() => void forget(current.t)}
                      >
                        <Trash2 size={13} />
                        Forget saved computation
                      </Button>
                    </div>
                    <small>
                      Forget preserves messages and downloaded model files.
                    </small>
                  </section>
                </Dialog.Popup>
              </Dialog.Portal>
            </Dialog.Root>
          )}
        </section>
        <aside className="cache-settings">
          <div className="section-heading">
            <h2>
              <Settings2 size={16} />
              Storage policy
            </h2>
          </div>
          {draft ? (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void save();
              }}
            >
              <label>
                Reuse mode
                <select
                  value={draft.mode}
                  onChange={(e) => edit("mode", e.target.value)}
                >
                  <option value="memory">Memory only</option>
                  <option value="off">Off</option>
                  <option
                    value="persistent"
                    disabled={!cache?.persistent_supported}
                  >
                    Persistent disk snapshots
                  </option>
                </select>
              </label>
              <p>
                {draft.mode === "persistent"
                  ? "Reuse qualified thread state after a model or server restart."
                  : draft.mode === "memory"
                    ? "Reuse compatible state while the engine stays loaded."
                    : "Playground requests do not reuse prompt state."}
              </p>
              <label>
                Expire after
                <input
                  type="number"
                  min={1}
                  max={365}
                  required
                  value={draft.retention_days}
                  onChange={(e) =>
                    edit("retention_days", Number(e.target.value))
                  }
                />
                <small>
                  Days since last use. Retained threads are protected.
                </small>
              </label>
              <label>
                Disk budget
                <input
                  type="number"
                  min={1}
                  max={256}
                  required
                  value={draft.disk_budget_gib}
                  onChange={(e) =>
                    edit("disk_budget_gib", Number(e.target.value))
                  }
                />
                <small>
                  GiB for snapshots. Temporary saves need additional free space.
                </small>
              </label>
              {dirty && (
                <Button
                  type="button"
                  variant="ghost"
                  onClick={() => {
                    setDraft(cache.settings);
                    setDirty(false);
                  }}
                >
                  Cancel
                </Button>
              )}
              <Button type="submit" disabled={!dirty || busy}>
                {busy ? "Saving…" : "Save changes"}
              </Button>
              {dirty && <small className="unsaved-note">Unsaved changes</small>}
            </form>
          ) : (
            <p>Loading cache settings…</p>
          )}
          <div className="cache-explainer">
            <Clock3 size={17} />
            <h3>What gets reused?</h3>
            <p>
              Snapshots belong to an exact model, engine and load profile. A
              changed profile or edited prefix can require a fresh prefill.
            </p>
            <p>
              Saving a thread keeps its messages. Retaining its cache protects
              the saved computation from ordinary expiry.
            </p>
            <details>
              <summary>Compatibility details</summary>
              <p>{cache?.reason}</p>
            </details>
          </div>
          {cache?.settings?.last_error && (
            <p className="inline-error">
              Last save: {cache.settings.last_error}
            </p>
          )}
        </aside>
      </div>
    </div>
  );
}
