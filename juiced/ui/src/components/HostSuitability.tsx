import { useEffect, useRef, useState } from "react";
import { api, bytes, type Model } from "../api";
import { Button } from "./t3/button";
import "./HostSuitability.css";

type Pool = {
  required_peak_bytes: number;
  observed_available_bytes: number | null;
  capacity_bytes: number | null;
  reserved_bytes: number;
  protected_headroom_bytes?: number;
  source: string;
};
type Assessment = {
  model: string;
  enabled: boolean;
  context_tokens: number | null;
  compatibility: { compatible: boolean | null; reason: string };
  resources: {
    schema_version: number;
    fit: "fits" | "limited_fit" | "does_not_fit" | "unknown";
    reason: string;
    estimate_source?: string;
    observed_at_ms: number;
    admission_open: boolean;
    pools: Record<string, Pool>;
  };
};
const fitNames = {
  fits: "Fits declared limits",
  limited_fit: "Current capacity is in use",
  does_not_fit: "Exceeds declared host limits",
  unknown: "Host fit is unknown",
};
export function HostSuitability({ model, dirty, revision }: { model: Model; dirty: boolean; revision: number }) {
  const [assessment, setAssessment] = useState<Assessment>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const request = useRef<AbortController | null>(null);
  useEffect(() => {
    request.current?.abort();
    setAssessment(undefined); setError(""); setBusy(false);
    return () => request.current?.abort();
  }, [model.id, model.policy, dirty, revision, model.runtime?.pid, model.runtime?.worker_generation]);
  async function check() {
    request.current?.abort();
    const controller = new AbortController(); request.current = controller;
    setBusy(true); setError(""); setAssessment(undefined);
    try {
      const result = await api<Assessment>(`juiced/suitability?model=${encodeURIComponent(model.id)}`, undefined, controller.signal);
      if (result.model !== model.id || result.resources?.schema_version !== 1)
        throw Error("The host returned an incompatible assessment. Refresh the model and check again.");
      if (!controller.signal.aborted) setAssessment(result);
    } catch (error) {
      if (!controller.signal.aborted) setError((error as Error).message);
    } finally {
      if (!controller.signal.aborted) setBusy(false);
    }
  }
  return <section className="host-suitability" aria-label="Host fit">
    <header><div><h3>Host fit</h3><p>{dirty ? "Save your changes before checking these settings." : "Check saved settings against the host's declared memory limits and current capacity."}</p></div>
      <Button size="sm" variant="outline" disabled={dirty || busy} onClick={() => void check()}>{busy ? "Checking…" : error ? "Retry host check" : "Check saved settings"}</Button></header>
    {error && <p className="inline-error" role="alert">Host check unavailable: {error}</p>}
    {busy && <p role="status">Reading capacity without loading the model…</p>}
    {assessment && <div className="host-fit-result" role="status">
      <strong>{fitNames[assessment.resources.fit]}</strong>
      <p>{assessment.resources.reason}</p>
      {assessment.compatibility.compatible === false && <p>Runtime requirement: {assessment.compatibility.reason}</p>}
      {!assessment.enabled && <p>The loading policy is Disabled. This check does not enable the model.</p>}
      {!assessment.resources.admission_open && <p>Admission is paused or changing. Wait for maintenance to finish, then check again.</p>}
      <p className="host-fit-basis">{assessment.context_tokens ? `${assessment.context_tokens.toLocaleString()} tokens per request. ` : ""}A capacity observation reserves no memory and does not qualify model quality or hardware support.</p>
      {!!Object.keys(assessment.resources.pools).length && <details><summary>Memory details</summary>
        <div className="host-fit-pools">{Object.entries(assessment.resources.pools).map(([name, pool]) => <dl key={name}>
          <dt>Physical pool</dt><dd>{name}</dd>
          <dt>Declared peak</dt><dd>{bytes(pool.required_peak_bytes)}</dd>
          <dt>Observed available</dt><dd>{pool.observed_available_bytes === null ? "Unknown" : bytes(pool.observed_available_bytes)}</dd>
          <dt>Pool ceiling</dt><dd>{pool.capacity_bytes === null ? "Unknown" : bytes(pool.capacity_bytes)}</dd>
          <dt>Existing reservations</dt><dd>{bytes(pool.reserved_bytes)}</dd>
          <dt>Essential service reserve</dt><dd>{pool.protected_headroom_bytes === undefined ? "Not reported" : bytes(pool.protected_headroom_bytes)}</dd>
          <dt>Capacity source</dt><dd>{pool.source}</dd>
        </dl>)}</div>
        <p>Estimate source: {assessment.resources.estimate_source}. The peak includes a new worker's load or resident weights plus one request and saved-state operation. Existing workers retain their reservations.</p>
      </details>}
      <small>Checked {new Date(assessment.resources.observed_at_ms).toLocaleTimeString()}. Check again before loading if capacity changes.</small>
    </div>}
  </section>;
}
