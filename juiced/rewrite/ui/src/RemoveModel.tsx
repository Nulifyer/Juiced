import { useEffect, useState } from "react";
import type { Client } from "./api";
import { bytes } from "./domain";
import { plainError } from "./modelErrors";
import { notify } from "./toast";
import { Button, Dialog } from "./ui";
import { useResource } from "./useResource";

export type RemovalKeep = { name: string; reason: string; usedBy: string[]; folder: string };
export type RemovalPlan = { revision: number; removable: boolean; refusal: string | null; freedBytes: number; deletes: number; keeps: RemovalKeep[] };

const object = (value: unknown): Record<string, unknown> => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
const count = (value: unknown) => typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
const text = (value: unknown) => typeof value === "string" ? value : "";

export function parseRemoval(value: unknown): RemovalPlan {
  const row = object(value);
  if (typeof row.removable !== "boolean") throw new Error("The removal preview is invalid. Retry.");
  const keeps = Array.isArray(row.keeps) ? row.keeps.map(object) : [];
  return {
    revision: count(row.revision), removable: row.removable, refusal: text(object(row.refusal).message) || null, freedBytes: count(row.freed_bytes),
    deletes: Array.isArray(row.deletes) ? row.deletes.length : 0,
    keeps: keeps.map((keep) => ({ name: text(keep.name), reason: text(keep.reason), usedBy: Array.isArray(keep.used_by) ? keep.used_by.filter((id): id is string => typeof id === "string") : [], folder: text(keep.location).replace(/[\\/][^\\/]*$/, "") })),
  };
}

/** What a removal deletes, in one sentence. */
export function removalSummary(plan: Pick<RemovalPlan, "deletes" | "freedBytes">): string {
  return plan.deletes ? `Deletes ${plan.deletes} ${plan.deletes === 1 ? "file" : "files"} and frees ${bytes(plan.freedBytes)}.` : "Deletes no files.";
}

/** Why a file stays on disk after its model is removed. */
export function keepNote(keep: RemovalKeep): string {
  if (keep.reason === "shared") return `${keep.name} stays. ${keep.usedBy.join(", ")} ${keep.usedBy.length === 1 ? "uses" : "use"} it.`;
  if (keep.reason === "linked") return `${keep.name} stays in ${keep.folder}. Juiced links it and didn't put it there.`;
  return `${keep.name} couldn't be deleted. It stays as a Disabled model.`;
}

/** Remove for one model: the reason when it can't be removed, and one confirmation that says what is deleted and what stays. */
export function RemoveModel({ client, model, revision, online, disabled, blocked, label = "Remove model", onRemoved }: { client: Client; model: string; revision?: number; online: boolean; disabled?: boolean; blocked?: string; label?: string; onRemoved: () => void | Promise<void> }) {
  const plan = useResource(client, `juiced/models/removal?model=${encodeURIComponent(model)}`, parseRemoval, 0, true, online);
  const [confirming, setConfirming] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [error, setError] = useState("");
  const reload = plan.reload;
  useEffect(() => { if (online && revision !== undefined) void reload(); }, [revision, online, reload]);
  const reason = blocked ?? plan.data?.refusal ?? (plan.error && !plan.data ? plan.error : null);
  async function open() {
    setError("");
    const fresh = await plan.reload();
    if (fresh?.removable) setConfirming(true);
  }
  async function remove() {
    if (!plan.data) return;
    setRemoving(true); setError("");
    try {
      const reply = await client.request<{ freed_bytes?: number; kept?: { name?: string; reason?: string }[] }>("juiced/models/remove", { model, expected_revision: plan.data.revision });
      const undeleted = (reply.kept ?? []).filter((keep) => keep.reason === "undeleted").length;
      setConfirming(false);
      notify(undeleted ? `Removed ${model}, but ${undeleted} ${undeleted === 1 ? "file" : "files"} couldn't be deleted. Review them under Storage.` : `Removed ${model}. Freed ${bytes(reply.freed_bytes ?? 0)}.`, undeleted ? "error" : undefined);
      await onRemoved();
    } catch (failure) { setError(plainError(failure, "The model wasn't removed. Retry.")); void plan.reload(); }
    finally { setRemoving(false); }
  }
  return <>
    <Button variant="danger" disabled={!online || disabled || !!reason || !plan.data || removing} onClick={() => void open()}>{label}</Button>
    {reason && <p className="field-help remove-reason">{reason}</p>}
    {confirming && plan.data && <Dialog title={`Remove ${model}?`} onClose={() => setConfirming(false)}>
      <p>{removalSummary(plan.data)} Its settings go too.</p>
      {plan.data.keeps.map((keep) => <p key={keep.name} className="field-help">{keepNote(keep)}</p>)}
      {error && <p className="inline-error" role="alert">{error}</p>}
      <div className="dialog-actions"><Button onClick={() => setConfirming(false)}>Cancel</Button><Button variant="danger" disabled={removing} onClick={() => void remove()}>{removing ? "Removing…" : "Remove model"}</Button></div>
    </Dialog>}
  </>;
}
