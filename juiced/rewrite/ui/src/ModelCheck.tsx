import { useEffect, useState } from "react";
import { ApiError, type Client } from "./api";
import type { Model } from "./domain";
import { notify } from "./toast";
import { Badge, Button, HelpTip } from "./ui";

export type CheckStep = { id: string; label: string; status: "pass" | "fail" | "skip" | "info"; detail: string; ms?: number };
export type CheckResult = { model: string; checked_at_ms: number; passed: boolean; current: boolean; steps: CheckStep[]; error?: string };

const record = (value: unknown): Record<string, unknown> => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};

export function parseCheck(value: unknown): CheckResult | null {
  const body = record(value);
  if (typeof body.model !== "string" || typeof body.passed !== "boolean" || !Array.isArray(body.steps)) return null;
  const steps = body.steps.flatMap((item): CheckStep[] => {
    const step = record(item);
    if (typeof step.id !== "string" || typeof step.label !== "string" || !["pass", "fail", "skip", "info"].includes(step.status as string) || typeof step.detail !== "string") return [];
    return [{ id: step.id, label: step.label, status: step.status as CheckStep["status"], detail: step.detail, ms: typeof step.ms === "number" ? step.ms : undefined }];
  });
  return { model: body.model, checked_at_ms: typeof body.checked_at_ms === "number" ? body.checked_at_ms : 0, passed: body.passed, current: body.current === true, steps, error: typeof body.error === "string" ? body.error : undefined };
}

export function ago(ms: number, now = Date.now()): string {
  const minutes = Math.max(0, Math.round((now - ms) / 60000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  return hours < 24 ? `${hours} h ago` : `${Math.round(hours / 24)} d ago`;
}

const marks: Record<CheckStep["status"], string> = { pass: "✓", fail: "✗", skip: "–", info: "" };

/** The last model check and a button to run one. */
export function ModelCheck({ model, client, online, disabled }: { model: Model; client: Client; online: boolean; disabled: boolean }) {
  const [result, setResult] = useState<CheckResult | null>(null);
  const [running, setRunning] = useState(false);
  const path = `models/${encodeURIComponent(model.id)}/check`;
  useEffect(() => {
    let cancelled = false;
    client.request<unknown>(path).then((body) => { if (!cancelled) setResult(parseCheck(body)); }).catch(() => { if (!cancelled) setResult(null); });
    return () => { cancelled = true; };
  }, [client, path, model.state, model.revision]);
  async function run() {
    setRunning(true);
    try {
      const checked = parseCheck(await client.request<unknown>(path, {}));
      setResult(checked);
      if (checked) notify(checked.passed ? `${model.id} passed its check.` : `${model.id} failed part of its check.`, checked.passed ? "success" : "error");
    } catch (error) {
      notify(error instanceof ApiError || error instanceof Error ? error.message : "The check failed.", "error");
    } finally { setRunning(false); }
  }
  if (model.policy === "disabled" && !result) return null;
  return <section className="inspector-section model-check" aria-label="Model check">
    <div className="section-heading"><div className="page-title"><h3>Check</h3><HelpTip label="Model check">A few short real requests: chat, tool calls, the thinking switch and image input where the model declares them, plus decode speed. Juiced runs it after a chat model starts with new settings or a new engine.</HelpTip></div>
      {result && <Badge tone={result.passed ? "good" : "bad"}>{result.passed ? "Passed" : "Failed"}</Badge>}</div>
    {result?.error && <p className="inline-error">{result.error}</p>}
    {result && result.steps.length > 0 && <ul className="check-steps">{result.steps.map((step) => <li key={step.id} className={`check-${step.status}`}><span className="check-mark" aria-hidden="true">{marks[step.status]}</span><span>{step.label}</span><span className="muted">{step.detail}</span></li>)}</ul>}
    {result && <p className="field-help">Checked {ago(result.checked_at_ms)}{result.current ? "" : " · from an earlier launch"}</p>}
    {!result && <p className="field-help">Not checked since the service started.</p>}
    <div className="action-row"><Button disabled={disabled || running || !online || model.policy === "disabled"} onClick={() => void run()}>{running ? "Checking…" : model.loaded ? "Run check" : "Load and check"}</Button></div>
  </section>;
}
