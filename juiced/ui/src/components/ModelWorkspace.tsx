import { useEffect, useState } from "react";
import { DraftLeaveDialog } from "./DraftLeaveDialog";
import {
  Download,
  ExternalLink,
  LoaderCircle,
  RefreshCw,
  X,
} from "lucide-react";
import { api, bytes, type Model } from "../api";
import { Button } from "./t3/button";
import { ModelRemoval } from "./ModelRemoval";
import { policyNames } from "./ModelLibrary";
import { modelQuant } from "./modelDiscovery";
import {
  editableOptions,
  optionPatch,
  readArg,
  runtimeFields,
  samplingFields,
  setArg,
  stripArgs,
  validateSettings,
  withExtraArgs,
  type ArgField,
  type OptionResponse,
  type Options,
} from "./modelSettings";
import "./ModelWorkspace.css";
import { HelpTip } from "./HelpTip";
import { ModelEffective } from "./ModelEffective";
import { ModelReadme } from "./ModelReadme";
import { ExecutionProfiles } from "./ExecutionProfiles";
import "./BackendInventory.css";

function repository(model: Model) {
  const checkpoint = model.checkpoints?.main || model.checkpoint || "";
  return /^[^/:]+\/[^:]+:/.test(checkpoint)
    ? checkpoint.slice(0, checkpoint.lastIndexOf(":"))
    : "";
}
function retainedExtra(value: unknown) {
  const args = String(value || "");
  try { return stripArgs(args); } catch { return args; }
}
const optionLabels: Record<string, string> = {
  llamacpp_backend: "Runtime backend",
  llamacpp_device: "Runtime device",
  merge_args: "Include backend defaults",
  auto_evict: "Automatic eviction",
  auto_update: "Automatic updates",
  downsize_idle_timeout: "Downsize after idle seconds",
  evict_idle_timeout: "Evict after idle seconds",
  evict_weight_factor: "Eviction weight",
  width: "Image width",
  height: "Image height",
  steps: "Sampling steps",
  cfg_scale: "Guidance scale",
  flow_shift: "Flow shift",
  sampling_method: "Sampler",
  asr4all_threads: "CPU threads",
  asr4all_commit: "Commit window",
  asr4all_right: "Right context",
};
const runtimeBehaviorOptions = new Set(["auto_evict", "auto_update", "downsize_idle_timeout", "evict_idle_timeout", "evict_weight_factor", "merge_args"]);
const modelOptionOrder = ["width", "height", "steps", "sampling_method", "cfg_scale", "flow_shift", "asr4all_threads", "asr4all_commit", "asr4all_right"];
export function ModelWorkspace({
  model,
  models,
  removalRequested,
  registerNavigationGuard,
  onClose,
  onInspect,
  onBrowse,
  onTest,
  onFindVariant,
  refresh,
  notify,
}: {
  model: Model;
  models: Model[];
  removalRequested: boolean;
  registerNavigationGuard: (guard?: (action: () => void) => void) => void;
  onClose: () => void;
  onInspect: (model: Model) => void;
  onBrowse: (repo: string) => void;
  onTest: (model: Model) => void;
  onFindVariant: (model: Model, runtime: string) => void;
  refresh: () => Promise<void>;
  notify: (message: string) => void;
}) {
  const [tab, setTab] = useState(removalRequested ? "manage" : model.recipe === "llamacpp" ? "settings" : "advanced");
  const [response, setResponse] = useState<OptionResponse>();
  const [draft, setDraft] = useState<Options>({});
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [leaveAction, setLeaveAction] = useState<(() => void) | null>(null);
  const [revision, setRevision] = useState(0);
  const [conflict, setConflict] = useState(false);
  const [message, setMessage] = useState("");
  const [extra, setExtra] = useState("");
  const [extraEdited, setExtraEdited] = useState(false);
  const [copyFrom, setCopyFrom] = useState("");
  const repo = repository(model);
  const related = repo
    ? models.filter((m) => repository(m) === repo && m.downloaded)
    : [];
  const args = String(
    draft.llamacpp_args ?? response?.effective.llamacpp_args ?? "",
  );
  const dirty =
    !!response &&
    (Object.keys(optionPatch(editableOptions(response.saved), draft)).length >
      0 ||
      extraEdited);
  const pending = saving || model.operation?.pending;
  const busy = model.runtime?.is_busy;
  const backendArguments = Object.entries(response?.effective || {}).filter(([key, value]) => key.endsWith("_args") && typeof value === "string");
  const advancedOptions = Object.entries(response?.effective || {}).filter(([key, value]) =>
    !["model_name", ...(model.recipe === "llamacpp" ? ["ctx_size"] : []), "pinned", "llamacpp_device"].includes(key) &&
    !key.startsWith("llamacpp_hybrid_") && !(key.endsWith("_args") && typeof value === "string") && !key.endsWith("_backend"))
    .sort(([left], [right]) => {
      const rank = (key: string) => modelOptionOrder.includes(key) ? modelOptionOrder.indexOf(key) : 100;
      return rank(left) - rank(right);
    });
  const optionSections: [string, [string, any][]][] = model.recipe === "llamacpp"
    ? [["Backend options", advancedOptions]]
    : [["Model settings", advancedOptions.filter(([key]) => !runtimeBehaviorOptions.has(key))],
       ["Runtime behavior", advancedOptions.filter(([key]) => runtimeBehaviorOptions.has(key))]];
  useEffect(() => {
    const controller = new AbortController();
    setError("");
    void api<OptionResponse>(
      `models/${encodeURIComponent(model.id)}/options`,
      undefined,
      controller.signal,
    )
      .then((data) => {
        setResponse(data);
        setDraft(editableOptions(data.saved));
        try {
          setExtra(stripArgs(String(data.effective.llamacpp_args || "")));
        } catch (e) {
          setExtra(String(data.effective.llamacpp_args || ""));
          setError((e as Error).message);
        }
      })
      .catch((e) => {
        if (!controller.signal.aborted) setError(e.message);
      });
    return () => controller.abort();
  }, [model.id, revision]);
  function leave(action: () => void) {
    if (saving) return;
    if (dirty) setLeaveAction(() => action);
    else action();
  }
  useEffect(() => {
    registerNavigationGuard(leave);
    return () => registerNavigationGuard(undefined);
  }, [dirty, saving, registerNavigationGuard]);
  useEffect(() => {
    if (!dirty) return;
    const warn = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [dirty]);
  async function reviewLatest() {
    if (!response) return;
    setSaving(true);
    try {
      const patch = optionPatch(editableOptions(response.saved), draft);
      const latest = await api<OptionResponse>(`models/${encodeURIComponent(model.id)}/options`);
      setResponse(latest);
      const merged = { ...editableOptions(latest.saved), ...patch };
      setDraft(merged);
      if (!extraEdited) setExtra(stripArgs(String(merged.llamacpp_args ?? latest.effective.llamacpp_args ?? "")));
      setConflict(false);
      setError("");
      setMessage("Latest saved settings loaded. Your edits are retained for review before saving.");
      setTab("effective");
    } catch (error) { setError((error as Error).message); }
    finally { setSaving(false); }
  }
  function update(key: string, value: any) {
    setDraft((current) => ({ ...current, [key]: value }));
    setMessage("");
  }
  function argControl(field: ArgField) {
    let value = "";
    try {
      value = readArg(args, field);
    } catch {
      /* The advanced editor retains malformed input for correction. */
    }
    const change = (next: string) => {
      try {
        update("llamacpp_args", setArg(args, field, next));
        setError("");
      } catch (e) {
        setError((e as Error).message);
      }
    };
    return (
      <label key={field.key} htmlFor={`model-setting-${field.key}`}>
        <span className="field-label">{field.label}<HelpTip label={field.label}>{field.help}</HelpTip></span>
        {field.choices ? (
          <select
            id={`model-setting-${field.key}`}
            aria-label={field.label}
            value={value}
            onChange={(e) => change(e.target.value)}
          >
            <option value="">Backend default</option>
            {[...new Set([...field.choices, ...(value ? [value] : [])])].map(
              (v) => (
                <option key={v}>{v}</option>
              ),
            )}
          </select>
        ) : (
          <input
            type="number"
            id={`model-setting-${field.key}`}
            aria-label={field.label}
            disabled={
              field.key === "gpu" &&
              (draft.llamacpp_backend ??
                response?.effective.llamacpp_backend) === "cpu"
            }
            min={field.min}
            step={field.step || 1}
            value={value}
            placeholder="Backend default"
            onChange={(e) => change(e.target.value)}
          />
        )}

      </label>
    );
  }
  async function apply(reload: boolean) {
    if (!response) return;
    setSaving(true);
    setError("");
    setConflict(false);
    setMessage("");
    let saved = false;
    try {
      const next = { ...draft };
      if (extraEdited) next.llamacpp_args = withExtraArgs(args, extra);
      validateSettings(next);
      const patch = optionPatch(editableOptions(response.saved), next);
      if (Object.keys(patch).length) {
        const latest = await api<OptionResponse>(
          `models/${encodeURIComponent(model.id)}/options`,
        );
        if (
          Object.keys(patch).some(
            (key) =>
              JSON.stringify(latest.saved[key]) !==
              JSON.stringify(response.saved[key]),
          )
        )
          {
          setConflict(true);
          throw Error(
            "These settings changed in another client. Review the latest values before saving.",
          );
          }
        const result = await api<OptionResponse>(
          `models/${encodeURIComponent(model.id)}/options`,
          patch,
        );
        saved = true;
        setResponse(result);
        setDraft(editableOptions(result.saved));
        setExtra(stripArgs(String(result.effective.llamacpp_args || "")));
        setExtraEdited(false);
      }
      if (reload)
        await api("juiced/operation", {
          model: model.id,
          action: model.loaded ? "reload" : "load",
        });
      setMessage(
        reload
          ? "Settings saved. The model is restarting with its loading policy unchanged."
          : "Settings saved for the next load. The running model was not restarted.",
      );
      await refresh();
    } catch (e) {
      setError(
        (saved ? "Settings saved, but applying them did not finish: " : "") +
          (e as Error).message,
      );
    } finally {
      setSaving(false);
    }
  }
  async function policy(value: string) {
    setSaving(true);
    setError("");
    try {
      await api("juiced/policy", { model: model.id, policy: value });
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setSaving(false);
    }
  }
  async function copySettings() {
    if (!copyFrom) return;
    setSaving(true);
    setError("");
    try {
      const source = await api<OptionResponse>(
        `models/${encodeURIComponent(copyFrom)}/options`,
      );
      if (source.recipe !== model.recipe)
        throw Error(
          "Settings can only be copied between variants using the same runtime.",
        );
      setDraft(editableOptions(source.effective));
      setExtra(stripArgs(String(source.effective.llamacpp_args || "")));
      setExtraEdited(false);
      setMessage(
        `Settings from ${copyFrom} are staged for review. Save when ready; no running model has changed.`,
      );
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setSaving(false);
    }
  }
  let parallel = 1;
  try {
    parallel = Number(readArg(args, runtimeFields[0])) || 1;
  } catch {
    /* Keep the form usable while arguments are corrected. */
  }
  const context = Number(draft.ctx_size ?? response?.effective.ctx_size ?? -1);
  return (
    <>
      <section className="model-workspace" aria-label="Model details">
          <header className="model-workspace-header">
            <div className="dialog-heading">
              <div>
                <h2>{model.id}</h2>

              </div>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Back to model list"
                disabled={saving}
                onClick={() => leave(onClose)}
              >
                <X size={17} />
              </Button>
            </div>
            <div className="variant-summary">
              <span className="quant-badge">
                {modelQuant(model) || model.recipe}
              </span>
              <span>{model.service}</span>
              <span>
                {model.size ? bytes(model.size * 1e9) : "Size not reported"}
              </span>
              <span className={model.loaded ? "is-ready" : ""}>
                {model.operation?.pending
                  ? model.operation.state
                  : model.loaded
                    ? busy
                      ? "Busy"
                      : "Ready"
                    : "Unloaded"}
              </span>
            </div>
            <div className="model-workspace-actions">
              <label>
                Loading policy
                <select
                  aria-label="Loading policy"
                  value={model.policy}
                  disabled={pending}
                  onChange={(e) => void policy(e.target.value)}
                >
                  {Object.entries(policyNames).map(([key, value]) => (
                    <option key={key} value={key}>
                      {value}
                    </option>
                  ))}
                </select>
              </label>
              <Button
                variant="outline"
                disabled={!model.loaded || pending}
                onClick={() => leave(() => onTest(model))}
              >
                Test this variant
              </Button>
              {repo && (
                <Button
                  variant="ghost"
                  onClick={() => leave(() => onBrowse(repo))}
                >
                  <Download size={14} /> Add another quant
                </Button>
              )}
            </div>
            <nav className="workspace-tabs" aria-label="Model details">
              {[
                [model.recipe === "llamacpp" ? "settings" : "advanced", "Settings"],
                ["runtime", "Runtime"],
                ...(model.recipe === "llamacpp" ? [["advanced", "Advanced"]] : []),
                ["effective", "Effective"],
                ["readme", "README"],
                ["weights", "Weights & variants"],
                ["manage", "Manage"],
              ].map(([key, label]) => (
                <button
                  key={key}
                  aria-current={tab === key ? "page" : undefined}
                  className={tab === key ? "active" : ""}
                  onClick={() => setTab(key)}
                >
                  {label}
                </button>
              ))}
            </nav>
          </header>
          <div className="model-workspace-content">
            {error && (
              <p className="inline-error" role="alert">
                {error}
                {conflict && <Button size="sm" variant="outline" onClick={() => void reviewLatest()}>Review latest settings</Button>}
              </p>
            )}
            {model.operation?.error && (
              <p className="inline-error" role="alert">
                {model.operation.error}
              </p>
            )}
            {message && (
              <p className="settings-message" role="status">
                {message}
              </p>
            )}
            {["settings", "advanced"].includes(tab) &&
              (!response ? (
                <p>
                  {error
                    ? "Settings could not be read. Retry to load them."
                    : "Reading model settings…"}
                  {error && <Button size="sm" variant="outline" onClick={() => setRevision(revision + 1)}>Retry settings</Button>}
                </p>
              ) : (
                <>
                  {busy && <div className="settings-status">
                    <strong>
                      {dirty
                        ? "Unsaved changes"
                        : model.runtime?.settings_pending
                          ? "Saved changes need a reload"
                          : model.loaded
                            ? "Running saved settings"
                            : "Settings for the next load"}
                    </strong>
                    <p>
                      {busy
                        ? "Active requests are running. Save for the next load, or wait before reloading."
                        : "Load-time changes apply to this variant only. Reload keeps its loading policy."}
                    </p>
                  </div>}
                  {tab === "advanced" && related.some(
                    (m) => m.id !== model.id && m.recipe === model.recipe,
                  ) && (
                    <div className="settings-copy">
                      <label>
                        Copy settings from another quant
                        <select
                          value={copyFrom}
                          disabled={saving}
                          onChange={(e) => setCopyFrom(e.target.value)}
                        >
                          <option value="">Choose an installed variant</option>
                          {related
                            .filter(
                              (m) =>
                                m.id !== model.id && m.recipe === model.recipe,
                            )
                            .map((m) => (
                              <option key={m.id} value={m.id}>
                                {modelQuant(m) || m.id} · {m.id}
                              </option>
                            ))}
                        </select>
                      </label>
                      <Button
                        variant="outline"
                        disabled={!copyFrom || saving}
                        onClick={() => void copySettings()}
                      >
                        Copy for review
                      </Button>
                    </div>
                  )}
                  {tab === "settings" && <>
                  <fieldset className="settings-section" disabled={saving}>
                    <legend>Context & capacity</legend>
                    <div className="settings-grid">
                      {"ctx_size" in response.effective && (
                        <label htmlFor="model-context-tokens">
                          <span className="field-label">Total context tokens<HelpTip label="Total context tokens">Includes prompt, reasoning and output. -1 selects automatically. More context and concurrent chats require more memory.</HelpTip></span>
                          <input
                            type="number"
                            id="model-context-tokens"
                            aria-label="Total context tokens"
                            min={-1}
                            step={1}
                            value={
                              draft.ctx_size ??
                              response.effective.ctx_size ??
                              -1
                            }
                            onChange={(e) =>
                              update(
                                "ctx_size",
                                e.target.value === ""
                                  ? null
                                  : Number(e.target.value),
                              )
                            }
                          />

                        </label>
                      )}
                      {model.recipe === "llamacpp" &&
                        argControl(runtimeFields[0])}
                    </div>
                    {model.recipe === "llamacpp" && (
                      <p className="context-estimate">
                        {context > 0
                          ? `${Math.floor(context / parallel).toLocaleString()} tokens per chat across ${parallel} slot${parallel === 1 ? "" : "s"}.`
                          : "Per-chat context is determined when the backend loads."}{" "}

                      </p>
                    )}
                  </fieldset>
                  {model.recipe === "llamacpp" && (
                    <>
                      <fieldset className="settings-section" disabled={saving}>
                        <legend>Performance & memory</legend>
                        <div className="settings-grid">
                          {runtimeFields.slice(1).map(argControl)}
                        </div>
                      </fieldset>
                      <fieldset className="settings-section" disabled={saving}>
                        <legend>Generation defaults <HelpTip label="Generation defaults">Used when callers do not supply sampling settings. Clients can override these per request.</HelpTip></legend>
                        <div className="settings-grid">
                          {samplingFields.map(argControl)}
                        </div>
                      </fieldset>
                    </>
                  )}
                  </>}
                  {tab === "advanced" && <>
                  {optionSections.filter(([, options]) => options.length > 0).map(([title, options]) => (
                  <fieldset key={title} className="settings-section" disabled={saving}>
                    <legend>{title}</legend>
                    <div className="settings-grid">
                      {options.map(([key, value]) => {
                          const current = Object.prototype.hasOwnProperty.call(draft, key) ? draft[key] : value;
                          const optionType = typeof (response.defaults[key] ?? value);
                          const label = optionLabels[key] || key.replace(/_/g, " ");
                          return (
                            <label key={key}>
                              <span className="field-label">{label}<HelpTip label={label}>Default: {value === null ? "Follow server policy" : String(response.defaults[key] ?? "Automatic")}. Clear the override to inherit the default.</HelpTip></span>
                              {optionType === "boolean" || (value === null && response.defaults[key] == null) ? (
                                <select
                                  aria-label={label}
                                  value={
                                    draft[key] === null
                                      ? "inherit"
                                      : typeof current === "boolean"
                                        ? String(current)
                                        : "inherit"
                                  }
                                  onChange={(e) =>
                                    update(
                                      key,
                                      e.target.value === "inherit"
                                        ? null
                                        : e.target.value === "true",
                                    )
                                  }
                                >
                                  <option value="inherit">Use default</option>
                                  <option value="true">Enabled</option>
                                  <option value="false">Disabled</option>
                                </select>
                              ) : (
                                <input
                                  aria-label={label}
                                  type={
                                    optionType === "number"
                                      ? "number"
                                      : "text"
                                  }
                                  value={current ?? ""}
                                  placeholder={String(response.defaults[key] ?? "Backend default")}
                                  onChange={(e) =>
                                    update(
                                      key,
                                      e.target.value === ""
                                        ? null
                                        : optionType === "number"
                                          ? Number(e.target.value)
                                          : e.target.value,
                                    )
                                  }
                                />
                              )}
                            </label>
                          );
                        })}
                    </div>
                  </fieldset>
                  ))}
                  {(model.recipe === "llamacpp" || backendArguments.length > 0) && <section className="settings-section">
                    <h2>Backend arguments <HelpTip label="Backend arguments">Additional flags not covered by the Settings controls. Existing cache, template and backend flags are preserved.</HelpTip></h2>
                    {model.recipe === "llamacpp" ? (
                      <label>
                        Additional llama.cpp arguments
                        <textarea
                          aria-label="Additional llama.cpp arguments"
                          value={extra}
                          rows={5}
                          spellCheck={false}
                          onChange={(e) => {
                            setExtra(e.target.value);
                            setExtraEdited(true);
                            setMessage("");
                          }}
                        />
                      </label>
                    ) : (
                      backendArguments
                        .map(([key, value]) => (
                          <label key={key}>
                            {key.replace(/_/g, " ")}
                            <textarea
                              rows={4}
                              aria-label={key.replace(/_/g, " ")}
                              value={Object.prototype.hasOwnProperty.call(draft, key) ? draft[key] ?? "" : value ?? ""}
                              onChange={(e) => update(key, e.target.value)}
                              spellCheck={false}
                            />
                          </label>
                        ))
                    )}
                  </section>}
                  </>}
                </>
              ))}
            {tab === "runtime" && <ExecutionProfiles model={model} dirty={dirty} refresh={refresh} onChanged={() => setRevision(v => v + 1)} onFindVariant={runtime => leave(() => onFindVariant(model, runtime))} />}
            {tab === "effective" && response && <ModelEffective model={model} options={response} draft={draft} extra={extraEdited ? extra : undefined} />}
            {tab === "readme" && <ModelReadme repository={repo} />}
            {tab === "weights" && (
              <>
                <section className="settings-section">
                  <h2>Source files</h2>
                  <p>
                    Each quant is a separate selectable model with its own
                    settings and loading policy.
                  </p>
                  <dl className="model-facts">
                    {Object.entries(model.checkpoints || {}).map(
                      ([role, path]) => (
                        <div key={role}>
                          <dt>
                            {role === "main"
                              ? "Model weights"
                              : role === "mmproj"
                                ? "Vision projector"
                                : role}
                          </dt>
                          <dd>{path}</dd>
                        </div>
                      ),
                    )}
                  </dl>
                  {repo && (
                    <a
                      href={`https://huggingface.co/${repo}`}
                      target="_blank"
                      rel="noreferrer"
                    >
                      Open publisher repository <ExternalLink size={12} />
                    </a>
                  )}
                </section>
                <section className="settings-section">
                  <h2>Installed variants</h2>
                  {related.length ? (
                    related.map((m) => (
                      <button
                        className={
                          "installed-variant " +
                          (m.id === model.id ? "selected" : "")
                        }
                        key={m.id}
                        onClick={() => leave(() => onInspect(m))}
                      >
                        <strong>{modelQuant(m) || m.id}</strong>
                        <span>{m.id}</span>
                        <small>
                          {m.loaded ? "Loaded" : "Unloaded"} ·{" "}
                          {policyNames[m.policy]}
                        </small>
                      </button>
                    ))
                  ) : (
                    <p>No other variants from this source are installed.</p>
                  )}
                </section>
                <p className="page-note">
                  Changing quant or load settings changes the cache profile.
                  Incompatible saved computation is rebuilt; downloaded weights
                  and other variants are kept.
                </p>
              </>
            )}
            {tab === "manage" && (
              <>
                <ModelRemoval
                  model={model}
                  onRefresh={refresh}
                  focusRequested={removalRequested}
                  notify={notify}
                  onRemoved={async () => {
                    onClose();
                    await refresh();
                  }}
                />
                <details className="raw-details">
                  <summary>Complete catalog record</summary>
                  <pre>{JSON.stringify(model, null, 2)}</pre>
                </details>
              </>
            )}
          </div>
          {["settings", "advanced", "effective"].includes(tab) && response && (
            <footer className="model-workspace-footer">
              <div>
                <strong>
                  {dirty
                    ? "Unsaved changes"
                    : model.runtime?.settings_pending
                      ? "Reload to apply saved changes"
                      : "No unsaved changes"}
                </strong>
                <small>
                  {model.policy === "disabled"
                    ? "Enable On demand or Hot before loading."
                    : model.policy === "hot"
                      ? "Hot policy stays enabled during reload."
                      : "On demand policy stays enabled during reload."}
                </small>
              </div>
              {dirty && (
                <Button
                  variant="ghost"
                  disabled={saving}
                  onClick={() => {
                    setDraft(editableOptions(response.saved));
                    setExtra(
                      retainedExtra(response.effective.llamacpp_args),
                    );
                    setExtraEdited(false);
                    setError("");
                  }}
                >
                  Revert edits
                </Button>
              )}
              <Button
                variant="outline"
                disabled={!dirty || pending}
                onClick={() => void apply(false)}
              >
                Save for next load
              </Button>
              <Button
                disabled={
                  pending ||
                  busy ||
                  model.policy === "disabled" ||
                  !model.downloaded
                }
                onClick={() => void apply(true)}
              >
                {pending ? (
                  <LoaderCircle size={14} className="spin" />
                ) : (
                  <RefreshCw size={14} />
                )}
                {model.loaded
                  ? dirty
                    ? "Save and reload"
                    : model.runtime?.settings_pending
                      ? "Apply saved settings"
                      : "Reload model"
                  : dirty
                    ? "Save and load"
                    : "Load model"}
              </Button>
            </footer>
          )}
      </section>
      <DraftLeaveDialog open={!!leaveAction} title="Discard unsaved model settings?"
        description="Your running model and saved settings have not changed. Keep editing, or discard this draft and continue."
        onKeepEditing={() => setLeaveAction(null)}
        onDiscard={() => { const action = leaveAction; setLeaveAction(null); if (response) { setDraft(editableOptions(response.saved)); setExtra(retainedExtra(response.effective.llamacpp_args)); setExtraEdited(false); } action?.(); }} />
    </>
  );
}
