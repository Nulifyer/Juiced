import { useState } from "react";
import type { Client } from "./api";
import { bytes, bytesPair } from "./domain";
import { parseEngines, type EngineJob } from "./engines";
import { backendNames } from "./hardware";
import { parsePulls, type LocalInventory, type LocalQuant, type PullView } from "./localLibraryData";
import { jobActions, parseArtifactInventory, type ArtifactJob } from "./management";
import { codeMessage, plainError } from "./modelErrors";
import { notify } from "./toast";
import { Badge, Button, Empty, HelpTip } from "./ui";
import { useResource } from "./useResource";

type Action = "pause" | "resume" | "retry" | "cancel" | "forget";
type Row = { key: string; title: string; detail: string; state: string; done: number; total: number | null; error: string | null; note?: string | null; jobs: ArtifactJob[]; pulls: string[]; pull?: PullView; finished: boolean };
export type NextStep = { kind: "open"; model: string } | { kind: "add"; group: string; quant: string | null };

const stateNames: Record<string, string> = { adding: "Adding", "not-added": "Not added", updating: "Updating", "not-updated": "Not updated", queued: "Queued", downloading: "Downloading", pausing: "Pausing", paused: "Paused", waiting: "Paused", interrupted: "Interrupted", publishing: "Finishing", installing: "Installing", verifying: "Verifying", complete: "Done", cancelled: "Cancelled", failed: "Failed", quarantined: "Failed" };
const tone = (state: string) => state === "complete" ? "good" : ["failed", "quarantined", "not-added", "not-updated"].includes(state) ? "bad" : ["paused", "waiting", "interrupted"].includes(state) ? "warning" : "quiet";
const thenStates = { add: { pending: "adding", done: null, failed: "not-added" }, update: { pending: "updating", done: null, failed: "not-updated" } } as const;
const actionNames: Record<Action, string> = { pause: "Pause", resume: "Resume", retry: "Retry", cancel: "Cancel", forget: "Dismiss" };
const failure = (code: string) => codeMessage(code) ?? `The download failed (${code}).`;

/** The jobs behind a pull: the ones it names, and a failed part's job found by its source. */
export function pullJobs(pull: PullView, jobs: ArtifactJob[]): ArtifactJob[] {
  const owned = new Map<string, ArtifactJob>();
  for (const file of pull.files) {
    const job = fileJob(file, jobs);
    if (job) owned.set(job.id, job);
  }
  return [...owned.values()];
}

function fileJob(file: PullView["files"][number], jobs: ArtifactJob[]): ArtifactJob | undefined {
  if (file.job) return jobs.find((job) => job.id === file.job);
  return file.source && file.state !== "present" ? jobs.find((item) => item.source_url === file.source && item.state !== "complete") : undefined;
}

/** Bytes on disk for a pull; a failed part no longer names its job, but the job keeps its bytes. */
export function pullBytes(pull: PullView, jobs: ArtifactJob[]): number {
  return pull.files.reduce((sum, file) => sum + Math.min(Math.max(file.downloaded, fileJob(file, jobs)?.downloaded_bytes ?? 0), file.bytes || Infinity), 0);
}

function jobName(job: ArtifactJob & { source_name?: string | null }) {
  if (job.source_name) return job.source_name;
  try { return decodeURIComponent(new URL(job.source_url!).pathname.split("/").filter(Boolean).at(-1) || "Download"); } catch { return "Download"; }
}

function host(url: string | null) {
  try { return url ? new URL(url).host || "Local copy" : "Local copy"; } catch { return "Download"; }
}

function pullRow(pull: PullView, jobs: ArtifactJob[]): Row {
  const owned = pullJobs(pull, jobs);
  const total = pull.files.reduce((sum, file) => sum + file.bytes, 0);
  const name = pull.repo.split("/").pop() ?? pull.repo;
  const then = pull.state === "complete" && pull.then ? thenStates[pull.then.kind][pull.then.state] : null;
  const settling = pull.then?.state === "pending";
  const present = pull.files.filter((file) => file.state === "present").length;
  const parts = pull.files.length > 1 ? pull.state === "complete" ? `${pull.files.length} parts` : `${present} of ${pull.files.length} parts` : null;
  const cause = owned.find((job) => job.error)?.error ?? pull.error;
  return { key: `pull:${pull.id}`, pulls: [pull.id], pull, title: [name, pull.label].filter(Boolean).join(" "), detail: [pull.repo.split("/")[0], parts].filter(Boolean).join(" · "), state: then ?? pull.state, done: pullBytes(pull, jobs), total: total || null, error: pull.error ? cause : null, note: pull.then?.state === "failed" && !pull.error ? pull.then.message : null, jobs: owned, finished: ["complete", "failed"].includes(pull.state) && !settling };
}

/** A finished pull leads to the model that uses its files, or to the file details to add one. */
export function nextStep(pull: PullView | undefined, local: LocalInventory | null | undefined): NextStep | null {
  if (pull?.state === "complete" && pull.then?.state === "pending") return null;
  const model = pull?.state === "complete" && pull.then?.state === "done" ? pull.then.model : null;
  if (model && local?.groups.some((group) => group.usedBy.includes(model) || group.quants.some((quant) => quant.usedBy.includes(model)))) return { kind: "open", model };
  if (!pull || pull.state !== "complete" || !local) return null;
  const group = local.groups.find((item) => item.repo?.toLowerCase() === pull.repo.toLowerCase());
  if (!group) return null;
  const names = new Set(pull.files.map((file) => file.file.split("/").pop()));
  const holds = (quant: LocalQuant) => quant.files.some((id) => names.has(group.files.find((file) => file.id === id)?.name));
  const quant = group.quants.find(holds) ?? group.quants.find((item) => !!pull.label && item.label.toLowerCase() === pull.label.toLowerCase());
  if (quant) return quant.usedBy[0] ? { kind: "open", model: quant.usedBy[0] } : { kind: "add", group: group.id, quant: quant.file };
  const unused = group.quants.length ? group.quants.some((item) => item.usedBy.length === 0) : group.usedBy.length === 0;
  return !unused && group.usedBy[0] ? { kind: "open", model: group.usedBy[0] } : { kind: "add", group: group.id, quant: null };
}

/** Pulling the same quant again leaves a finished entry per attempt; one row stands for them all. */
export function collapseFinished<T extends { title: string; state: string; finished: boolean; pulls: string[]; jobs: unknown[] }>(rows: T[]): T[] {
  const first = new Map<string, T>();
  return rows.filter((row) => {
    if (!row.finished) return true;
    const key = `${row.title}\n${row.state}`;
    const kept = first.get(key);
    if (!kept) { first.set(key, row); return true; }
    kept.pulls.push(...row.pulls); kept.jobs.push(...row.jobs);
    return false;
  });
}

function engineRow(job: EngineJob): Row {
  return { key: `engine:${job.id}`, title: `${job.engine} ${backendNames[job.backend] ?? job.backend} engine`, detail: "Engine install", state: job.state, done: job.downloaded_bytes, total: job.total_bytes, error: job.error, jobs: [], pulls: [], finished: ["complete", "failed"].includes(job.state) };
}

export function useActiveDownloads(client: Client, enabled: boolean): number {
  const pulls = useResource(client, "juiced/library/pulls", parsePulls, 3000, true, enabled);
  const inventory = useResource(client, "juiced/artifacts?limit=64", parseArtifactInventory, 3000, true, enabled);
  const pulled = new Set((pulls.data ?? []).flatMap((pull) => pullJobs(pull, inventory.data?.jobs ?? []).map((job) => job.id)));
  return (pulls.data ?? []).filter((pull) => !["complete", "failed"].includes(pull.state)).length
    + (inventory.data?.jobs ?? []).filter((job) => job.writer_active && !pulled.has(job.id)).length;
}

export function Downloading({ client, online, onSearch, local, onOpenModel, onAddFile }: { client: Client; online: boolean; onSearch: () => void; local?: LocalInventory | null; onOpenModel?: (id: string) => void; onAddFile?: (group: string, quant: string | null) => void }) {
  const pulls = useResource(client, "juiced/library/pulls", parsePulls, 1500, true);
  const inventory = useResource(client, "juiced/artifacts?limit=64", parseArtifactInventory, 1500, true);
  const engines = useResource(client, "juiced/engines", parseEngines, 2000, true);
  const [busy, setBusy] = useState("");
  const jobs = inventory.data?.jobs ?? [];
  const pulled = new Set((pulls.data ?? []).flatMap((pull) => pullJobs(pull, jobs).map((job) => job.id)));
  const rows: Row[] = collapseFinished([
    ...(pulls.data ?? []).map((pull) => pullRow(pull, jobs)),
    // Completed jobs carry the source names Downloaded groups by, so they are never forgotten here; the file shows under Downloaded instead.
    ...(inventory.data?.jobs ?? []).filter((job) => !pulled.has(job.id) && job.state !== "complete").map((job): Row => ({ key: `job:${job.id}`, title: jobName(job), detail: [host(job.source_url), job.source_url && job.computed_digest ? "unverified" : null].filter(Boolean).join(" · "), state: job.state, done: job.downloaded_bytes, total: job.expected_bytes ?? (job.source_url && job.computed_digest ? null : job.maximum_bytes), error: job.error, jobs: [job], pulls: [], finished: !job.writer_active && ["complete", "cancelled", "failed", "quarantined"].includes(job.state) })),
    ...(engines.data?.jobs ?? []).map(engineRow),
  ].sort((left, right) => Number(left.finished) - Number(right.finished)));
  const loaded = pulls.data && inventory.data && engines.data;
  const error = pulls.error || inventory.error || engines.error;
  async function act(row: Row, action: Action) {
    setBusy(row.key);
    try {
      // Resuming the pull itself keeps what it was to do when done, such as adding the model.
      const revive = row.pull?.state === "failed" && (action === "resume" || action === "retry");
      if (!(revive && action === "resume")) for (const job of row.jobs.filter((item) => jobActions(item).includes(action) && !(action === "forget" && item.state === "complete"))) await client.request(`juiced/artifacts/jobs/${encodeURIComponent(job.id)}`, { action, expected_revision: job.revision });
      if (revive) await client.request(`juiced/library/pulls/${encodeURIComponent(row.pull!.id)}`, { action: "resume" });
      if (action === "forget") for (const pull of row.pulls) await client.request(`juiced/library/pulls/${encodeURIComponent(pull)}`, { action: "dismiss" });
      notify(action === "forget" ? `Dismissed ${row.title}.` : `${actionNames[action]} requested for ${row.title}.`);
      await Promise.all([pulls.reload(), inventory.reload()]);
    } catch (caught) { notify(plainError(caught, "The change didn't apply. Retry."), "error"); }
    finally { setBusy(""); }
  }
  const actions = (row: Row): Action[] => {
    const available = new Set(row.jobs.flatMap(jobActions));
    // Partial files stay on disk until the jobs are cancelled, so a failed pull offers Retry and Cancel first.
    if (row.pulls.length && row.finished && !["retry", "resume", "cancel"].some((action) => available.has(action as Action))) available.add("forget");
    if (row.pulls.length && !row.finished) available.delete("forget");
    if (row.pull?.state === "failed" && row.pull.error === "artifact_network") available.add("resume");
    return (["pause", "resume", "retry", "cancel", "forget"] as Action[]).filter((action) => available.has(action));
  };
  const leftovers = inventory.data?.quarantined_files ?? [];
  return <section className="downloads" aria-label="Downloads">
    {error && <div className="action-row"><p className="inline-error" role="alert">{loaded ? "Showing the last observation. " : ""}{error}</p><Button onClick={() => void Promise.all([pulls.reload(), inventory.reload(), engines.reload()])}>Retry</Button></div>}
    {!loaded ? <p className="muted" role="status">{error ? "" : "Reading downloads…"}</p>
      : rows.length === 0 ? <Empty title="Nothing downloading"><button className="link-button" onClick={onSearch}>Find a model</button> in Discover.</Empty>
      : <ul className="download-list" aria-label="Download list">{rows.map((row) => { const step = nextStep(row.pull, local); const settled = ["complete", "cancelled", "adding", "not-added", "updating", "not-updated"].includes(row.state); return <li key={row.key} className="download-row" aria-label={row.title}>
        <div className="download-main"><div className="download-title"><strong className="break-word">{row.title}</strong><span className="muted">{row.detail}</span></div>
          <span className="muted download-size">{row.pull?.state === "complete" || row.state === "complete" ? bytes(row.total ?? row.done) : row.total ? bytesPair(row.done, row.total, " of ") : bytes(row.done)}</span>
          <Badge tone={tone(row.state)}>{stateNames[row.state] ?? row.state}</Badge></div>
        {!settled && <progress aria-label={`${row.title} progress`} max={row.total ?? 1} value={row.total ? Math.min(row.done, row.total) : undefined} />}
        {(row.error || row.note) && <p className="inline-warning">{row.error ? failure(row.error) : row.note}</p>}
        <div className="action-row download-actions">{step && <Button onClick={() => step.kind === "open" ? onOpenModel?.(step.model) : onAddFile?.(step.group, step.quant)}>{step.kind === "open" ? "Open model" : "Add model"}</Button>}{actions(row).map((action) => <Button key={action} variant={action === "cancel" ? "danger" : step || action === "forget" ? "ghost" : "outline"} disabled={!online || busy === row.key || (inventory.data?.source_policy === "offline" && ["resume", "retry"].includes(action))} onClick={() => void act(row, action)}>{actionNames[action]}</Button>)}</div>
      </li>; })}</ul>}
    {leftovers.length > 0 && <p className="field-help download-leftovers">{leftovers.length} unfinished {leftovers.length === 1 ? "file holds" : "files hold"} {bytes(leftovers.reduce((sum, file) => sum + file.reserved_bytes, 0))} <HelpTip label="Unfinished files">Partial files left by an interrupted download. Their space stays reserved until Juiced can check them.</HelpTip></p>}
  </section>;
}
