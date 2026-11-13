import { useEffect, useRef, useState } from "react";
import "./quant-sync.css";
import type { Client } from "./api";
import type { Model } from "./domain";
import type { LaunchOptions } from "./profiles";
import { notify } from "./toast";
import { Button, HelpTip } from "./ui";
import { useResource } from "./useResource";

export type SyncField = "context_tokens" | "threads" | "gpu_layers" | "parallel_slots" | "batching" | "attention" | "sampling" | "extra_args";
export type VariantMember = { model: string; revision: number; quant: string | null; loaded: boolean; state: string | null; saved: LaunchOptions; eligible: boolean; reason: string | null; differs: SyncField[] };
export type Variants = { model: string; revision: number; syncable: boolean; saved: LaunchOptions; group: { id: string; label: string } | null; members: VariantMember[] };
export type SkippedField = { field: SyncField; reason: string; value?: number; limit?: number };
export type SyncResult = { model: string; outcome: "updated" | "unchanged" | "skipped" | "reload_failed"; reason: string | null; copied: SyncField[]; skipped_fields: SkippedField[]; restarted: boolean; restored_revision?: number | null };
export type SyncReply = { generation: number | null; results: SyncResult[] };

const fieldNames: Record<SyncField, string> = { context_tokens: "context", threads: "threads", gpu_layers: "GPU layers", parallel_slots: "slots", batching: "batching", attention: "attention and KV cache", sampling: "generation defaults", extra_args: "extra arguments" };
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
const integer = (value: unknown): value is number => Number.isSafeInteger(value) && (value as number) >= 0;
const field = (value: unknown): value is SyncField => typeof value === "string" && value in fieldNames;
const text = (value: unknown) => value === null || typeof value === "string";

export function parseVariants(value: unknown): Variants {
  const fail = () => { throw new Error("The service returned invalid quant groups. Retry."); };
  if (!object(value) || typeof value.model !== "string" || !integer(value.revision) || typeof value.syncable !== "boolean" || !object(value.saved) || !Array.isArray(value.members)) fail();
  const view = value as Record<string, unknown>;
  if (view.group !== null && (!object(view.group) || typeof view.group.id !== "string" || typeof view.group.label !== "string")) fail();
  for (const member of view.members as unknown[]) {
    if (!object(member) || typeof member.model !== "string" || !integer(member.revision) || !text(member.quant) || typeof member.loaded !== "boolean" || typeof member.eligible !== "boolean" || !text(member.reason) || !Array.isArray(member.differs) || !member.differs.every(field)) fail();
  }
  return value as Variants;
}

export function parseSyncReply(value: unknown): SyncReply {
  const outcomes = ["updated", "unchanged", "skipped", "reload_failed"];
  if (!object(value) || !(value.generation === null || integer(value.generation)) || !Array.isArray(value.results) || !value.results.every((result) => object(result) && typeof result.model === "string" && outcomes.includes(result.outcome as string) && text(result.reason) && Array.isArray(result.copied) && result.copied.every(field) && Array.isArray(result.skipped_fields) && result.skipped_fields.every((skip) => object(skip) && field(skip.field) && typeof skip.reason === "string") && typeof result.restarted === "boolean")) {
    throw new Error("The copy finished, but its report is invalid. Check each quant's settings.");
  }
  return value as SyncReply;
}

const memberReasons: Record<string, string> = {
  different_engine: "Different engine", different_architecture: "Different architecture", native_settings_authority: "Imported settings",
  typed_options_unsupported: "No launch settings", worker_in_use: "Unload it first", worker_busy: "Busy with requests",
  worker_cleanup_pending: "Cleanup pending", worker_preparation_pending: "Starting", slot_resize_requires_registration: "Slot count differs",
};
const reasonText = (reason: string) => memberReasons[reason] ?? "Not available";
const list = (fields: SyncField[]) => fields.map((name) => fieldNames[name]).join(", ");

function skipText(skip: SkippedField): string {
  const name = fieldNames[skip.field];
  if (skip.reason === "context_exceeds_training") return `${name} kept: over its ${skip.limit?.toLocaleString() ?? "trained"} trained tokens`;
  if (skip.reason === "context_exceeds_budget") return `${name} kept: doesn't fit the memory budget`;
  if (skip.reason === "slot_resize_requires_registration") return `${name} kept: it has ${skip.limit ?? "other"} slots`;
  return `${name} kept`;
}

export function describeResult(result: SyncResult): string {
  const skipped = result.skipped_fields.map(skipText);
  if (result.outcome === "reload_failed") return `Didn't start with the new settings${result.restored_revision != null ? "; previous settings restored" : ""}.`;
  if (result.outcome === "unchanged") return skipped.length ? `${skipped.join("; ")}.` : "Already the same.";
  if (result.outcome === "skipped") return [result.reason ? reasonText(result.reason) : null, ...skipped].filter(Boolean).join("; ") + ".";
  return [`Copied ${list(result.copied)}`, ...skipped].join("; ") + (result.restarted ? ". Restarted." : ".");
}

function memberStatus(member: VariantMember): string {
  if (!member.eligible) return reasonText(member.reason ?? "");
  if (member.differs.length === 0) return "Same settings";
  return `Differs: ${list(member.differs)}${member.loaded ? " · restarts" : ""}`;
}

export function QuantSync({ model, client, online, busy, blocked }: { model: Model; client: Client; online: boolean; busy: boolean; blocked: boolean }) {
  const variants = useResource(client, `models/${encodeURIComponent(model.id)}/variants`, parseVariants, 0);
  const [selected, setSelected] = useState<string[]>([]);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState("");
  const [report, setReport] = useState<SyncResult[] | null>(null);
  const revision = useRef(model.revision);
  useEffect(() => {
    if (revision.current !== model.revision) { revision.current = model.revision; setReport(null); void variants.reload(); }
  }, [model.revision, variants.reload]);
  const view = variants.data?.model === model.id ? variants.data : null;
  if (!view) return variants.error ? <section className="inspector-section"><p role="alert" className="inline-error">{variants.error}</p></section> : null;
  if (!view.syncable || !view.group || view.members.length === 0) return null;
  const selectable = (member: VariantMember) => member.eligible && member.differs.length > 0;
  const chosen = view.members.filter((member) => selected.includes(member.model) && selectable(member));
  const disabled = busy || running || !online || blocked;
  async function copy() {
    if (!view || chosen.length === 0) return;
    setRunning(true); setError(""); setReport(null);
    try {
      const reply = parseSyncReply(await client.request(`models/${encodeURIComponent(model.id)}/variants`, {
        expected_revision: view.revision,
        targets: chosen.map((member) => ({ model: member.model, expected_revision: member.revision })),
        ...(chosen.some((member) => member.loaded) ? { apply: "reload" } : {}),
      }));
      setReport(reply.results); setSelected([]);
      const updated = reply.results.filter((result) => result.outcome === "updated").length;
      notify(updated ? `Copied settings to ${updated} ${updated === 1 ? "quant" : "quants"}.` : "No quant needed changes.", reply.results.some((result) => result.outcome === "reload_failed") ? "error" : "success");
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "Copying settings failed. Retry.");
    } finally {
      setRunning(false);
      void variants.reload();
    }
  }
  const label = (member: VariantMember) => member.quant && view.members.filter((other) => other.quant === member.quant).length === 1 ? member.quant : member.model;
  const quantName = (id: string) => { const member = view.members.find((entry) => entry.model === id); return member ? label(member) : id; };
  return <section className="inspector-section quant-sync" aria-label="Other quants">
    <div className="section-heading"><div className="page-title"><h3>Other quants</h3><HelpTip label="Other quants">Copies this model's saved launch settings, generation defaults and extra arguments to other quants of {view.group.label}. A setting that doesn't fit a quant stays as it is and is listed after copying. Loaded quants restart.</HelpTip></div></div>
    <ul className="quant-sync-list">{view.members.map((member) => <li key={member.model}>
      <label title={member.model}>
        <input type="checkbox" checked={selected.includes(member.model) && selectable(member)} disabled={disabled || !selectable(member)} onChange={(event) => setSelected(event.target.checked ? [...selected, member.model] : selected.filter((id) => id !== member.model))} />
        <span className="quant-sync-name"><strong>{label(member)}</strong><span className={selectable(member) ? undefined : "muted"}>{memberStatus(member)}</span></span>
      </label>
    </li>)}</ul>
    {blocked && <p className="field-help">Save or cancel your changes first.</p>}
    {error && <p role="alert" className="inline-error">{error}</p>}
    {report && <ul className="quant-sync-report" aria-label="Copy results" aria-live="polite">{report.map((result) => <li key={result.model} className={result.outcome === "reload_failed" ? "inline-error" : result.outcome === "skipped" ? "inline-warning" : undefined}><strong>{quantName(result.model)}</strong> {describeResult(result)}</li>)}</ul>}
    {view.members.some(selectable) && <div className="action-row wrap"><Button disabled={disabled || chosen.length === 0} onClick={() => void copy()}>{running ? "Copying…" : chosen.length === 0 ? "Copy settings" : `Copy to ${chosen.length} ${chosen.length === 1 ? "quant" : "quants"}`}</Button></div>}
  </section>;
}
