import { useEffect, useState } from "react";
import {
  Database,
  Search,
  Pin,
  Settings2,
  Clock3,
  Trash2,
  ChevronRight,
} from "lucide-react";
import { Dialog } from "@base-ui/react/dialog";
import { api, bytes } from "../api";
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
  notify,
}: {
  cache: any;
  setCache: (value: any) => void;
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
  const visible = snapshots.filter(
    (s) =>
      [s.session || s.thread, s.model, s.profile]
        .join(" ")
        .toLowerCase()
        .includes(q.toLowerCase()) &&
      (filter === "all" || s.retained),
  );
  const current = snapshots.find((s) => s.id === selected);
  async function refresh() {
    setCache(await api("juiced/cache"));
  }
  async function control(s: any, action: "retain" | "forget") {
    setBusy(true);
    try {
      await api("juiced/cache/" + action, {
        snapshot: s.id,
        ...(action === "retain" ? { retained: !s.retained } : {}),
      });
      if (action === "forget") setSelected(undefined);
      await refresh();
      notify(
        action === "forget"
          ? "Saved computation removed. Client history is managed by its owner."
          : "Snapshot retention updated.",
      );
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
          <h1>Runtime cache</h1>
          <p>Manage reusable computation for downstream consumer sessions.</p>
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
                  {snapshots.length} snapshots ·{" "}
                  {cache?.settings?.disk_budget_gib || 0} GiB budget
                </p>
              </div>
            </div>
            <progress
              aria-label="Snapshot storage used"
              max={budget || 1}
              value={Math.min(total, budget)}
            />
            <div className="storage-caption">
              <span>
                Model weights use separate storage. Clients own conversation
                history.
              </span>
              <span>
                {budget ? Math.round((100 * total) / budget) : 0}% used
              </span>
            </div>
          </div>
          <div className="cache-filters" aria-label="Snapshot filters">
            <button
              aria-pressed={filter === "all"}
              onClick={() => setFilter("all")}
            >
              All snapshots <span>{snapshots.length}</span>
            </button>
            <button
              aria-pressed={filter === "retained"}
              onClick={() => setFilter("retained")}
            >
              Retained <span>{snapshots.filter((s) => s.retained).length}</span>
            </button>
          </div>
          <label className="search-input cache-search">
            <Search size={15} />
            <input
              aria-label="Search cached sessions"
              placeholder="Find a consumer session or model…"
              value={q}
              onChange={(e) => setQ(e.target.value)}
            />
          </label>
          <div className="cache-table-heading">
            <span>Session / model</span>
            <span>Saved state</span>
            <span>Last used</span>
          </div>
          <div className="cache-entry-list">
            {visible.map((s) => (
              <button
                key={s.id}
                className={
                  "cache-entry " + (selected === s.id ? "selected" : "")
                }
                onClick={() => setSelected(s.id)}
              >
                <span className="cache-entry-name">
                  <strong>{s.session || s.thread}</strong>
                  <small title={s.model}>{s.model}</small>
                </span>
                <span className="cache-entry-state">
                  {s.retained && <Pin size={12} />} {bytes(s.bytes)}
                  <small>{(s.tokens || 0).toLocaleString()} tokens</small>
                </span>
                <span className="cache-entry-age">
                  {age(s.last_used)}
                  <ChevronRight size={14} />
                </span>
              </button>
            ))}
            {!visible.length && (
              <div className="workspace-empty">
                <Database size={25} />
                <h3>
                  {q || filter !== "all"
                    ? "No snapshots match"
                    : "No saved session state"}
                </h3>
                <p>
                  Compatible clients can reuse computation with a stable session
                  ID. Temporary UI tests do not create disk snapshots.
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
                <Dialog.Popup className="model-drawer cache-entry-drawer">
                  <section className="cache-selection">
                    <div className="section-heading">
                      <Dialog.Title>Session cache</Dialog.Title>
                      <Button
                        variant="ghost"
                        size="compact"
                        onClick={() => setSelected(undefined)}
                      >
                        Close
                      </Button>
                    </div>
                    <Dialog.Description>
                      Reusable computation, independent of the consumer's saved
                      history.
                    </Dialog.Description>
                    <dl className="model-facts">
                      <dt>Session</dt>
                      <dd>{current.session || current.thread}</dd>
                      <dt>Model</dt>
                      <dd>{current.model}</dd>
                      <dt>Size</dt>
                      <dd>{bytes(current.bytes)}</dd>
                      <dt>Tokens</dt>
                      <dd>{(current.tokens || 0).toLocaleString()}</dd>
                      <dt>Profile</dt>
                      <dd>{current.profile}</dd>
                      <dt>Last used</dt>
                      <dd>{age(current.last_used)}</dd>
                    </dl>
                    <div className="cache-selection-actions">
                      <Button
                        size="compact"
                        variant="outline"
                        disabled={busy}
                        onClick={() => void control(current, "retain")}
                      >
                        <Pin size={13} />
                        {current.retained
                          ? "Release protection"
                          : "Retain snapshot"}
                      </Button>
                      <Button
                        size="compact"
                        variant="ghost"
                        disabled={busy}
                        onClick={() => void control(current, "forget")}
                      >
                        <Trash2 size={13} />
                        Forget saved computation
                      </Button>
                    </div>
                    <small>
                      Snapshot changes do not delete model weights or alter the
                      consumer's data.
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
                  ? "Reuse qualified consumer session state after a model or server restart."
                  : draft.mode === "memory"
                    ? "Reuse compatible state while the engine stays loaded."
                    : "Chat Completions requests bypass prompt reuse."}
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
                  Days since last use. Retained snapshots are protected.
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
              Snapshots belong to a consumer session and an exact model, engine
              and load profile. A changed profile or edited prefix can require a
              fresh prefill.
            </p>
            <p>
              Clients own their history. Retaining a snapshot protects its
              computation from ordinary expiry.
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
