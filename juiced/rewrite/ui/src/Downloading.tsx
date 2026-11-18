import { useState } from "react";
import type { Client } from "./api";
import { bytes, bytesPair } from "./domain";
import { parseEngines, type EngineJob } from "./engines";
import { backendNames } from "./hardware";
import { parsePulls, type PullView } from "./localLibraryData";
import { jobActions, parseArtifactInventory, type ArtifactJob } from "./management";
import { codeMessage, plainError, thenFailure } from "./modelErrors";
import { notify } from "./toast";
import { Badge, Button, Empty, HelpTip } from "./ui";
import { useResource } from "./useResource";

type Action = "pause" | "resume" | "retry" | "cancel" | "remove";
type Row = { key: string; title: string; detail: string; state: string; done: number; total: number | null; error: string | null; note?: string | null; jobs: ArtifactJob[]; pull?: PullView; engine?: boolean };

const stateNames: Record<string, string> = { installing: "Installing", "not-installed": "Not installed", updating: "Updating", "not-updated": "Not updated", queued: "Queued", downloading: "Downloading", pausing: "Pausing", cancelling: "Cancelling", paused: "Paused", waiting: "Paused", interrupted: "Interrupted", publishing: "Finishing", extracting: "Installing", verifying: "Verifying", failed: "Failed", quarantined: "Failed" };
const tone = (state: string) => ["failed", "quarantined", "not-installed", "not-updated"].includes(state) ? "bad" : ["paused", "waiting", "interrupted"].includes(state) ? "warning" : "quiet";
const thenStates = { add: { pending: "installing", failed: "not-installed" }, update: { pending: "updating", failed: "not-updated" } } as const;
const actionNames: Record<Action, string> = { pause: "Pause", resume: "Resume", retry: "Retry", cancel: "Cancel", remove: "Remove" };
const failure = (code: string) => codeMessage(code) ?? `The download failed (${code}).`;
/** States where the bytes are all here, so a progress bar would only repeat the size. */
const settledStates = ["installing", "not-installed", "updating", "not-updated"];

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

/** A finished download whose model exists has left the list; this also hides one the service has not dropped yet. */
export function listed(pull: PullView): boolean {
  return pull.state !== "complete" || (!!pull.then && pull.then.state !== "done");
}

export function pullRow(pull: PullView, jobs: ArtifactJob[]): Row {
  const owned = pullJobs(pull, jobs);
  const total = pull.files.reduce((sum, file) => sum + file.bytes, 0);
  const name = pull.repo.split("/").pop() ?? pull.repo;
  const then = pull.state === "complete" && pull.then && pull.then.state !== "done" ? thenStates[pull.then.kind][pull.then.state] : null;
  const present = pull.files.filter((file) => file.state === "present").length;
  const parts = pull.files.length > 1 ? pull.state === "complete" ? `${pull.files.length} parts` : `${present} of ${pull.files.length} parts` : null;
  const cause = owned.find((job) => job.error)?.error ?? pull.error;
  return { key: `pull:${pull.id}`, pull, title: [name, pull.label].filter(Boolean).join(" "), detail: [pull.repo.split("/")[0], parts].filter(Boolean).join(" · "), state: then ?? pull.state, done: pullBytes(pull, jobs), total: total || null, error: pull.error ? cause : null, note: pull.then?.state === "failed" && !pull.error ? thenFailure(pull.then) : null, jobs: owned };
}

/** What a row offers: a running download pauses or cancels; a stopped one resumes or retries, or is removed. */
export function rowActions(row: Pick<Row, "pull" | "jobs" | "engine">): Action[] {
  if (row.engine) return [];
  const pull = row.pull;
  if (pull?.state === "complete") return pull.then?.state === "failed" ? ["retry", "remove"] : [];
  const available = new Set<string>(row.jobs.flatMap(jobActions));
  if (pull?.state === "failed") available.add(pull.error === "artifact_network" ? "resume" : "retry");
  // Resume keeps the bytes already here, so it stands in for Retry when both apply.
  const forward: Action[] = available.has("pause") ? ["pause"] : available.has("resume") ? ["resume"] : available.has("retry") ? ["retry"] : [];
  const running = available.has("pause") || row.jobs.some((job) => job.writer_active);
  return [...forward, running ? "cancel" : "remove"];
}

function engineRow(job: EngineJob): Row {
  return { key: `engine:${job.id}`, title: `${job.engine} ${backendNames[job.backend] ?? job.backend} engine`, detail: "Engine install", state: job.state, done: job.downloaded_bytes, total: job.total_bytes, error: job.error, jobs: [], engine: true };
}

function jobRow(job: ArtifactJob): Row {
  return { key: `job:${job.id}`, title: jobName(job), detail: [host(job.source_url), job.source_url && job.computed_digest ? "unverified" : null].filter(Boolean).join(" · "), state: job.state, done: job.downloaded_bytes, total: job.expected_bytes ?? (job.source_url && job.computed_digest ? null : job.maximum_bytes), error: job.error, jobs: [job] };
}

/** Downloads in progress or failed. Finished and cancelled ones are not listed. */
export function downloadRows(pulls: PullView[], jobs: ArtifactJob[], engines: EngineJob[] = []): Row[] {
  const shown = pulls.filter(listed);
  const pulled = new Set(pulls.flatMap((pull) => pullJobs(pull, jobs).map((job) => job.id)));
  return [
    ...shown.map((pull) => pullRow(pull, jobs)),
    ...jobs.filter((job) => !pulled.has(job.id) && !["complete", "cancelled"].includes(job.state)).map(jobRow),
    ...engines.filter((job) => job.state !== "complete").map(engineRow),
  ];
}

export function useActiveDownloads(client: Client, enabled: boolean): number {
  const pulls = useResource(client, "juiced/library/pulls", parsePulls, 3000, true, enabled);
  const inventory = useResource(client, "juiced/artifacts?limit=64", parseArtifactInventory, 3000, true, enabled);
  return downloadRows(pulls.data ?? [], inventory.data?.jobs ?? []).length;
}

export function Downloading({ client, online, onSearch, onChanged }: { client: Client; online: boolean; onSearch: () => void; onChanged?: () => void }) {
  const pulls = useResource(client, "juiced/library/pulls", parsePulls, 1500, true);
  const inventory = useResource(client, "juiced/artifacts?limit=64", parseArtifactInventory, 1500, true);
  const engines = useResource(client, "juiced/engines", parseEngines, 2000, true);
  const [busy, setBusy] = useState("");
  const rows = downloadRows(pulls.data ?? [], inventory.data?.jobs ?? [], engines.data?.jobs ?? []);
  const loaded = pulls.data && inventory.data && engines.data;
  const error = pulls.error || inventory.error || engines.error;
  async function act(row: Row, action: Action) {
    setBusy(row.key);
    try {
      const pull = (step: string) => client.request(`juiced/library/pulls/${encodeURIComponent(row.pull!.id)}`, { action: step });
      const job = (step: string) => Promise.all(row.jobs.filter((item) => jobActions(item).includes(step as "pause")).map((item) => client.request(`juiced/artifacts/jobs/${encodeURIComponent(item.id)}`, { action: step, expected_revision: item.revision })));
      if (action === "cancel" || action === "remove") {
        if (row.pull) await pull("remove");
        else { await job("cancel"); await forget(client, row.jobs); }
      } else if (row.pull?.state === "complete") await pull("retry");
      else if (row.pull?.state === "failed") {
        // Resuming the pull itself keeps what it was to do when done, such as making the model.
        if (action === "retry") await job("retry");
        await pull("resume");
      } else await job(action);
      notify(action === "cancel" || action === "remove" ? `Removed the ${row.title} download.` : `${actionNames[action]} requested for ${row.title}.`);
      await Promise.all([pulls.reload(), inventory.reload()]);
      onChanged?.();
    } catch (caught) { notify(plainError(caught, "The change didn't apply. Retry."), "error"); }
    finally { setBusy(""); }
  }
  const leftovers = inventory.data?.quarantined_files ?? [];
  return <section className="downloads" aria-label="Downloads">
    {error && <div className="action-row"><p className="inline-error" role="alert">{loaded ? "Showing the last observation. " : ""}{error}</p><Button onClick={() => void Promise.all([pulls.reload(), inventory.reload(), engines.reload()])}>Retry</Button></div>}
    {!loaded ? <p className="muted" role="status">{error ? "" : "Reading downloads…"}</p>
      : rows.length === 0 ? <Empty title="Nothing downloading"><button className="link-button" onClick={onSearch}>Find a model</button> in Discover.</Empty>
      : <ul className="download-list" aria-label="Download list">{rows.map((row) => <li key={row.key} className="download-row" aria-label={row.title}>
        <div className="download-main"><div className="download-title"><strong className="break-word">{row.title}</strong><span className="muted">{row.detail}</span></div>
          <span className="muted download-size">{row.pull?.state === "complete" ? bytes(row.total ?? row.done) : row.total ? bytesPair(row.done, row.total, " of ") : bytes(row.done)}</span>
          <Badge tone={tone(row.state)}>{stateNames[row.state] ?? row.state}</Badge></div>
        {!settledStates.includes(row.state) && <progress aria-label={`${row.title} progress`} max={row.total ?? 1} value={row.total ? Math.min(row.done, row.total) : undefined} />}
        {(row.error || row.note) && <p className="inline-warning">{row.error ? failure(row.error) : row.note}</p>}
        <div className="action-row download-actions">{rowActions(row).map((action) => <Button key={action} variant={action === "cancel" || action === "remove" ? "ghost" : "outline"} disabled={!online || busy === row.key || (inventory.data?.source_policy === "offline" && ["resume", "retry"].includes(action))} onClick={() => void act(row, action)}>{actionNames[action]}</Button>)}</div>
      </li>)}</ul>}
    {leftovers.length > 0 && <p className="field-help download-leftovers">{leftovers.length} unfinished {leftovers.length === 1 ? "file holds" : "files hold"} {bytes(leftovers.reduce((sum, file) => sum + file.reserved_bytes, 0))} <HelpTip label="Unfinished files">Partial files left by an interrupted download. Their space stays reserved until Juiced can check them.</HelpTip></p>}
  </section>;
}

/** A cancelled transfer holds nothing, so its record goes too. */
async function forget(client: Client, jobs: ArtifactJob[]) {
  for (const job of jobs) {
    try {
      const view = await client.request<{ state?: string; revision?: number }>(`juiced/artifacts/jobs/${encodeURIComponent(job.id)}`);
      if (view.state === "cancelled" && typeof view.revision === "number") await client.request(`juiced/artifacts/jobs/${encodeURIComponent(job.id)}`, { action: "forget", expected_revision: view.revision });
    } catch { /* A transfer still stopping is not listed once it is cancelled. */ }
  }
}
