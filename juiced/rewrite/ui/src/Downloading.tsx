import { useState } from "react";
import type { Client } from "./api";
import { bytes } from "./domain";
import { parseEngines, type EngineJob } from "./engines";
import { backendNames } from "./hardware";
import { parsePulls, type PullView } from "./localLibraryData";
import { jobActions, parseArtifactInventory, type ArtifactJob } from "./management";
import { notify } from "./toast";
import { Badge, Button, Empty, HelpTip } from "./ui";
import { useResource } from "./useResource";

type Action = "pause" | "resume" | "retry" | "cancel" | "forget";
type Row = { key: string; title: string; detail: string; state: string; done: number; total: number | null; error: string | null; jobs: ArtifactJob[]; pull?: string; finished: boolean };

const stateNames: Record<string, string> = { queued: "Queued", downloading: "Downloading", pausing: "Pausing", paused: "Paused", waiting: "Paused", interrupted: "Interrupted", publishing: "Finishing", installing: "Installing", verifying: "Verifying", complete: "Done", cancelled: "Cancelled", failed: "Failed", quarantined: "Failed" };
const tone = (state: string) => state === "complete" ? "good" : ["failed", "quarantined"].includes(state) ? "bad" : ["paused", "waiting", "interrupted"].includes(state) ? "warning" : "quiet";
const actionNames: Record<Action, string> = { pause: "Pause", resume: "Resume", retry: "Retry", cancel: "Cancel", forget: "Dismiss" };
const failures: Record<string, string> = {
  storage_root_unavailable: "A storage folder moved or disappeared. Restore it, then retry.",
  artifact_network: "The download didn't finish. Retry when the source is reachable.",
  artifact_hash_mismatch: "The file didn't match its SHA256.",
  artifact_size_mismatch: "The file size didn't match or went over the limit.",
  artifact_source_changed: "The source file changed. Retry starts over.",
  artifact_source_policy: "This source or redirect isn't allowed.",
  artifact_publication_uncertain: "Saving the file may not have finished. Its space stays reserved.",
};
const failure = (code: string) => failures[code] ?? `The download failed (${code}).`;

function jobName(job: ArtifactJob & { source_name?: string | null }) {
  if (job.source_name) return job.source_name;
  try { return decodeURIComponent(new URL(job.source_url!).pathname.split("/").filter(Boolean).at(-1) || "Download"); } catch { return "Download"; }
}

function host(url: string | null) {
  try { return url ? new URL(url).host || "Local copy" : "Local copy"; } catch { return "Download"; }
}

function pullRow(pull: PullView, jobs: Map<string, ArtifactJob>): Row {
  const owned = pull.files.flatMap((file) => file.job ? [jobs.get(file.job)].filter((job): job is ArtifactJob => !!job) : []);
  const total = pull.files.reduce((sum, file) => sum + file.bytes, 0);
  const name = pull.repo.split("/").pop() ?? pull.repo;
  return { key: `pull:${pull.id}`, pull: pull.id, title: [name, pull.label].filter(Boolean).join(" "), detail: [pull.repo.split("/")[0], pull.files.length > 1 ? `${pull.files.length} files` : null, "Hugging Face"].filter(Boolean).join(" · "), state: pull.state, done: pull.files.reduce((sum, file) => sum + Math.min(file.downloaded, file.bytes || file.downloaded), 0), total: total || null, error: pull.error, jobs: owned, finished: ["complete", "failed"].includes(pull.state) };
}

function engineRow(job: EngineJob): Row {
  return { key: `engine:${job.id}`, title: `${job.engine} ${backendNames[job.backend] ?? job.backend} engine`, detail: "Engine install", state: job.state, done: job.downloaded_bytes, total: job.total_bytes, error: job.error, jobs: [], finished: ["complete", "failed"].includes(job.state) };
}

export function useActiveDownloads(client: Client, enabled: boolean): number {
  const pulls = useResource(client, "juiced/library/pulls", parsePulls, 3000, true, enabled);
  const inventory = useResource(client, "juiced/artifacts?limit=64", parseArtifactInventory, 3000, true, enabled);
  const pulled = new Set((pulls.data ?? []).flatMap((pull) => pull.files.map((file) => file.job).filter(Boolean) as string[]));
  return (pulls.data ?? []).filter((pull) => !["complete", "failed"].includes(pull.state)).length
    + (inventory.data?.jobs ?? []).filter((job) => job.writer_active && !pulled.has(job.id)).length;
}

export function Downloading({ client, online, onSearch }: { client: Client; online: boolean; onSearch: () => void }) {
  const pulls = useResource(client, "juiced/library/pulls", parsePulls, 1500, true);
  const inventory = useResource(client, "juiced/artifacts?limit=64", parseArtifactInventory, 1500, true);
  const engines = useResource(client, "juiced/engines", parseEngines, 2000, true);
  const [busy, setBusy] = useState("");
  const jobs = new Map((inventory.data?.jobs ?? []).map((job) => [job.id, job]));
  const pulled = new Set((pulls.data ?? []).flatMap((pull) => pull.files.map((file) => file.job).filter(Boolean) as string[]));
  const rows: Row[] = [
    ...(pulls.data ?? []).map((pull) => pullRow(pull, jobs)),
    // Completed jobs carry the source names Downloaded groups by, so they are never forgotten here; the file shows under Downloaded instead.
    ...(inventory.data?.jobs ?? []).filter((job) => !pulled.has(job.id) && job.state !== "complete").map((job): Row => ({ key: `job:${job.id}`, title: jobName(job), detail: host(job.source_url), state: job.state, done: job.downloaded_bytes, total: job.expected_bytes ?? job.maximum_bytes, error: job.error, jobs: [job], finished: !job.writer_active && ["complete", "cancelled", "failed", "quarantined"].includes(job.state) })),
    ...(engines.data?.jobs ?? []).map(engineRow),
  ].sort((left, right) => Number(left.finished) - Number(right.finished));
  const loaded = pulls.data && inventory.data && engines.data;
  const error = pulls.error || inventory.error || engines.error;
  async function act(row: Row, action: Action) {
    setBusy(row.key);
    try {
      for (const job of row.jobs.filter((item) => jobActions(item).includes(action) && !(action === "forget" && item.state === "complete"))) await client.request(`juiced/artifacts/jobs/${encodeURIComponent(job.id)}`, { action, expected_revision: job.revision });
      if (action === "forget" && row.pull) await client.request(`juiced/library/pulls/${encodeURIComponent(row.pull)}`, { action: "dismiss" });
      notify(action === "forget" ? `Dismissed ${row.title}.` : `${actionNames[action]} requested for ${row.title}.`);
      await Promise.all([pulls.reload(), inventory.reload()]);
    } catch (caught) { notify(caught instanceof Error ? caught.message : "The change didn't apply. Retry.", "error"); }
    finally { setBusy(""); }
  }
  const actions = (row: Row): Action[] => {
    const available = new Set(row.jobs.flatMap(jobActions));
    if (row.pull && row.finished) available.add("forget");
    if (row.pull && !row.finished) available.delete("forget");
    return (["pause", "resume", "retry", "cancel", "forget"] as Action[]).filter((action) => available.has(action));
  };
  const leftovers = inventory.data?.quarantined_files ?? [];
  return <section className="downloads" aria-label="Downloads">
    {error && <div className="action-row"><p className="inline-error" role="alert">{loaded ? "Showing the last observation. " : ""}{error}</p><Button onClick={() => void Promise.all([pulls.reload(), inventory.reload(), engines.reload()])}>Retry</Button></div>}
    {!loaded ? <p className="muted" role="status">{error ? "" : "Reading downloads…"}</p>
      : rows.length === 0 ? <Empty title="Nothing downloading">Model downloads and engine installs show here. <button className="link-button" onClick={onSearch}>Find a model</button></Empty>
      : <ul className="download-list" aria-label="Download list">{rows.map((row) => <li key={row.key} className="download-row" aria-label={row.title}>
        <div className="row-heading"><div className="download-title"><strong className="break-word">{row.title}</strong><span className="muted">{row.detail}</span></div><Badge tone={tone(row.state)}>{stateNames[row.state] ?? row.state}</Badge></div>
        {!["complete", "cancelled"].includes(row.state) && <progress aria-label={`${row.title} progress`} max={row.total ?? 1} value={row.total ? Math.min(row.done, row.total) : undefined} />}
        <div className="row-heading"><span className="muted numeric-left">{row.state === "complete" ? bytes(row.total ?? row.done) : `${bytes(row.done)} of ${row.total ? bytes(row.total) : "unknown"}`}</span>
          <div className="action-row">{actions(row).map((action) => <Button key={action} variant={action === "cancel" ? "danger" : "outline"} disabled={!online || busy === row.key || (inventory.data?.source_policy === "offline" && ["resume", "retry"].includes(action))} onClick={() => void act(row, action)}>{actionNames[action]}</Button>)}</div></div>
        {row.error && <p className="inline-warning">{failure(row.error)}</p>}
      </li>)}</ul>}
    {leftovers.length > 0 && <p className="field-help download-leftovers">{leftovers.length} unfinished {leftovers.length === 1 ? "file holds" : "files hold"} {bytes(leftovers.reduce((sum, file) => sum + file.reserved_bytes, 0))}. <HelpTip label="Unfinished files">Partial files left after an interrupted download. Their space stays reserved until Juiced can check them.</HelpTip></p>}
  </section>;
}
