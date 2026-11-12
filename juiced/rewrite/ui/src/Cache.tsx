import { useState } from "react";
import { chatTestTokenLimit, type Client } from "./api";
import { ConversationCache, size } from "./CacheSessions";
import { operationEligible, type Model } from "./domain";
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
  return <div className="page-body cache-page">
    <ConversationCache client={client} online={online} />
    <ManualSnapshots client={client} models={models} online={online} />
  </div>;
}

function ManualSnapshots({ client, models, online }: { client: Client; models: Model[]; online: boolean }) {
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
      const result = await operation.run<{ choices?: { text?: string }[] }>("completions", { model, prompt, max_tokens: Math.min(16, chatTestTokenLimit(selected)), stream: false }, "Prompt processed. You can save the snapshot now.", opaque);
      if (result) { setContextReady(true); setResponse(result.choices?.[0]?.text ?? "The worker returned no text content."); }
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function save() {
    if (!data) return;
    try {
      const saved = await operation.run("juiced/cache/save", { model, slot: 0, session: sessionValue(session), expected_generation: data.generation, maximum_bytes: positiveBytes(ceiling, "Size limit") }, "Snapshot saved.");
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
      const result = await operation.run<{ recovered: boolean; journal_cleanup_pending: boolean }>("juiced/cache/recover", { snapshot: id, session: sessionValue(session), expected_revision: revision, ...(reader ? { reader } : {}) }, reader ? "Restore protection cleared. The snapshot is kept." : "Freed the unfinished snapshot's space.");
      if (result?.journal_cleanup_pending) operation.fail(`${reader ? "Restore protection cleared." : "Snapshot removed."} A cleanup note remains; run a storage check before retrying.`);
      setInspection(null);
    } catch (error) { operation.fail((error as Error).message); }
  }
  async function inspect(target: Cursor[] = [{}]) {
    setInspectionStale(true);
    const result = await operation.run<unknown>(pagePath("juiced/cache/inspect", target.at(-1)!), undefined, "File check finished.");
    if (result !== undefined) {
      try { setInspection(parseCacheInspection(result)); setInspectionPages(target); setInspectionStale(false); }
      catch (error) { operation.fail((error as Error).message); }
    }
  }
  const snapshots = (data?.data ?? []).filter((snapshot) => !snapshot.protocol.startsWith("auto."));
  const shown = !!data && (data.enabled || snapshots.length > 0 || pages.length > 1 || !!data.page?.next_after);
  const problems = inspection?.snapshots.filter((item) => item.checksum_valid === false || item.issues.length > 0) ?? [];
  const untracked = inspection ? inspection.orphan_page?.observed_count ?? inspection.orphan_files.length : 0;
  return <section className="manual-snapshots" aria-label="Manual snapshots">
    <div className="toolbar">
      <h2 className="toolbar-title">Manual snapshots</h2>
      <HelpTip label="Manual snapshots">An API client can save a model's slot under its own session ID with <code>POST /v1/juiced/cache/save</code> and bring it back later with <code>/v1/juiced/cache/restore</code>. Saved conversations above don't need this; they're saved automatically. The switch only allows or blocks manual save and restore. Forget and recover always work. Needs a llama.cpp model with one slot. Snapshots made with other API keys aren't listed.</HelpTip>
      {data && <span className="muted">{snapshots.length} saved{snapshots.length ? ` · ${size(snapshots.reduce((total, snapshot) => total + (snapshot.bytes ?? snapshot.reserved_bytes), 0))}` : ""}</span>}
      <span className="spacer" />
      <Button variant="ghost" disabled={blocked} title="Check every saved file, including saved conversations, for damage and untracked files." onClick={() => void inspect()}>Check files</Button>
      <Switch checked={data?.enabled === true} label="Allow manual save and restore" disabled={blocked} onChange={() => data && void operation.run("juiced/cache", { enabled: !data.enabled, expected_generation: data.configuration_generation }, data.enabled ? "Manual save and restore turned off. Saved snapshots are kept." : "Manual save and restore turned on.")}>{!data ? "Unavailable" : "Allow save and restore"}</Switch>
    </div>
    {inventory.error && <div className="action-row"><p role="alert" className="inline-error">{data ? "Showing the last observation. " : ""}{inventory.error}</p><Button disabled={operation.busy || inventory.loading} onClick={() => void refreshFirstPage()}>{pages.length > 1 ? "Restart snapshot pages" : "Refresh snapshots"}</Button></div>}
    {operation.error && <p role="alert" className="inline-error">{operation.error}</p>}
    {shown && <div className="toolbar" role="group" aria-label="Snapshot session">
      <select aria-label="Snapshot model" value={model} disabled={operation.busy} onChange={(event) => { setModel(event.target.value); setContextReady(false); setResponse(""); }}><option value="">Choose a model…</option>{candidates.map((item) => <option key={item.id} value={item.id}>{item.id}</option>)}</select>
      <input aria-label="Session ID" placeholder="Session ID" value={session} autoComplete="off" spellCheck={false} disabled={operation.busy} title="Use the same ID to restore or forget. This tab never stores it." onChange={(event) => { setSession(event.target.value); setContextReady(false); setResponse(""); }} />
      <Button disabled={operation.busy} onClick={() => { setSession(crypto.randomUUID()); setContextReady(false); setResponse(""); }}>New ID</Button>
      <Button disabled={contextBlocked} onClick={() => setSaving(true)}>Save a snapshot</Button>
    </div>}
    {shown && (pages.length > 1 || data?.page?.next_after) && <div className="action-row cache-pagination" aria-label="Snapshot pages"><Button disabled={operation.busy || inventory.loading || pages.length <= 1} onClick={() => setPages(pages.slice(0, -1))}>Previous snapshot page</Button><span className="muted">Page {pages.length}</span><Button disabled={blocked || inventory.loading || !data?.page?.next_after} onClick={() => data?.page?.next_after && setPages([...pages, { after:data.page.next_after, expected_generation:data.generation, expected_view:data.page.view }])}>Next snapshot page</Button></div>}
    {inventory.error && pages.length > 1 && <p className="inline-warning">{data ? "Showing an older page." : "This page is unavailable."}</p>}
    {snapshots.length > 0 && <div className="data-table snapshot-table" role="table" aria-label="Snapshots">
      {snapshots.map((snapshot) => <article className="data-row" role="row" key={snapshot.id} aria-label={`Snapshot ${snapshot.id}`}>
        <span role="cell" className="snapshot-id"><code className="break-word" title={`Session fingerprint ${snapshot.session_digest}`}>{snapshot.id.slice(0, 12)}</code><span className="muted">{snapshot.protocol}{snapshot.active_readers > 0 ? ` · ${snapshot.active_readers} restoring` : ""}</span></span>
        <span role="cell">{snapshot.state === "saved" ? <Badge tone="good">Saved</Badge> : <Badge tone="warning">Incomplete</Badge>}</span>
        <span role="cell" className="numeric muted">{size(snapshot.bytes ?? snapshot.reserved_bytes)}{snapshot.bytes === null ? " reserved" : ""}</span>
        <span role="cell" className="row-end action-row">
          {snapshot.state === "saved" && <><Button disabled={contextBlocked || (snapshot.retained_readers?.length ?? 0) > 0} onClick={() => void restore(snapshot.id)}>Restore</Button><Button variant="ghost" className="quiet-danger" disabled={blocked || !session || snapshot.active_readers > 0 || (snapshot.retained_readers?.length ?? 0) > 0} onClick={() => void forget(snapshot.id, snapshot.revision)}>Forget</Button></>}
          {snapshot.state !== "saved" && snapshot.writer_bound === true && <Button variant="danger" disabled={blocked || !session || snapshot.active_readers > 0} title="Return the space this unfinished save reserved." onClick={() => void recover(snapshot.id, snapshot.revision)}>Free reserved space</Button>}
          {snapshot.retained_readers?.map((reader) => <Button key={reader.id} disabled={blocked || !session || snapshot.active_readers > 0} title={reader.release_eligible ? "A finished restore left a cleanup note." : "An earlier restore still protects this snapshot."} onClick={() => void recover(snapshot.id, reader.revision, reader.id)}>Clear restore protection</Button>)}
        </span>
      </article>)}
    </div>}
    {inspection && <div className="panel inspection-result" role="region" aria-label="File check">
      <header className="panel-head"><h3>File check</h3><span className="muted">{inspection.snapshots.length} {inspection.snapshots.length === 1 ? "file" : "files"} checked · {size(inspection.charged_bytes)} used or reserved · {untracked} untracked {untracked === 1 ? "file" : "files"}{inspection.orphan_page?.scan_complete === false ? " (scan incomplete)" : ""}</span></header>
      {inspectionStale && <p className="inline-warning">Showing the previous check.</p>}
      {(inspectionPages.length > 1 || inspection.page?.next_after) && <div className="action-row wrap cache-pagination" aria-label="Check pages"><Button disabled={!online || operation.busy || inspectionPages.length <= 1} onClick={() => void inspect(inspectionPages.slice(0, -1))}>Previous check page</Button><span className="muted">Page {inspectionPages.length}</span><Button disabled={!online || operation.busy || inspectionStale || !inspection.page?.next_after} onClick={() => inspection.page?.next_after && void inspect([...inspectionPages, { after:inspection.page.next_after, expected_generation:inspection.generation, expected_view:inspection.page.view }])}>Next check page</Button></div>}
      {problems.length === 0 && inspection.issues.length === 0 ? <p className="inline-success">No problems found.</p> : problems.map((item) => <div className="compact-row two" key={item.snapshot.id}><code className="break-word">{item.snapshot.id.slice(0, 12)}</code><span className={item.checksum_valid === false ? "inline-error" : "muted"}>{item.checksum_valid === false ? "Damaged file" : "Needs attention"}{item.issues.length ? ` · ${item.issues.join(", ")}` : ""}</span></div>)}
      {inspection.issues.map((issue) => <p className="inline-warning" key={issue}>{issue}</p>)}
    </div>}
    {saving && <Dialog title="Save a snapshot" onClose={() => setSaving(false)}>
      <p className="field-help">{model} · session {session.slice(0, 12)}{session.length > 12 ? "…" : ""}</p>
      <label htmlFor="cache-prompt">Prompt</label><textarea id="cache-prompt" rows={4} value={prompt} disabled={operation.busy} onChange={(event) => setPrompt(event.target.value)} />
      <div className="action-row wrap"><Button disabled={contextBlocked || !prompt.trim()} onClick={() => void complete()}>Process prompt</Button>{contextReady && <span className="inline-success">Prompt processed.</span>}</div>
      {response && <pre className="code-block temporary-completion">{response}</pre>}
      <label htmlFor="snapshot-ceiling">Size limit in bytes</label><input id="snapshot-ceiling" value={ceiling} inputMode="numeric" disabled={operation.busy} onChange={(event) => setCeiling(event.target.value)} />
      {operation.error && <p role="alert" className="inline-error">{operation.error}</p>}
      <div className="dialog-actions"><Button onClick={() => setSaving(false)}>Cancel</Button><Button variant="primary" disabled={contextBlocked || !contextReady} onClick={() => void save()}>Save snapshot</Button></div>
    </Dialog>}
  </section>;
}
