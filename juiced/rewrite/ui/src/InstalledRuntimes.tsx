import { useState } from "react";
import type { Client } from "./api";
import { parseRuntimeImportReceipt, parseRuntimeInventory, runtimeImportRequest, type RuntimeImportReceipt } from "./runtimePackages";
import { useOperation, useResource } from "./useResource";
import { Button, Dialog, Icon } from "./ui";

export function InstalledRuntimes({ client, online, paused }: { client: Client; online: boolean; paused: boolean }) {
  const [importing, setImporting] = useState(false);
  const inventory = useResource(client, "juiced/runtime-packages", parseRuntimeInventory);
  const operation = useOperation(client, inventory.reload);
  const [draft, setDraft] = useState<{ path: string; generation: number | null }>({ path: "", generation: null });
  const [receipt, setReceipt] = useState<RuntimeImportReceipt | null>(null);
  const data = inventory.data;
  const stale = !!draft.path && !!data && draft.generation !== data.generation;
  const blocked = !online || paused || operation.busy || inventory.loading || !!inventory.error || !data || data.installation_active !== false || stale;
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
  const importer = <>
    <Button variant="ghost" onClick={() => setImporting(true)}><Icon name="upload" size={14} />Import a local engine bundle</Button>
    {importing && <Dialog title="Import engine bundle" onClose={() => setImporting(false)}>
      {data && data.installation_active === undefined && <p className="field-help">This service can't import packages.</p>}
      {paused && <p className="field-help">Resume new requests before importing.</p>}
      <form onSubmit={(event) => { event.preventDefault(); void install(); }}>
        <label htmlFor="runtime-specification">Bundle specification on service host</label>
        <input id="runtime-specification" value={draft.path} maxLength={4096} placeholder="/path/to/runtime-import.json" autoComplete="off" spellCheck={false} disabled={!online || operation.busy || !data} onChange={(event) => { const path = event.target.value; setDraft((current) => ({ path, generation: current.path ? current.generation : data?.generation ?? null })); setReceipt(null); }} />
        <p className="field-help">An absolute path on the service host.</p>
        {stale && <div className="action-row wrap"><p className="inline-warning">Installed engines changed since you started. Your path is kept.</p><Button onClick={() => data && setDraft((current) => ({ ...current, generation: data.generation }))}>Use the latest list</Button></div>}
        {feedback}
        <div className="action-row wrap"><Button disabled={operation.busy || !draft.path} onClick={() => { setDraft({ path: "", generation: null }); setReceipt(null); operation.fail(""); }}>Clear import draft</Button><Button type="submit" variant="primary" disabled={blocked || !draft.path}>{operation.busy ? "Importing…" : "Import local runtime"}</Button></div>
      </form>
    </Dialog>}
  </>;
  return <div className="runtime-packages" aria-label="Local engine bundles" role="region">{!importing && feedback}{importer}</div>;
}
