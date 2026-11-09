import { useEffect, useState } from "react";
import {
  Database,
  Search,
  Pin,
  Trash2,
  ChevronRight,
} from "lucide-react";
import { Dialog } from "@base-ui/react/dialog";
import { api, bytes, type Model } from "../api";
import { HelpTip } from "./HelpTip";
import { Button } from "./t3/button";
import "./CacheWorkspace.css";
import { PreparedCopies } from "./PreparedCopies";
import { DraftLeaveDialog } from "./DraftLeaveDialog";
function age(value?: number) {
  if (!value) return "Not used yet";
  const days = Math.floor((Date.now() / 1000 - value) / 86400);
  return days > 1 ? `${days} days ago` : days === 1 ? "Yesterday" : "Today";
}
export function CacheWorkspace({
  cache,
  models,
  setCache,
  notify,
  registerNavigationGuard,
}: {
  cache: any;
  models: Model[];
  setCache: (value: any) => void;
  notify: (s: string) => void;
  registerNavigationGuard: (guard?: (action: () => void) => void) => void;
}) {
  const [tab, setTab] = useState("snapshots");
  const [q, setQ] = useState(""),
    [filter, setFilter] = useState("all"),
    [selected, setSelected] = useState<string>(),
    [draft, setDraft] = useState<any>(),
    [dirty, setDirty] = useState(false),
    [validationError, setValidationError] = useState(""),
    [busy, setBusy] = useState(false);
  const [overrideModel, setOverrideModel] = useState("");
  const [overrideMode, setOverrideMode] = useState("memory");
  const [leaveAction, setLeaveAction] = useState<(() => void) | null>(null);
  useEffect(() => {
    registerNavigationGuard(action => {
      if (busy) return;
      if (dirty) setLeaveAction(() => action);
      else action();
    });
    return () => registerNavigationGuard(undefined);
  }, [dirty, busy, registerNavigationGuard]);
  useEffect(() => {
    if (!dirty) return;
    const warn = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [dirty]);
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
  const overrideEntries = Object.entries(draft?.model_modes || {}) as [string, string][];
  const availableOverrideModels = models.filter(model =>
    model.downloaded && model.recipe === "llamacpp" && model.service === "Chat" &&
    !Object.hasOwn(draft?.model_modes || {}, model.id)
  ).sort((a, b) => a.id.localeCompare(b.id));
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
    for (const [key, label, maximum, input] of [
      ["retention_days", "Expiry", 365, "cache-expiry-days"],
      ["disk_budget_gib", "Disk budget", 256, "cache-disk-budget"],
    ] as const) {
      const whole = key === "retention_days";
      if (!Number.isFinite(draft[key]) || (whole && !Number.isInteger(draft[key])) || draft[key] < 1 || draft[key] > maximum) {
        setValidationError(`${label} must be ${whole ? "a whole number" : "a number"} from 1 to ${maximum}.`);
        setTab("settings");
        requestAnimationFrame(() => document.getElementById(input)?.focus());
        return;
      }
    }
    setValidationError("");
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
    setValidationError("");
    setDraft((d: any) => ({ ...d, [key]: value }));
    setDirty(true);
  }
  return (
    <div className="cache-workspace">
      <DraftLeaveDialog open={!!leaveAction} title="Discard unsaved cache settings?"
        description="The saved cache policy has not changed. Keep editing, or discard this draft and continue."
        onKeepEditing={() => setLeaveAction(null)}
        onDiscard={() => { const action = leaveAction; setLeaveAction(null); setDraft(cache.settings); setDirty(false); setValidationError(""); action?.(); }} />
      <header className="page-heading">
        <div>
          <div className="heading-with-help"><h1>Runtime cache</h1><HelpTip label="Runtime cache">Cache stores reusable computation for client sessions. Clients own conversation history. Reuse requires the same model, engine and compatible load profile; changing them can require a fresh prefill. {cache?.reason}</HelpTip></div>
        </div>
        <Button size="compact" variant="outline" onClick={() => void refresh()}>
          Refresh
        </Button>
      </header>
      <nav className="workspace-tabs" aria-label="Cache views">
        {[["snapshots", "Snapshots"], ["settings", "Storage policy"], ["overrides", "Model overrides"], ["prepared", "Retained weights"]].map(([id, label]) => <button key={id} className={tab === id ? "active" : ""} aria-current={tab === id ? "page" : undefined} onClick={() => setTab(id)}>{label}{dirty && id !== "snapshots" ? " *" : ""}</button>)}
      </nav>
      {tab === "prepared" && <PreparedCopies />}
      <div className="cache-layout" hidden={tab === "prepared"}>
        <section className="cache-inventory" hidden={tab !== "snapshots"}>
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
                  {snapshots.length} snapshot{snapshots.length === 1 ? "" : "s"} ·{" "}
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
              <span>Snapshot storage</span>
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
                <Dialog.Popup className="cache-snapshot-dialog">
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
        <section className="cache-settings" hidden={tab === "snapshots"}>
          {draft ? (
            <form
              noValidate
              onSubmit={(e) => {
                e.preventDefault();
                void save();
              }}
            >
              <div className="cache-policy-fields" hidden={tab !== "settings"}>
              <label htmlFor="cache-reuse-mode">
                <span className="field-label">Reuse mode <HelpTip label="Reuse mode">Memory reuse lasts while the engine stays loaded. Persistent snapshots can survive model or server restarts on qualified profiles. Off bypasses prompt reuse.</HelpTip></span>
                <select
                  id="cache-reuse-mode"
                  aria-label="Reuse mode"
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
              </div>
              <div className="cache-model-overrides" hidden={tab !== "overrides"}>
                <div className="field-label">Model overrides <HelpTip label="Model overrides">Override the default reuse mode per model. Memory only permits Ollama and other protocols without changing persistent caching for other models. Existing snapshots are kept.</HelpTip></div>
                {overrideEntries.length === 0 && <p className="cache-overrides-empty">No model overrides. All models use the default reuse mode.</p>}
                {overrideEntries.sort(([a], [b]) => a.localeCompare(b)).map(([id, mode]) => (
                  <div className="cache-override-row" key={id}>
                    <label>
                      <span>{id}</span>
                      <select aria-label={`Cache mode for ${id}`} value={mode} disabled={busy}
                        onChange={e => edit("model_modes", { ...draft.model_modes, [id]: e.target.value })}>
                        <option value="memory">Memory only</option>
                        <option value="persistent" disabled={!cache?.persistent_supported}>Persistent disk snapshots</option>
                      </select>
                    </label>
                    <Button type="button" variant="ghost" disabled={busy} aria-label={`Use default for ${id}`}
                      onClick={() => {
                        const modes = { ...draft.model_modes };
                        delete modes[id];
                        edit("model_modes", modes);
                        requestAnimationFrame(() => document.getElementById("cache-override-model")?.focus());
                      }}>Use default</Button>
                  </div>
                ))}
                <div className="cache-override-add">
                  <label htmlFor="cache-override-model">Model
                    <select id="cache-override-model" aria-label="Model" value={overrideModel} disabled={busy || !availableOverrideModels.length}
                      onChange={e => setOverrideModel(e.target.value)}>
                      <option value="">{availableOverrideModels.length ? "Choose a model…" : "No more eligible models"}</option>
                      {availableOverrideModels.map(model => <option key={model.id} value={model.id}>{model.id}</option>)}
                    </select>
                  </label>
                  <label htmlFor="cache-override-mode">Reuse mode
                    <select id="cache-override-mode" aria-label="Reuse mode" value={overrideMode} disabled={busy}
                      onChange={e => setOverrideMode(e.target.value)}>
                      <option value="memory">Memory only</option>
                      <option value="persistent" disabled={!cache?.persistent_supported}>Persistent disk snapshots</option>
                    </select>
                  </label>
                  <Button type="button" variant="outline" disabled={busy || !availableOverrideModels.some(model => model.id === overrideModel)}
                    onClick={() => {
                      edit("model_modes", { ...draft.model_modes, [overrideModel]: overrideMode });
                      setOverrideModel("");
                    }}>Add override</Button>
                </div>
              </div>
              <div className="cache-policy-fields" hidden={tab !== "settings"}>
              <label htmlFor="cache-expiry-days">
                <span className="field-label">Expiry (days) <HelpTip label="Cache expiry">Days since last use. Retained snapshots are protected.</HelpTip></span>
                <input
                  type="number"
                  min={1}
                  max={365}
                  required
                  id="cache-expiry-days"
                  aria-label="Expiry (days)"
                  value={draft.retention_days}
                  onChange={(e) =>
                    edit("retention_days", Number(e.target.value))
                  }
                />

              </label>
              <label htmlFor="cache-disk-budget">
                <span className="field-label">Disk budget (GiB) <HelpTip label="Disk budget">Budget for snapshots. Temporary saves need additional free space.</HelpTip></span>
                <input
                  type="number"
                  min={1}
                  max={256}
                  required
                  id="cache-disk-budget"
                  step="any"
                  aria-label="Disk budget (GiB)"
                  value={draft.disk_budget_gib}
                  onChange={(e) =>
                    edit("disk_budget_gib", Number(e.target.value))
                  }
                />
              </label>
              </div>
              {validationError && <p role="alert" className="inline-error">{validationError}</p>}
              <div className="cache-save-actions">
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
              </div>
            </form>
          ) : (
            <p>Loading cache settings…</p>
          )}
          {cache?.settings?.last_error && (
            <p className="inline-error">
              Last save: {cache.settings.last_error}
            </p>
          )}
        </section>
      </div>
    </div>
  );
}
