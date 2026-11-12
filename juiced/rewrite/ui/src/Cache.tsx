import { useState } from "react";
import { chatTestTokenLimit, type Client } from "./api";
import { operationEligible, bytes, type Model } from "./domain";
import { parseCacheView, parseCacheInspection, positiveBytes, sessionValue, type CacheInspection } from "./management";
import { Badge, Button, Dialog, Empty, HelpTip, Switch } from "./ui";
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
  const [saving, setSaving] = useState(false);
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
      const result = await operation.run<{ choices?: { text?: string }[] }>("completions", { model, prompt, max_tokens: Math.min(16, chatTestTokenLimit(selected)), stream: false }, "Prompt processed. You can save the slot now.", opaque);
      if (result) { setContextReady(true); setResponse(result.choices?.[0]?.text ?? "The worker returned no text content."); }
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function save() {
    if (!data) return;
    try {
      const saved = await operation.run("juiced/cache/save", { model, slot: 0, session: sessionValue(session), expected_generation: data.generation, maximum_bytes: positiveBytes(ceiling, "Snapshot ceiling") }, "Snapshot saved.");
      if (saved !== undefined) setSaving(false);
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function restore(id: string) {
    try {
      const result = await operation.run<{ retained_reader_cleanup_pending?: boolean }>("juiced/cache/restore", { model, slot: 0, session: sessionValue(session), snapshot: id }, "Snapshot restored.");
      if (result) {
        setContextReady(result.retained_reader_cleanup_pending !== true);
        if (result.retained_reader_cleanup_pending) operation.fail("Snapshot restored. Clear its restore protection before the next snapshot operation.");
      }
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function forget(id: string, revision: number) {
    try {
      await operation.run("juiced/cache/forget", { snapshot: id, session: sessionValue(session), expected_revision: revision }, "Snapshot forgotten.");
      setInspection(null);
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function recover(id: string, revision: number, reader?: string) {
    try {
      const result = await operation.run<{ recovered: boolean; journal_cleanup_pending: boolean }>("juiced/cache/recover", { snapshot: id, session: sessionValue(session), expected_revision: revision, ...(reader ? { reader } : {}) }, reader ? "Restore protection cleared. The snapshot is kept." : "Recovered the incomplete snapshot's space.");
      if (result?.journal_cleanup_pending) operation.fail(`${reader ? "Restore protection cleared." : "Snapshot removed."} A cleanup note remains; run a storage check before retrying.`);
      setInspection(null);
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function inspect(target: Cursor[] = [{}]) {
    setInspectionStale(true);
    const result = await operation.run<unknown>(pagePath("juiced/cache/inspect", target.at(-1)!), undefined, "Storage check finished.");
    if (result !== undefined) {
      try { setInspection(parseCacheInspection(result)); setInspectionPages(target); setInspectionStale(false); }
      catch (error) { operation.fail((error as Error).message); }
    }
  }
  const snapshots = data?.data ?? [];
  const used = snapshots.reduce((total, snapshot) => total + (snapshot.bytes ?? snapshot.reserved_bytes), 0);
  return <div className="page-body cache-page">
    {inventory.error && <div className="action-row"><p role="alert" className="inline-error">{data ? "Showing the last observation. " : ""}{inventory.error}</p><Button disabled={operation.busy || inventory.loading} onClick={() => void refreshFirstPage()}>{pages.length > 1 ? "Restart snapshot pages" : "Refresh snapshots"}</Button></div>}
    {operation.error && <p role="alert" className="inline-error">{operation.error}</p>}
    <section className="panel cache-summary" aria-label="Prompt snapshots">
      <div><h2>Prompt snapshots</h2><span className="muted">{!data ? "Unavailable" : `${snapshots.length} saved · ${bytes(used)}`}</span></div>
      <HelpTip label="Prompt snapshots">A snapshot saves an engine slot's processed prompt for one session, so restoring it skips reprocessing. Works with llama.cpp completions models that have one slot and a slot-save folder. Snapshots made with other API keys aren't listed.</HelpTip>
      <span className="spacer" />
      <Button disabled={blocked} onClick={() => void inspect()}>Check storage</Button>
      <Switch checked={data?.enabled === true} label="Snapshots" disabled={blocked} onChange={() => data && void operation.run("juiced/cache", { enabled: !data.enabled, expected_generation: data.configuration_generation }, data.enabled ? "Snapshots turned off. Saved files are kept." : "Snapshots turned on.")}>{!data ? "Unavailable" : data.enabled ? "On" : "Off"}</Switch>
    </section>
    <section className="toolbar" aria-label="Snapshot session">
      <select aria-label="Snapshot model" value={model} disabled={operation.busy} onChange={(event) => { setModel(event.target.value); setContextReady(false); setResponse(""); }}><option value="">Choose a model…</option>{candidates.map((item) => <option key={item.id} value={item.id}>{item.id}</option>)}</select>
      <input aria-label="Session ID" placeholder="Session ID" value={session} autoComplete="off" spellCheck={false} disabled={operation.busy} title="Use the same ID to restore or forget. This tab never stores it." onChange={(event) => { setSession(event.target.value); setContextReady(false); setResponse(""); }} />
      <Button disabled={operation.busy} onClick={() => { setSession(crypto.randomUUID()); setContextReady(false); setResponse(""); }}>New session</Button>
      <Button variant="primary" disabled={contextBlocked} onClick={() => setSaving(true)}>Save a snapshot</Button>
    </section>
    <section aria-label="Saved snapshots">
      {(pages.length > 1 || data?.page?.next_after) && <div className="action-row cache-pagination" aria-label="Snapshot pages"><Button disabled={operation.busy || inventory.loading || pages.length <= 1} onClick={() => setPages(pages.slice(0, -1))}>Previous snapshot page</Button><span className="muted">Page {pages.length}</span><Button disabled={blocked || inventory.loading || !data?.page?.next_after} onClick={() => data?.page?.next_after && setPages([...pages, { after:data.page.next_after, expected_generation:data.generation, expected_view:data.page.view }])}>Next snapshot page</Button></div>}
      {inventory.error && pages.length > 1 && <p className="inline-warning">{data ? "Showing an older page." : "This page is unavailable."}</p>}
      {data && snapshots.length === 0 ? <Empty title="No snapshots">Choose a model and session, then save a snapshot.</Empty> : snapshots.length > 0 && <div className="data-table snapshot-table" role="table" aria-label="Snapshots">
        {snapshots.map((snapshot) => <article className="data-row" role="row" key={snapshot.id} aria-label={`Snapshot ${snapshot.id}`}>
          <span role="cell" className="snapshot-id"><code className="break-word" title={`Session digest ${snapshot.session_digest}`}>{snapshot.id}</code><span className="muted">{snapshot.protocol}{snapshot.active_readers > 0 ? ` · ${snapshot.active_readers} restoring` : ""}</span></span>
          <span role="cell"><Badge tone={snapshot.state === "saved" ? "good" : "warning"}>{snapshot.state}</Badge></span>
          <span role="cell" className="numeric muted">{bytes(snapshot.bytes ?? snapshot.reserved_bytes)}{snapshot.bytes === null ? " reserved" : ""}</span>
          <span role="cell" className="row-end action-row">
            {snapshot.state === "saved" && <><Button disabled={contextBlocked || (snapshot.retained_readers?.length ?? 0) > 0} onClick={() => void restore(snapshot.id)}>Restore snapshot</Button><Button variant="danger" disabled={blocked || !session || snapshot.active_readers > 0 || (snapshot.retained_readers?.length ?? 0) > 0} onClick={() => void forget(snapshot.id, snapshot.revision)}>Forget snapshot</Button></>}
            {snapshot.state !== "saved" && snapshot.writer_bound === true && <Button variant="danger" disabled={blocked || !session || snapshot.active_readers > 0} onClick={() => void recover(snapshot.id, snapshot.revision)}>Recover retained reservation</Button>}
            {snapshot.retained_readers?.map((reader) => <Button key={reader.id} disabled={blocked || !session || snapshot.active_readers > 0} title={reader.release_eligible ? "A finished restore left a cleanup note." : "An earlier restore still protects this snapshot."} onClick={() => void recover(snapshot.id, reader.revision, reader.id)}>Recover restore protection</Button>)}
          </span>
        </article>)}
      </div>}
    </section>
    {inspection && <section className="panel inspection-result" aria-label="Retained cache storage">
      <header className="panel-head"><h3>Storage check</h3><span className="muted">{bytes(inspection.charged_bytes)} used or reserved · {inspection.orphan_page?.observed_count ?? inspection.orphan_files.length} untracked files{inspection.orphan_page?.scan_complete === false ? " (scan incomplete)" : ""}</span></header>
      {inspectionStale && <p className="inline-warning">Showing the previous check.</p>}
      {(inspectionPages.length > 1 || inspection.page?.next_after) && <div className="action-row wrap cache-pagination" aria-label="Inspection pages"><Button disabled={!online || operation.busy || inspectionPages.length <= 1} onClick={() => void inspect(inspectionPages.slice(0, -1))}>Previous inspection page</Button><span className="muted">Page {inspectionPages.length}</span><Button disabled={!online || operation.busy || inspectionStale || !inspection.page?.next_after} onClick={() => inspection.page?.next_after && void inspect([...inspectionPages, { after:inspection.page.next_after, expected_generation:inspection.generation, expected_view:inspection.page.view }])}>Next inspection page</Button></div>}
      {inspection.snapshots.map((item) => <div className="compact-row two" key={item.snapshot.id}><code className="break-word">{item.snapshot.id}</code><span className={item.checksum_valid === false ? "inline-error" : "muted"}>{item.checksum_valid === true ? "Checksum OK" : item.checksum_valid === false ? "Checksum mismatch" : "Not checked"}{item.issues.length ? ` · ${item.issues.join(", ")}` : ""}</span></div>)}
      {inspection.issues.map((issue) => <p className="inline-warning" key={issue}>{issue}</p>)}
    </section>}
    {saving && <Dialog title="Save a snapshot" onClose={() => setSaving(false)}>
      <p className="field-help">{model} · session {session.slice(0, 12)}{session.length > 12 ? "…" : ""}</p>
      <label htmlFor="cache-prompt">Prompt</label><textarea id="cache-prompt" rows={4} value={prompt} disabled={operation.busy} onChange={(event) => setPrompt(event.target.value)} />
      <div className="action-row wrap"><Button disabled={contextBlocked || !prompt.trim()} onClick={() => void complete()}>Complete session request</Button>{contextReady && <span className="inline-success">Prompt processed.</span>}</div>
      {response && <pre className="code-block temporary-completion">{response}</pre>}
      <label htmlFor="snapshot-ceiling">Snapshot ceiling in bytes</label><input id="snapshot-ceiling" value={ceiling} inputMode="numeric" disabled={operation.busy} onChange={(event) => setCeiling(event.target.value)} />
      {operation.error && <p role="alert" className="inline-error">{operation.error}</p>}
      <div className="dialog-actions"><Button onClick={() => setSaving(false)}>Close</Button><Button variant="primary" disabled={contextBlocked || !contextReady} onClick={() => void save()}>Save slot 0 snapshot</Button></div>
    </Dialog>}
  </div>;
}
