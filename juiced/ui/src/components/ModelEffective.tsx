import { useEffect, useState } from "react";
import { RefreshCw } from "lucide-react";
import { api, type Model, type ContextObservation } from "../api";
import type { OptionResponse, Options } from "./modelSettings";
import { effectiveRows, settingValue, observedCapacity } from "./effectiveSettings";
import { Button } from "./t3/button";
import { HelpTip } from "./HelpTip";
import "./ModelEffective.css";

type Worker = { model_name: string; recipe_options?: Options; launch_command?: string[]; context_observation?: ContextObservation; cleanup_pending?: boolean };
export function ModelEffective({ model, options, draft, extra }: { model: Model; options: OptionResponse; draft: Options; extra?: string }) {
  const [worker, setWorker] = useState<Worker>();
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError("");
    void api<{ all_models_loaded: Worker[] }>("health", undefined, controller.signal)
      .then((result) => { if (!controller.signal.aborted) setWorker(result.all_models_loaded?.find((item) => item.model_name === model.id)); })
      .catch((error) => { if (!controller.signal.aborted) { setWorker(undefined); setError(error.message); } })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [model.id, model.runtime?.pid, model.runtime?.worker_generation, revision]);
  const capacity = model.recipe === "llamacpp" && !worker?.cleanup_pending ? observedCapacity(worker?.context_observation) : null;
  let draftError = "";
  let rows;
  try { rows = effectiveRows(options, draft, worker?.recipe_options, extra); }
  catch (error) {
    draftError = `Correct the additional backend arguments before comparing them: ${(error as Error).message}`;
    rows = effectiveRows(options, draft, worker?.recipe_options);
  }
  return <section className="effective-settings" aria-label="Effective model settings">
    <header><div><h3>Saved, running and draft</h3><p>Compare persistent intent with the worker serving every connected client.</p></div>
      <Button variant="ghost" size="sm" onClick={() => setRevision(revision + 1)} disabled={loading}><RefreshCw size={14} /> Refresh</Button></header>
    {error && <p className="inline-error" role="alert">Running settings unavailable: {error}</p>}
    {draftError && <p className="inline-error" role="alert">{draftError}</p>}
    {loading ? <p role="status">Reading the running worker…</p> : !worker ? <p className="settings-message">This model is unloaded. Running values become available after it loads.</p> : null}
    {model.capability_contract && <div className="model-capability-summary" aria-label="Declared model services">
      <strong>Model services</strong><span>{model.capability_contract.operations.map(operation => operation.id).join(", ") || "No declared runtime service"}</span>
      <HelpTip label="Model capability evidence">
        <p>These operations come from the native runtime descriptor and model metadata. A declaration does not establish tested hardware compatibility or output quality.</p>
        <p>{model.capability_contract.execution.compatibility.reason}</p>
        {model.capability_contract.artifact.missing_components.length > 0 && <p>Missing components: {model.capability_contract.artifact.missing_components.join(", ")}.</p>}
        <p>Inspect Runtime for compatible profiles. Qualification remains unknown until a matching test is recorded.</p>
      </HelpTip>
    </div>}
    {worker && <div className="capacity-summary"><strong>{capacity ? `${capacity.perSlot.toLocaleString()} tokens per chat` : "Serving context not reported"}</strong><span>{capacity ? `${capacity.slots ? `${capacity.slots} slot${capacity.slots === 1 ? "" : "s"} reported. ` : ""}Observed from the engine at load. Includes input, reasoning and output.` : "The worker settings below are requested configuration; actual serving limits are unknown."}</span></div>}
    <div className="effective-table-scroll" tabIndex={0} aria-label="Settings comparison table">
      <table><thead><tr><th>Setting / source</th><th>Saved</th><th>Running<HelpTip label="Running setting source">These are the settings passed to the worker. The engine may cap the context; its observed serving limit is shown separately above.</HelpTip></th><th>Draft</th></tr></thead>
        <tbody>{rows.map((row) => <tr key={row.key} className={row.changed ? "changed-setting" : ""}>
          <th scope="row">{row.key.replaceAll("_", " ")}<small>{row.source}</small></th>
          <td>{settingValue(row.saved)}</td><td>{loading ? "Reading…" : settingValue(row.running)}</td><td>{settingValue(row.draft)}{row.changed && <small>Edited</small>}</td>
        </tr>)}</tbody></table>
    </div>
    <section className="running-command"><h3>Actual worker command</h3><p>This is the current process command. Saved or draft changes appear here only after a reload.</p>
      {worker?.launch_command?.length ? <pre tabIndex={0}>{worker.launch_command.map((part) => /^[\w./:=,+-]+$/.test(part) ? part : "'" + part.replaceAll("'", "'\\''") + "'").join(" ")}</pre> : <p>{loading ? "Reading command…" : "No running command is available."}</p>}
    </section>
  </section>;
}
