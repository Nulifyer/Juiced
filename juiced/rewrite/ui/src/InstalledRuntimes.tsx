import { useState } from "react";
import type { Client } from "./api";
import { bytes, type RuntimeControlMemory } from "./domain";
import { parseRuntimeImportReceipt, parseRuntimeInventory, runtimeImportRequest, type RuntimeImportReceipt } from "./runtimePackages";
import { useOperation, useResource } from "./useResource";
import { Badge, Button } from "./ui";

export function InstalledRuntimes({ client, online, paused, memory }: { client: Client; online: boolean; paused: boolean; memory?: RuntimeControlMemory }) {
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
  return <section className="runtime-packages management-section" aria-label="Installed runtime packages">
    <div className="section-heading"><h2>Installed runtimes</h2><Button disabled={!online || operation.busy || inventory.loading} onClick={() => void refresh()}>Refresh installed runtimes</Button></div>
    <p className="field-help">Import an engine bundle already on the service host. Then select it for an unloaded model in Models. Importing preserves model policies and does not start a worker.</p>
    {memory?.state === "accounted" && <p className="field-help">Latest protected runtime reservation: {bytes(memory.metadata_bytes + memory.binding_bytes)} retained, {bytes(memory.scratch_bytes)} in work and results. These claims share the appliance host budget.</p>}
    {inventory.error && <p role="alert" className="inline-error">{data ? "Showing the last package observation. " : ""}{inventory.error}</p>}
    {operation.error && <p role="alert" className="inline-error">{operation.error} Your specification path is kept. Refresh installed runtimes before retrying.</p>}
    {receipt && <div role="status" className="inline-success break-word">{receipt.newly_installed ? "Installed runtime" : "Verified existing runtime"}: {receipt.package}. Model selection and loading policy are unchanged.</div>}
    {(operation.busy || data?.installation_active) && <p role="status" className="field-help">Installation is active on the service. Leaving this page does not cancel accepted work. Refresh to inspect its result.</p>}
    {data && data.installation_active === undefined && <p className="field-help">This service does not report installation ownership. Package review is available. Refresh after upgrading the service to import here.</p>}
    {paused && <p className="field-help">Resume new requests before importing a runtime.</p>}
    <form onSubmit={(event) => { event.preventDefault(); void install(); }}>
      <label htmlFor="runtime-specification">Bundle specification on service host</label>
      <input id="runtime-specification" value={draft.path} maxLength={4096} placeholder="/path/to/runtime-import.json" autoComplete="off" spellCheck={false} disabled={!online || operation.busy || !data} onChange={(event) => { const path = event.target.value; setDraft((current) => ({ path, generation: current.path ? current.generation : data?.generation ?? null })); setReceipt(null); }} />
      <p className="field-help">Use an absolute path on the connected appliance. This form does not upload browser files or download from the network. Protected-memory services require capacity for package metadata, copy work and the result.</p>
      {stale && <p className="inline-warning">The installed runtimes changed. Your path is kept. Review the packages, then refresh installed runtimes to use the current generation.</p>}
      <div className="action-row wrap"><Button disabled={operation.busy || !draft.path} onClick={() => { setDraft({ path: "", generation: null }); setReceipt(null); operation.fail(""); }}>Clear import draft</Button><Button type="submit" variant="primary" disabled={blocked || !draft.path}>{operation.busy ? "Importing runtime…" : "Import local runtime"}</Button></div>
    </form>
    {!data && !inventory.error && <p className="field-help">Reading installed packages…</p>}
    {data && <><p className="field-help">{data.packages.length} packages · Saved generation {data.generation}</p>{data.packages.length === 0 && <p className="field-help">No managed runtimes are installed. Configured external workers are listed separately.</p>}{data.packages.map((item) => <article className="management-row" aria-label={`Installed package ${item.id}`} key={item.id}>
      <div className="row-heading"><h3 className="break-word">{item.provenance.build_revision}</h3><Badge tone={item.phase === "staging" ? "warning" : "quiet"}>{item.phase === "staging" ? "Incomplete installation" : "Installed"}</Badge></div>
      <p className="field-help break-word">{item.id}</p><dl className="facts"><div><dt>Engine</dt><dd>{item.implementation_id}</dd></div><div><dt>Payload reservation</dt><dd>{bytes(item.reserved_bytes)} · {item.member_count} {item.member_count === 1 ? "member" : "members"}</dd></div><div><dt>Declared target</dt><dd>{item.declared_target.operating_system} / {item.declared_target.architecture} / {item.declared_target.abi}</dd></div>{item.binding_leases !== undefined && <div><dt>Retained bindings</dt><dd>{item.binding_leases}</dd></div>}</dl>
      {!item.declared_host_match && <p className="field-help">The declared OS or CPU differs from this service. This package cannot be selected here.</p>}
      {item.phase === "staging" && <p className="field-help">Keep its files and reservation. Retry the same specification after restoring its source. Partial or conflicting files require recovery before retrying.</p>}
    </article>)}</>}
    <p className="field-help">Package labels remain unqualified for ABI and hardware compatibility. Retained bindings are current observations, not permission to delete a package.</p>
  </section>;
}
