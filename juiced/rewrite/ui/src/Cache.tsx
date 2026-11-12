import { useState } from "react";
import { chatTestTokenLimit, type Client } from "./api";
import { operationEligible, bytes, type Model } from "./domain";
import { parseCacheView, parseCacheInspection, positiveBytes, sessionValue, type CacheInspection } from "./management";
import { Badge, Button, Empty } from "./ui";
import { useOperation, useResource } from "./useResource";

type Cursor = { after?: string; expected_generation?: number; expected_view?: string };
function pagePath(base: string, cursor: Cursor) {
  const params = new URLSearchParams({ limit:"128" });
  if (cursor.after !== undefined) params.set("after", cursor.after);
  if (cursor.expected_generation !== undefined) params.set("expected_generation", String(cursor.expected_generation));
  if (cursor.expected_view !== undefined) params.set("expected_view", cursor.expected_view);
  return `${base}?${params}`;
}

export function Cache({ client, models, online }: { client: Client; models: Model[]; online: boolean }) {
  const [pages, setPages] = useState<Cursor[]>([{}]);
  const inventory = useResource(client, pagePath("juiced/cache", pages.at(-1)!), parseCacheView);
  async function refreshFirstPage() { if (pages.length > 1) setPages([{}]); else return inventory.reload(); }
  const operation = useOperation(client, refreshFirstPage);
  const [model, setModel] = useState(""); const [session, setSession] = useState("");
  const [prompt, setPrompt] = useState(""); const [ceiling, setCeiling] = useState("1048576");
  const [contextReady, setContextReady] = useState(false); const [response, setResponse] = useState("");
  const [inspection, setInspection] = useState<CacheInspection | null>(null);
  const [inspectionPages, setInspectionPages] = useState<Cursor[]>([{}]);
  const [inspectionStale, setInspectionStale] = useState(false);
  const data = inventory.data;
  const candidates = models.filter((item) => operationEligible(item, "completions") && item.max_inflight === 1);
  const selected = candidates.find((item) => item.id === model);
  const blocked = !online || !!inventory.error || !data || operation.busy;
  const contextBlocked = blocked || !data?.enabled || !selected || !session;
  async function complete() {
    try {
      if (!selected) throw new Error("Choose an available single-slot completions model.");
      const opaque = sessionValue(session);
      setContextReady(false); setResponse("");
      const result = await operation.run<{ choices?: { text?: string }[] }>("completions", { model, prompt, max_tokens: Math.min(16, chatTestTokenLimit(selected)), stream: false }, "Completed this session's temporary request. Its idle slot can now be saved.", opaque);
      if (result) { setContextReady(true); setResponse(result.choices?.[0]?.text ?? "The worker returned no text content."); }
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function save() {
    if (!data) return;
    try {
      await operation.run("juiced/cache/save", { model, slot: 0, session: sessionValue(session), expected_generation: data.generation, maximum_bytes: positiveBytes(ceiling, "Snapshot ceiling") }, "Saved a validated snapshot. Engine compatibility remains unqualified.");
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function restore(id: string) {
    try {
      const result = await operation.run<{ retained_reader_cleanup_pending?: boolean }>("juiced/cache/restore", { model, slot: 0, session: sessionValue(session), snapshot: id }, "The engine acknowledged restoration for this exact session. Compatibility remains unqualified.");
      if (result) {
        setContextReady(result.retained_reader_cleanup_pending !== true);
        if (result.retained_reader_cleanup_pending) operation.fail("The engine restored this context. A retained restore receipt still needs recovery before another snapshot operation. The saved snapshot is preserved.");
      }
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function forget(id: string, revision: number) {
    try {
      await operation.run("juiced/cache/forget", { snapshot: id, session: sessionValue(session), expected_revision: revision }, "Forgot the snapshot after its references were released.");
      setInspection(null);
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function recover(id: string, revision: number, reader?: string) {
    try {
      const result = await operation.run<{ recovered: boolean; journal_cleanup_pending: boolean }>("juiced/cache/recover", { snapshot: id, session: sessionValue(session), expected_revision: revision, ...(reader ? { reader } : {}) }, reader ? "The service cleared restore protection. The saved snapshot is retained." : "The service confirmed writer exit and recovered the retained snapshot reservation.");
      if (result?.journal_cleanup_pending) operation.fail(`${reader ? "Restore protection settled. The snapshot is retained." : "Snapshot removal completed."} A recovery receipt still needs cleanup. Refresh retained storage before retrying.`);
      setInspection(null);
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function inspect(target: Cursor[] = [{}]) {
    setInspectionStale(true);
    const result = await operation.run<unknown>(pagePath("juiced/cache/inspect", target.at(-1)!), undefined, "Storage inspection completed. No recovery or repair was authorized.");
    if (result !== undefined) {
      try { setInspection(parseCacheInspection(result)); setInspectionPages(target); setInspectionStale(false); }
      catch (error) { operation.fail((error as Error).message); }
    }
  }
  return <section className="content-pane management-pane">
    <div className="section-heading"><h2>Explicit session snapshots</h2><Button disabled={inventory.loading} onClick={() => void refreshFirstPage()}>Refresh snapshots</Button></div><p className="muted">Opt in to saving an idle engine slot. Snapshots are bound to the exact model execution, credential context, protocol and session. Restoration is always explicit.</p>
    {inventory.error && <p role="alert" className="inline-error">{data ? "Showing the last observation. " : ""}{inventory.error}</p>}{operation.error && <p role="alert" className="inline-error">{operation.error}</p>}{operation.notice && <p role="status" className="inline-success">{operation.notice}</p>}
    <section className="management-section"><div className="row-heading snapshot-policy"><div><h3>Persistent snapshot policy</h3><p className="field-help">Disabling prevents new saves and restores. Existing snapshots remain available to inspect or forget.</p></div><div className="action-row"><Badge tone={!data ? "warning" : data.enabled ? "good" : "quiet"}>{!data ? "Unavailable" : data.enabled ? "Enabled" : "Disabled"}</Badge><Button disabled={blocked} onClick={() => data && void operation.run("juiced/cache", { enabled: !data.enabled, expected_generation: data.configuration_generation }, data.enabled ? "Disabled snapshot saves and restores. Existing files are retained." : "Enabled explicit snapshot saves and restores.")}>{!data ? "Snapshot policy unavailable" : data.enabled ? "Disable snapshots" : "Enable snapshots"}</Button></div></div><p className="inline-warning">Engine compatibility is unqualified. This workflow supports completions with one declared slot, slot 0, and an engine slot-save directory. It does not cache answers.</p></section>
    <section className="management-section" aria-label="Snapshot session"><h3>Session context</h3><div className="form-grid"><div className="form-wide"><label htmlFor="cache-model">Snapshot model</label><select id="cache-model" value={model} disabled={operation.busy} onChange={(event) => { setModel(event.target.value); setContextReady(false); setResponse(""); }}><option value="">Choose a single-slot completions model…</option>{candidates.map((item) => <option key={item.id} value={item.id}>{item.id}</option>)}</select><p className="field-help">The engine must also declare its slot-save directory. The service rejects unsupported profiles.</p></div><div className="form-wide"><label htmlFor="cache-session">Opaque session ID</label><div className="copy-row"><input id="cache-session" value={session} autoComplete="off" spellCheck={false} disabled={operation.busy} onChange={(event) => { setSession(event.target.value); setContextReady(false); setResponse(""); }} /><Button disabled={operation.busy} onClick={() => { setSession(crypto.randomUUID()); setContextReady(false); setResponse(""); }}>New session</Button></div><p className="field-help">Use the same identifier to restore, forget or recover. Keep it privately. This tab never saves the identifier or prompt.</p></div><div className="form-wide"><label htmlFor="cache-prompt">Temporary completion prompt</label><textarea id="cache-prompt" rows={3} value={prompt} disabled={operation.busy} onChange={(event) => setPrompt(event.target.value)} /></div></div><div className="form-actions"><p className="field-help">Complete a request under this session before saving its idle slot.</p><Button disabled={contextBlocked || !prompt.trim()} onClick={() => void complete()}>Complete session request</Button></div>{response && <pre className="code-block temporary-completion">{response}</pre>}<div className="form-grid snapshot-save"><div><label htmlFor="snapshot-ceiling">Snapshot ceiling in bytes</label><input id="snapshot-ceiling" value={ceiling} inputMode="numeric" disabled={operation.busy} onChange={(event) => setCeiling(event.target.value)} /></div><div className="form-submit"><Button variant="primary" disabled={contextBlocked || !contextReady} onClick={() => void save()}>Save slot 0 snapshot</Button></div></div></section>
    <section className="management-section" aria-label="Saved snapshots"><div className="section-heading"><h3>Snapshots for this credential context</h3><Badge tone="warning">{data?.qualification ?? "Unqualified"}</Badge></div><div className="action-row wrap cache-pagination" aria-label="Snapshot pages"><Button disabled={operation.busy || inventory.loading || pages.length <= 1} onClick={() => setPages(pages.slice(0, -1))}>Previous snapshot page</Button><span className="muted">Page {pages.length}</span>{inventory.error && <Button disabled={operation.busy || inventory.loading} onClick={() => void refreshFirstPage()}>Restart snapshot pages</Button>}<Button disabled={blocked || inventory.loading || !data?.page?.next_after} onClick={() => data?.page?.next_after && setPages([...pages, { after:data.page.next_after, expected_generation:data.generation, expected_view:data.page.view }])}>Next snapshot page</Button></div>{inventory.error && <p className="inline-warning">{data ? "This snapshot page is a prior observation." : "This snapshot page is unavailable."} Restart snapshot pages to read from page 1.</p>}{data?.data.length === 0 && <Empty title="No snapshots in this credential context">Enable snapshots, complete a session request, then save slot 0. Changing credentials can hide earlier snapshots.</Empty>}{data?.data.map((snapshot) => <article className="management-row" key={snapshot.id} aria-label={`Snapshot ${snapshot.id}`}><div className="row-heading"><code className="break-word">{snapshot.id}</code><Badge tone={snapshot.state === "saved" ? "good" : "warning"}>{snapshot.state}</Badge></div><p className="field-help">{bytes(snapshot.bytes ?? snapshot.reserved_bytes)}{snapshot.bytes === null ? " reserved" : ""} · {snapshot.protocol} · {snapshot.active_readers} active reads</p><p className="field-help break-word">Session digest: {snapshot.session_digest}</p><div className="action-row wrap"><Button disabled={contextBlocked || snapshot.state !== "saved" || (snapshot.retained_readers?.length ?? 0) > 0} onClick={() => void restore(snapshot.id)}>Restore snapshot</Button><Button variant="danger" disabled={blocked || !session || snapshot.state !== "saved" || snapshot.active_readers > 0 || (snapshot.retained_readers?.length ?? 0) > 0} onClick={() => void forget(snapshot.id, snapshot.revision)}>Forget snapshot</Button>{snapshot.state !== "saved" && snapshot.writer_bound === true && <Button variant="danger" disabled={blocked || !session || snapshot.active_readers > 0} onClick={() => void recover(snapshot.id, snapshot.revision)}>Recover retained reservation</Button>}</div>{snapshot.retained_readers?.map((reader) => <div key={reader.id}><p className="inline-warning">{reader.release_eligible ? "A completed restore retains a cleanup receipt. Recovery clears that receipt and preserves the saved snapshot." : "A prior restore retains protection for this saved snapshot. Recovery clears that protection only after confirmed reader exit and preserves the snapshot bytes."}</p><Button disabled={blocked || !session || snapshot.active_readers > 0} onClick={() => void recover(snapshot.id, reader.revision, reader.id)}>Recover restore protection</Button></div>)}{snapshot.state !== "saved" && <p className="inline-warning">Unsettled files stay reserved. Recovery removes this incomplete snapshot only after the service confirms its writer exited. Live, unknown and legacy unbound writers keep their reservations.</p>}</article>)}</section>
    <section className="management-section" aria-label="Retained cache storage"><div className="section-heading"><h3>Retained storage</h3><Button disabled={blocked} onClick={() => void inspect()}>Inspect retained storage</Button></div><p className="field-help">Checks file observations and checksums. This can read large snapshots. Inspection does not repair, delete or authorize recovery.</p>{inspection && <div className="inspection-result">{inspectionStale && <p className="inline-warning">Showing the previous inspection. Inspect from the first page after a failed refresh.</p>}<div className="action-row wrap cache-pagination" aria-label="Inspection pages"><Button disabled={!online || operation.busy || inspectionPages.length <= 1} onClick={() => void inspect(inspectionPages.slice(0, -1))}>Previous inspection page</Button><span className="muted">Page {inspectionPages.length}</span><Button disabled={!online || operation.busy || inspectionStale || !inspection.page?.next_after} onClick={() => inspection.page?.next_after && void inspect([...inspectionPages, { after:inspection.page.next_after, expected_generation:inspection.generation, expected_view:inspection.page.view }])}>Next inspection page</Button></div><p>{bytes(inspection.charged_bytes)} retained or reserved · {inspection.orphan_page?.observed_count ?? inspection.orphan_files.length} observed untracked files</p>{inspection.orphan_page?.scan_complete === false && <p className="inline-warning">The untracked-file scan is incomplete. Its observed count is not a total.</p>}{inspection.snapshots.map((item) => <div className="management-row" key={item.snapshot.id}><code className="break-word">{item.snapshot.id}</code><p className="field-help">{item.checksum_valid === true ? "Checksum valid" : item.checksum_valid === false ? "Checksum mismatch" : "Checksum unavailable"}{item.issues.length ? ` · ${item.issues.join(", ")}` : ""}</p></div>)}{inspection.issues.map((issue) => <p className="inline-warning" key={issue}>{issue}</p>)}</div>}</section>
  </section>;
}
