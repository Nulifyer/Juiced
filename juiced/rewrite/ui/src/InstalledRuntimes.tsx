import { useState } from "react";
import type { Client } from "./api";
import { bytes, type RuntimeControlMemory } from "./domain";
import { parseRuntimeImportReceipt, parseRuntimeInventory, runtimeImportRequest, type RuntimeImportReceipt } from "./runtimePackages";
import { useOperation, useResource } from "./useResource";
import { Badge, Button, Dialog, HelpTip, Icon } from "./ui";

export function InstalledRuntimes({ client, online, paused, memory }: { client: Client; online: boolean; paused: boolean; memory?: RuntimeControlMemory }) {
  const [importing, setImporting] = useState(false);
  const inventory = useResource(client, "juiced/runtime-packages", parseRuntimeInventory);
  const operation = useOperation(client, inventory.reload);
  const [draft, setDraft] = useState<{ path: string; generation: number | null }>({ path: "", generation: null });
  const [receipt, setReceipt] = useState<RuntimeImportReceipt | null>(null);
  const data = inventory.data;
  const stale = !!draft.path && !!data && draft.generation !== data.generation;
  const blocked = !online || paused || operation.busy || inventory.loading || !!inventory.error || !data || data.installation_active !== false || stale;
  async function refresh() {
    const fresh = await inventory.reload();
    if (fresh) setDraft((current) => ({ ...current, generation: fresh.generation }));
  }
  async function install() {
    if (blocked || !data) return;
    setReceipt(null);
    try {
      const body = runtimeImportRequest(data, draft.path, draft.generation);
      const result = await operation.run<unknown>("juiced/runtime-packages/import", body, "Runtime installation reported completion.");
      if (result !== undefined) {
        setReceipt(parseRuntimeImportReceipt(result));
        setDraft({ path: "", generation: null });
      }
    } catch (error) { operation.fail((error as Error).message); }
  }
  const feedback = <>
    {operation.error && <p role="alert" className="inline-error">{operation.error} Your path is kept.</p>}
    {receipt && <div role="status" className="inline-success break-word">{receipt.newly_installed ? "Installed" : "Already installed"}: {receipt.package}</div>}
    {(operation.busy || data?.installation_active) && <p role="status" className="field-help">Installing… Leaving this page doesn't cancel it.</p>}
  </>;
  return <section className="runtime-packages management-section" aria-label="Installed runtime packages">
    <div className="section-heading"><div className="page-title"><h2>Installed engines</h2><HelpTip label="Installed engines">Engine packages stored by the service. Pick one for a model in its Settings tab while the model is unloaded.{memory?.state === "accounted" ? ` Package metadata reserves ${bytes(memory.metadata_bytes + memory.binding_bytes)}.` : ""}</HelpTip></div><div className="action-row">{data && <span className="muted">{data.packages.length} installed</span>}<Button variant="ghost" aria-label="Refresh installed runtimes" disabled={!online || operation.busy || inventory.loading} onClick={() => void refresh()}><Icon name="refresh" size={16} /></Button></div></div>
    {inventory.error && <p role="alert" className="inline-error">{data ? "Showing the last observation. " : ""}{inventory.error}</p>}
    {!importing && feedback}
    {!data && !inventory.error && <p className="field-help">Loading…</p>}
    {data?.packages.length === 0 && <p className="field-help">No engine packages installed.</p>}
    {data?.packages.map((item) => <article className="item-row" aria-label={`Installed package ${item.id}`} key={item.id}>
      <div className="row-heading"><strong className="break-word" title={item.id}>{item.implementation_id} · {item.provenance.build_revision}</strong><Badge tone={item.phase === "staging" ? "warning" : "quiet"}>{item.phase === "staging" ? "Incomplete" : "Installed"}</Badge></div>
      <p className="field-help">{bytes(item.reserved_bytes)} · {item.declared_target.operating_system}/{item.declared_target.architecture}{item.binding_leases ? ` · used by ${item.binding_leases}` : ""}{!item.declared_host_match ? " · not for this host" : ""}</p>
      {item.phase === "staging" && <p className="field-help">Import didn't finish. Restore the source, then import it again.</p>}
    </article>)}
    <div className="action-row"><Button onClick={() => setImporting(true)}>Import a local engine bundle</Button></div>
    {importing && <Dialog title="Import engine bundle" onClose={() => setImporting(false)}>
      {data && data.installation_active === undefined && <p className="field-help">This service can't import packages.</p>}
      {paused && <p className="field-help">Resume new requests before importing.</p>}
      <form onSubmit={(event) => { event.preventDefault(); void install(); }}>
        <label htmlFor="runtime-specification">Bundle specification on service host</label>
        <input id="runtime-specification" value={draft.path} maxLength={4096} placeholder="/path/to/runtime-import.json" autoComplete="off" spellCheck={false} disabled={!online || operation.busy || !data} onChange={(event) => { const path = event.target.value; setDraft((current) => ({ path, generation: current.path ? current.generation : data?.generation ?? null })); setReceipt(null); }} />
        <p className="field-help">An absolute path on the service host.</p>
        {stale && <p className="inline-warning">Installed engines changed. Your path is kept; refresh before importing.</p>}
        {feedback}
        <div className="action-row wrap"><Button disabled={operation.busy || !draft.path} onClick={() => { setDraft({ path: "", generation: null }); setReceipt(null); operation.fail(""); }}>Clear import draft</Button><Button type="submit" variant="primary" disabled={blocked || !draft.path}>{operation.busy ? "Importing…" : "Import local runtime"}</Button></div>
      </form>
    </Dialog>}
  </section>;
}
