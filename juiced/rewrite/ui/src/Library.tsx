import { useEffect, useState } from "react";
import type { Client } from "./api";
import { bytes, type Model } from "./domain";
import { downloadRequest, firstInventoryPages, inventoryPath, jobActions, nextInventoryPage, parseArtifactInventory, previousInventoryPage, type Artifact, type ArtifactInventory, type InventoryCursors, type InventorySection } from "./management";
import { Badge, Button, Dialog, Empty, HelpTip, Icon } from "./ui";
import { useOperation, useResource } from "./useResource";
import { Registration } from "./Registration";
import { ArtifactSets, referenceSummary } from "./ArtifactSets";
import { parseArtifactSets, primaryArtifactSet, type ArtifactSet } from "./artifactSetData";

function jobLabel(url: string | null, id: string) {
  try { return decodeURIComponent(new URL(url!).pathname.split("/").filter(Boolean).at(-1) || id); } catch { return id; }
}
function jobFailure(code: string) {
  const messages: Record<string, string> = {
    storage_root_unavailable: "A storage folder moved or disappeared. Restore it, then retry.",
    artifact_network: "The download didn't finish. Retry when the source is reachable.",
    artifact_hash_mismatch: "The file didn't match the SHA256. Check the source and checksum.",
    artifact_size_mismatch: "The file size didn't match or went over the limit.",
    artifact_source_changed: "The source file changed. Retry starts over.",
    artifact_source_policy: "This source or redirect isn't allowed.",
    artifact_publication_uncertain: "Saving the file may not have finished. Its space stays reserved.",
    artifact_persistence: "The change may not be saved. Refresh before retrying.",
  };
  return messages[code] ?? "The job failed. Check Diagnostics for details.";
}

const sectionNames: Record<InventorySection, string> = { jobs: "job", artifacts: "weight file", artifact_sets: "set", quarantined_files: "retained file" };
function Pager({ section, data, cursors, loading, onChange }: { section: InventorySection; data: ArtifactInventory | null; cursors: InventoryCursors; loading: boolean; onChange: (next: InventoryCursors) => void }) {
  const info = data?.pages[section];
  const depth = cursors.stacks[section].length;
  if (!data || !info || (depth === 0 && !info.next_after)) return null;
  const name = sectionNames[section];
  return <div className="action-row wrap cache-pagination" aria-label={`${name[0]!.toUpperCase() + name.slice(1)} pages`}>
    <Button disabled={loading || depth === 0} onClick={() => onChange(previousInventoryPage(cursors, section))}>Previous {name} page</Button>
    <span className="muted">Page {depth + 1} · {info.total} total</span>
    <Button disabled={loading || !info.next_after} onClick={() => onChange(nextInventoryPage(cursors, data, section))}>Next {name} page</Button>
  </div>;
}

export type LibraryView = "find" | "files";
export function Library({ client, online, view, refreshModels }: { client: Client; online: boolean; view: LibraryView; models: Model[]; refreshModels: () => Promise<void>; onInspect: (id: string) => void }) {
  const [urlOpen, setUrlOpen] = useState(false);
  const [cursors, setCursors] = useState(firstInventoryPages);
  const [registerId, setRegisterId] = useState("");
  const [registerSet, setRegisterSet] = useState("");
  const inventory = useResource(client, inventoryPath(cursors, { artifact: registerId || undefined, artifact_set: registerSet || undefined }), parseArtifactInventory, 2000, true);
  const operation = useOperation(client, inventory.reload);
  const [url, setUrl] = useState("");
  const [sha, setSha] = useState("");
  const [expected, setExpected] = useState("");
  const [ceiling, setCeiling] = useState("");
  useEffect(() => { setRegisterId(""); setRegisterSet(""); setCursors(firstInventoryPages()); }, [client]);
  const data = inventory.data;
  const paged = Object.values(cursors.stacks).some((stack) => stack.length > 0);
  const blocked = !online || !!inventory.error || !data || operation.busy;
  const artifactAvailable = !data || !inventory.current || data.artifacts.some((item) => item.id === registerId) || data.selection.artifact?.id === registerId;
  const setAvailable = !registerSet || !data || !inventory.current || data.artifact_sets.some((set) => set.id === registerSet) || data.selection.artifact_set?.id === registerSet;
  const unavailableReason = !artifactAvailable ? "Weights removed. Your draft is kept; close it and pick another file." : !setAvailable ? "Set removed. Your draft is kept; close it and pick another set." : undefined;
  const pager = (section: InventorySection) => <Pager section={section} data={data} cursors={cursors} loading={inventory.loading} onChange={setCursors} />;
  async function createSet(artifact: Artifact) {
    if (!data) return;
    try {
      const result = await operation.run<ArtifactSet>("juiced/artifacts/sets", { expected_generation: data.generation, specification: primaryArtifactSet(artifact) }, "Model set created. Register an engine for it.");
      if (result) { parseArtifactSets([result], data.artifacts); setRegisterId(artifact.id); setRegisterSet(result.id); }
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function acquire() {
    if (!data) return;
    try {
      const download = downloadRequest(url, sha, expected, ceiling);
      if (await operation.run("juiced/artifacts/pull", { download, expected_generation: data.generation }, "Download started.") !== undefined) setUrlOpen(false);
    } catch (error) { operation.fail((error as Error).message); }
  }
  const jobRows = data?.jobs.map((job) => <article key={job.id} className="item-row" aria-label={`Download job ${job.id}`}>
    <div className="row-heading"><strong className="break-word" title={`Job ${job.id}`}>{jobLabel(job.source_url, job.id)}</strong><Badge tone={job.state === "complete" ? "good" : ["failed", "quarantined"].includes(job.state) ? "bad" : "quiet"}>{job.state}</Badge></div>
    <progress aria-label={`Download progress ${job.id}`} max={job.expected_bytes ?? job.maximum_bytes} value={job.downloaded_bytes} />
    <div className="row-heading"><span className="muted numeric-left">{bytes(job.downloaded_bytes)} / {bytes(job.expected_bytes ?? job.maximum_bytes)}</span><div className="action-row">{jobActions(job).map((action) => <Button key={action} variant={action === "cancel" ? "danger" : "outline"} disabled={blocked || (data.source_policy === "offline" && ["resume", "retry"].includes(action))} onClick={() => void operation.run(`juiced/artifacts/jobs/${encodeURIComponent(job.id)}`, { action, expected_revision: job.revision }, action === "forget" ? "Job dismissed. Weights kept." : `${action[0]!.toUpperCase() + action.slice(1)} requested.`)}>{action === "forget" ? "Dismiss job" : action[0]!.toUpperCase() + action.slice(1)}</Button>)}</div></div>
    {job.error && <p className="inline-warning">{jobFailure(job.error)}</p>}
  </article>);
  return <section className="content-pane management-pane">
    {inventory.error && <p role="alert" className="inline-error">{data ? "Showing the last observation. " : ""}{inventory.error}</p>}
    {inventory.error && paged && <div className="action-row wrap"><p className="inline-warning">The library changed while you were paging.</p><Button onClick={() => setCursors(firstInventoryPages())}>Return to first pages</Button></div>}
    {operation.error && !urlOpen && <p role="alert" className="inline-error">{operation.error}</p>}
    {view === "files" && <div className="action-row"><Button onClick={() => setUrlOpen(true)}>Add from URL</Button></div>}
    {urlOpen && <Dialog title="Add weights from a URL" onClose={() => setUrlOpen(false)}>
      <form aria-label="Acquire weights" onSubmit={(event) => { event.preventDefault(); void acquire(); }}>
        <div className="section-heading"><div className="page-title"><h2>Download weights</h2><HelpTip label="Download weights">Paste a direct file link and the publisher's SHA256. The file only becomes usable after its size and checksum match. Allowed sources: {data?.source_policy ?? "loading"}. Links can't contain credentials or query strings.</HelpTip></div></div>
        <div className="form-grid download-grid">
          <div className="form-wide"><label htmlFor="download-url">Download URL</label><input id="download-url" type="url" value={url} disabled={operation.busy} onChange={(event) => setUrl(event.target.value)} placeholder="https://publisher.example/model.gguf" /></div>
          <div className="form-wide"><label htmlFor="download-sha">Publisher SHA256</label><input id="download-sha" value={sha} disabled={operation.busy} onChange={(event) => setSha(event.target.value)} autoComplete="off" spellCheck={false} /></div>
          <div><label htmlFor="download-size">Expected size in bytes</label><input id="download-size" inputMode="numeric" value={expected} disabled={operation.busy} onChange={(event) => setExpected(event.target.value)} placeholder="Optional" /></div>
          <div><label htmlFor="download-ceiling">Storage ceiling in bytes</label><input id="download-ceiling" inputMode="numeric" value={ceiling} disabled={operation.busy} onChange={(event) => setCeiling(event.target.value)} title="Reserved before the download starts" /></div>
        </div>
        {operation.error && <p role="alert" className="inline-error">{operation.error}</p>}
        <div className="dialog-actions"><Button onClick={() => setUrlOpen(false)}>Cancel</Button><Button variant="primary" type="submit" disabled={blocked || data?.source_policy === "offline"}>Start download</Button></div>
      </form>
    </Dialog>}
    {view === "files" && <>
      <section className="management-section" aria-label="Available weight files"><div className="section-heading"><h2>Weight files</h2>{data && <span className="muted">{bytes(data.charged_bytes)} used or reserved</span>}</div>{pager("artifacts")}{data?.artifacts.length === 0 && <Empty title="No weight files">Downloaded model files show here. Find one under Find models.</Empty>}
        {data?.artifacts.map((item) => <article className="item-row" key={item.id} aria-label={`Weight file ${item.id}`}>
          <div className="row-heading"><code className="break-word digest" title={item.sha256}>{item.sha256}</code><Badge tone={item.removing ? "warning" : "good"}>{item.removing ? "Removing" : "Verified"}</Badge></div>
          <p className="field-help">{bytes(item.bytes)}{item.active_holds > 0 ? ` · ${item.active_holds} in use` : ""}{item.reference_count > 0 ? ` · ${referenceSummary(item.references, item.reference_count)}` : ""}</p>
          <div className="action-row wrap"><Button disabled={blocked || item.removing} onClick={() => { setRegisterSet(""); setRegisterId(item.id); }}>Register model</Button><Button disabled={blocked || item.removing} onClick={() => void createSet(item)}>Create model set</Button><Button variant="danger" disabled={blocked || item.removing || item.active_holds > 0 || item.reference_count > 0} title={item.reference_count > 0 || item.active_holds > 0 ? "Remove its registrations and sets first" : undefined} onClick={() => void operation.run("juiced/artifacts/remove", { artifact: item.id, expected_generation: data.generation }, "Weights removed.")}>Remove weights</Button></div>
        </article>)}
      </section>
      {data && <ArtifactSets sets={data.artifact_sets} total={data.pages.artifact_sets?.total ?? data.artifact_sets.length} pager={pager("artifact_sets")} disabled={blocked} onRegister={(artifact, set) => { setRegisterId(artifact); setRegisterSet(set.id); }} onRemove={(set) => void operation.run("juiced/artifacts/sets/remove", {set: set.id, expected_generation: data.generation}, "Set removed. Its files are kept.")} />}
      {!!data?.jobs.length && <section className="management-section" aria-label="Download jobs"><div className="section-heading"><h2>Jobs</h2></div>
        {pager("jobs")}
        {jobRows}
      </section>}
      {!!data?.quarantined_files?.length && <section className="management-section" aria-label="Retained files awaiting inspection"><div className="section-heading"><div className="page-title"><h2>Unclaimed files</h2><HelpTip label="Unclaimed files">Partial files left after an interrupted download. Their space stays reserved. They can't be registered or removed here.</HelpTip></div></div>{pager("quarantined_files")}{data.quarantined_files.map((file) => <div className="compact-row two" key={file.filename}><code className="break-word">{file.filename}</code><span className="muted">{bytes(file.reserved_bytes)}</span></div>)}</section>}
    </>}
    {registerId && <Registration key={`${registerId}:${registerSet}`} artifactId={registerId} artifactSet={registerSet || undefined} unavailableReason={unavailableReason} client={client} disabled={blocked || !inventory.current || !!unavailableReason} onClose={() => { setRegisterId(""); setRegisterSet(""); }} onRegistered={async () => { await inventory.reload(); await refreshModels(); }} />}
  </section>;
}

