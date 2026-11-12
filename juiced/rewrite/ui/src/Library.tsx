import { useEffect, useState } from "react";
import type { Client } from "./api";
import { bytes, policyNames, type Model } from "./domain";
import { downloadRequest, firstInventoryPages, inventoryPath, jobActions, nextInventoryPage, parseArtifactInventory, previousInventoryPage, type Artifact, type ArtifactInventory, type InventoryCursors, type InventorySection } from "./management";
import { Badge, Button, Empty } from "./ui";
import { useOperation, useResource } from "./useResource";
import { Registration } from "./Registration";
import { ArtifactSets, referenceSummary } from "./ArtifactSets";
import { parseArtifactSets, primaryArtifactSet, type ArtifactSet } from "./artifactSetData";

function jobLabel(url: string | null, id: string) {
  try { return decodeURIComponent(new URL(url!).pathname.split("/").filter(Boolean).at(-1) || id); } catch { return id; }
}
function jobFailure(code: string) {
  const messages: Record<string, string> = {
    storage_root_unavailable: "A claimed storage folder is unavailable. Restore its original location before retrying.",
    artifact_network: "The source did not complete the download. Retry when it is reachable.",
    artifact_hash_mismatch: "The file did not match the publisher checksum. Review the source and checksum before starting a new download.",
    artifact_size_mismatch: "The received size differed from the expected size or exceeded the ceiling.",
    artifact_source_changed: "The source changed during resume. Retry starts a fresh transfer.",
    artifact_source_policy: "The source or redirect is not permitted by the service's source policy.",
    artifact_publication_uncertain: "Publication is uncertain. Storage stays reserved for operator inspection.",
    artifact_persistence: "The storage change did not establish full durability. Refresh before retrying.",
  };
  return messages[code] ?? "The job could not complete. Refresh the job or inspect the service diagnostics.";
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

export function Library({ client, online, models, refreshModels, onInspect }: { client: Client; online: boolean; models: Model[]; refreshModels: () => Promise<void>; onInspect: (id: string) => void }) {
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
  const unavailableReason = !artifactAvailable ? "Weights removed. Draft kept. Close registration and choose a current file." : !setAvailable ? "Set removed. Draft kept. Close registration and choose a current set." : undefined;
  const pager = (section: InventorySection) => <Pager section={section} data={data} cursors={cursors} loading={inventory.loading} onChange={setCursors} />;
  async function createSet(artifact: Artifact) {
    if (!data) return;
    try {
      const result = await operation.run<ArtifactSet>("juiced/artifacts/sets", { expected_generation: data.generation, specification: primaryArtifactSet(artifact) }, "Model set recorded. Register its engine explicitly.");
      if (result) { parseArtifactSets([result], data.artifacts); setRegisterId(artifact.id); setRegisterSet(result.id); }
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function acquire() {
    if (!data) return;
    try {
      const download = downloadRequest(url, sha, expected, ceiling);
      await operation.run("juiced/artifacts/pull", { download, expected_generation: data.generation }, "Download accepted. Follow its validation and publication below.");
    } catch (error) { operation.fail((error as Error).message); }
  }
  return <section className="content-pane management-pane">
    <div className="section-heading"><h2>Managed weights</h2><Button disabled={inventory.loading} onClick={() => void inventory.reload()}>Refresh library</Button></div>
    <p className="muted">Download unchanged weight files, verify their checksum, then register an engine. Removing a registration retains its weights.</p>
    {inventory.error && <p role="alert" className="inline-error">{data ? "Showing the last observation. " : ""}{inventory.error}</p>}
    {inventory.error && paged && <div className="action-row wrap"><p className="inline-warning">The library changed while you were paging. Return to the first pages to read the current inventory.</p><Button onClick={() => setCursors(firstInventoryPages())}>Return to first pages</Button></div>}
    {operation.error && <p role="alert" className="inline-error">{operation.error}</p>}
    {operation.notice && <p role="status" className="inline-success">{operation.notice}</p>}
    <form className="management-section" aria-label="Acquire weights" onSubmit={(event) => { event.preventDefault(); void acquire(); }}>
      <h3>Acquire weights</h3><p className="field-help">Public HTTPS sources only by default. Direct links must contain no credentials or query. Signed redirects stay private to the service.</p>
      <div className="form-grid"><div className="form-wide"><label htmlFor="download-url">Download URL</label><input id="download-url" type="url" value={url} disabled={operation.busy} onChange={(event) => setUrl(event.target.value)} placeholder="https://publisher.example/model.gguf" /></div>
      <div className="form-wide"><label htmlFor="download-sha">Publisher SHA256</label><input id="download-sha" value={sha} disabled={operation.busy} onChange={(event) => setSha(event.target.value)} autoComplete="off" spellCheck={false} /></div>
      <div><label htmlFor="download-size">Expected size in bytes</label><input id="download-size" inputMode="numeric" value={expected} disabled={operation.busy} onChange={(event) => setExpected(event.target.value)} placeholder="Optional if unknown" /></div><div><label htmlFor="download-ceiling">Storage ceiling in bytes</label><input id="download-ceiling" inputMode="numeric" value={ceiling} disabled={operation.busy} onChange={(event) => setCeiling(event.target.value)} /></div></div>
      <div className="form-actions"><p className="field-help">The ceiling is reserved before writing, including when the size is unknown. Source policy: {data?.source_policy ?? "loading"}.</p><Button variant="primary" type="submit" disabled={blocked || data?.source_policy === "offline"}>Start download</Button></div>
    </form>
    <section className="management-section" aria-label="Download jobs"><div className="section-heading"><h3>Download jobs</h3><span className="muted">{data ? `${bytes(data.charged_bytes)} retained or reserved` : "Reading jobs…"}</span></div>
      {pager("jobs")}
      {data?.jobs.length === 0 && <Empty title="No download jobs">Start a bounded download above. Only complete, validated files become available for registration.</Empty>}
      {data?.jobs.map((job) => <article key={job.id} className="management-row" aria-label={`Download job ${job.id}`}><div className="row-heading"><strong className="break-word">{jobLabel(job.source_url, job.id)}</strong><Badge tone={job.state === "complete" ? "good" : ["failed", "quarantined"].includes(job.state) ? "bad" : "quiet"}>{job.state}</Badge></div><p className="field-help break-word">Job {job.id}</p><progress aria-label={`Download progress ${job.id}`} max={job.expected_bytes ?? job.maximum_bytes} value={job.downloaded_bytes} /><div className="row-heading"><span className="muted">{bytes(job.downloaded_bytes)} of {bytes(job.expected_bytes ?? job.maximum_bytes)}{job.expected_bytes === null ? " ceiling" : ""}</span><div className="action-row">{jobActions(job).map((action) => <Button key={action} variant={action === "cancel" ? "danger" : "outline"} disabled={blocked || (data.source_policy === "offline" && ["resume", "retry"].includes(action))} onClick={() => void operation.run(`juiced/artifacts/jobs/${encodeURIComponent(job.id)}`, { action, expected_revision: job.revision }, action === "forget" ? "Dismissed the settled job. Weight files are retained." : `${action[0]!.toUpperCase() + action.slice(1)} requested. Refresh observes the settled writer state.`)}>{action === "forget" ? "Dismiss job" : action[0]!.toUpperCase() + action.slice(1)}</Button>)}</div></div>{job.error && <p className="inline-warning">{jobFailure(job.error)} Storage remains reserved until its state is resolved.</p>}</article>)}
    </section>
    {!!data?.quarantined_files?.length && <section className="management-section" aria-label="Retained files awaiting inspection"><h3>Retained files awaiting inspection</h3><p className="field-help">These files have no established job or artifact identity. Their storage reservations remain charged. They cannot be registered or removed from this console.</p>{pager("quarantined_files")}{data.quarantined_files.map((file) => <div className="management-row" key={file.filename}><code className="break-word">{file.filename}</code><p className="field-help">{bytes(file.reserved_bytes)} reserved</p></div>)}</section>}
    <section className="management-section" aria-label="Available weight files"><h3>Available weight files</h3>{pager("artifacts")}{data?.artifacts.length === 0 && <Empty title="No validated weight files">A successful download appears here after its full size and checksum pass validation.</Empty>}
      {data?.artifacts.map((item) => <article className="management-row" key={item.id} aria-label={`Weight file ${item.id}`}><div className="row-heading"><code className="break-word">{item.sha256}</code><Badge tone={item.removing ? "warning" : "good"}>{item.removing ? "Removal unsettled" : "Validated file"}</Badge></div><p className="field-help">{bytes(item.bytes)} · {item.active_holds} active file holds</p>{item.reference_count > 0 && <p className="field-help break-word">{referenceSummary(item.references, item.reference_count)}</p>}<div className="action-row wrap"><Button disabled={blocked || item.removing} onClick={() => { setRegisterSet(""); setRegisterId(item.id); }}>Register model</Button><Button disabled={blocked || item.removing} onClick={() => void createSet(item)}>Create model set</Button><Button variant="danger" disabled={blocked || item.removing || item.active_holds > 0 || item.reference_count > 0} onClick={() => void operation.run("juiced/artifacts/remove", { artifact: item.id, expected_generation: data.generation }, "Removed the unreferenced weight file.")}>Remove weights</Button></div>{(item.reference_count > 0 || item.active_holds > 0) && <p className="field-help">Remove model registrations and sets, and release active holds before deleting these weights.</p>}</article>)}
    </section>
    {data && <ArtifactSets sets={data.artifact_sets} total={data.pages.artifact_sets?.total ?? data.artifact_sets.length} pager={pager("artifact_sets")} disabled={blocked} onRegister={(artifact, set) => { setRegisterId(artifact); setRegisterSet(set.id); }} onRemove={(set) => void operation.run("juiced/artifacts/sets/remove", {set: set.id, expected_generation: data.generation}, "Removed the set. Member files are retained.")} />}
    {registerId && <Registration key={`${registerId}:${registerSet}`} artifactId={registerId} artifactSet={registerSet || undefined} unavailableReason={unavailableReason} client={client} disabled={blocked || !inventory.current || !!unavailableReason} onClose={() => { setRegisterId(""); setRegisterSet(""); }} onRegistered={async () => { await inventory.reload(); await refreshModels(); }} />}
    <section className="management-section" aria-label="Registered library models"><h3>Registered models</h3>{models.length === 0 ? <p className="field-help">Register a validated weight file and engine above.</p> : models.map((model) => <div className="management-row row-heading" key={model.id}><div><strong className="break-word">{model.id}</strong><p className="field-help">{policyNames[model.policy]} · {model.state}</p>{model.artifact_set && <p className="field-help break-word">Set: {model.artifact_set}</p>}</div><Button onClick={() => onInspect(model.id)}>Manage model</Button></div>)}</section>
  </section>;
}

