import type { Client } from "./api";
import type { Model } from "./domain";
import { useState } from "react";
import { Button, HelpTip } from "./ui";
import { useOperation, useResource } from "./useResource";

export type Residency = {
  idle_unload_after_ms: number | null;
  source: "service" | "model" | "keep_alive";
  model_idle_unload_after_ms: number | null;
  keep_alive_ms: number | null;
  last_used_unix_ms: number | null;
  idle_unload_at_unix_ms: number | null;
  last_unload: { reason: "idle" | "pressure" | "operator"; at_unix_ms: number } | null;
};
type View = { generation: number; idle: number; budget: number | null };

const GIB = 1024 ** 3;

const choices = [0, 300_000, 900_000, 1_800_000, 3_600_000, 14_400_000];

export function duration(ms: number): string {
  if (ms === 0) return "Never";
  if (ms < 60_000) return `${Math.round(ms / 1000)} s`;
  if (ms < 3_600_000) return `${Math.round(ms / 60_000)} min`;
  const hours = ms / 3_600_000;
  return `${Number.isInteger(hours) ? hours : hours.toFixed(1)} h`;
}

export function remaining(ms: number): string {
  const minutes = Math.ceil(ms / 60_000);
  return minutes <= 1 ? "under a minute" : minutes < 60 ? `${minutes} min` : `${Math.round(minutes / 60)} h`;
}

export function parseResidencyView(value: unknown): View {
  const body = value && typeof value === "object" ? value as Record<string, unknown> : {};
  if (typeof body.configuration_generation !== "number" || typeof body.idle_unload_after_ms !== "number") throw new Error("The service returned unexpected residency settings.");
  return { generation: body.configuration_generation, idle: body.idle_unload_after_ms, budget: typeof body.resident_budget_bytes === "number" ? body.resident_budget_bytes : null };
}

function options(current: number) {
  return [...new Set([...choices, current])].sort((left, right) => (left || Infinity) - (right || Infinity));
}

export function residencyNote(model: Model): string | null {
  const residency = model.residency;
  if (!residency) return null;
  if (model.policy === "on-demand" && residency.idle_unload_at_unix_ms) return `Unloads in ${remaining(Math.max(0, residency.idle_unload_at_unix_ms - Date.now()))} if unused.`;
  if (!model.loaded && residency.last_unload && residency.last_unload.reason !== "operator") return residency.last_unload.reason === "idle" ? "Last unloaded after sitting idle." : "Last unloaded to make room for another model.";
  return null;
}

export function IdleDefault({ client, online }: { client: Client; online: boolean }) {
  const view = useResource(client, "juiced/residency", parseResidencyView, 5000);
  const operation = useOperation(client, view.reload);
  const data = view.data;
  if (!data) return null;
  return <label className="inline-field idle-default">Unload when idle
    <select aria-label="Unload idle On demand models after" value={data.idle} disabled={!online || operation.busy} onChange={(event) => {
      const next = Number(event.target.value);
      void operation.run("juiced/residency", { expected_generation: data.generation, idle_unload_after_ms: next }, next ? `Idle On demand models unload after ${duration(next)}.` : "On demand models stay loaded until memory is needed.");
    }}>{options(data.idle).map((ms) => <option key={ms} value={ms}>{ms ? `after ${duration(ms)}` : "never"}</option>)}</select>
  </label>;
}

export function ModelIdle({ model, client, online, disabled }: { model: Model; client: Client; online: boolean; disabled: boolean }) {
  const view = useResource(client, "juiced/residency", parseResidencyView, 5000);
  const operation = useOperation(client, view.reload);
  const data = view.data;
  const residency = model.residency;
  if (!data || !residency || model.policy !== "on-demand") return null;
  const own = residency.model_idle_unload_after_ms;
  const note = residency.source === "keep_alive" ? "A client set keep_alive for this model until its next request." : residencyNote(model);
  return <div className="idle-setting">
    <label className="inline-field">Unload when idle
      <select aria-label={`Idle unload for ${model.id}`} value={own === null ? "inherit" : own} disabled={disabled || !online || operation.busy} onChange={(event) => {
        const next = event.target.value === "inherit" ? null : Number(event.target.value);
        void operation.run("juiced/residency", { expected_generation: data.generation, model: model.id, idle_unload_after_ms: next }, next === null ? `${model.id} follows the service default.` : next ? `${model.id} unloads after ${duration(next)} idle.` : `${model.id} stays loaded until memory is needed.`);
      }}>
        <option value="inherit">Default ({data.idle ? `after ${duration(data.idle)}` : "never"})</option>
        {options(own ?? 0).map((ms) => <option key={ms} value={ms}>{ms ? `after ${duration(ms)}` : "never"}</option>)}
      </select>
    </label>
    {note && <p className="field-help">{note}</p>}
    {operation.error && <p className="inline-error" role="alert">{operation.error}</p>}
  </div>;
}

export function MemoryBudget({ client, online }: { client: Client; online: boolean }) {
  const view = useResource(client, "juiced/residency", parseResidencyView, 5000);
  const operation = useOperation(client, view.reload);
  const [draft, setDraft] = useState<string | null>(null);
  const data = view.data;
  if (!data || data.budget === null) return null;
  const current = Math.round(data.budget / GIB * 10) / 10;
  const value = draft === null ? null : Number(draft);
  const valid = value !== null && Number.isFinite(value) && value > 0;
  const save = () => {
    if (!valid || value === current) { setDraft(null); return; }
    void operation.run("juiced/residency", { expected_generation: data.generation, resident_budget_bytes: Math.round(value * GIB) }, `Models may now reserve up to ${value} GiB.`).then((result) => { if (result !== undefined) setDraft(null); });
  };
  return <span className="inline-field memory-budget">
    {draft === null
      ? <Button variant="ghost" disabled={!online} aria-label={`Model memory ${current} GiB. Change`} onClick={() => setDraft(String(current))}>Model memory {current} GiB</Button>
      : <form onSubmit={(event) => { event.preventDefault(); save(); }}>
          <label>Model memory <input type="number" min="1" step="1" inputMode="decimal" autoFocus aria-label="Model memory in GiB" value={draft} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Escape") setDraft(null); }} /> GiB</label>
          <Button type="submit" variant="primary" disabled={!valid || operation.busy}>Save</Button>
          <Button type="button" variant="ghost" onClick={() => setDraft(null)}>Cancel</Button>
        </form>}
    <HelpTip label="Model memory">The memory loaded models may reserve together. When a load would go over it, idle On demand models unload first. Hot and busy models stay.</HelpTip>
    {operation.error && <span className="inline-error" role="alert">{operation.error}</span>}
  </span>;
}
