import { useEffect, useMemo, useState } from "react";
import type { Client } from "./api";
import { filterRuntimes, parseRuntimeInventory, parseRuntimeResolution, resolutionPath, sourceLink, treatmentCounts, treatmentNames, type NativeRow, type RuntimeDefinition, type RuntimeInventory, type Treatment } from "./runtimeCatalogData";
import { Badge, Button, Empty } from "./ui";
import { useResource } from "./useResource";

export function RuntimeCatalog({ client, online }: { client: Client; online: boolean }) {
  const inventory = useResource(client, "juiced/runtimes", parseRuntimeInventory, 0);
  const [query, setQuery] = useState(""); const [treatment, setTreatment] = useState<Treatment | "all">("parity");
  const [selected, setSelected] = useState("");
  const data = inventory.data;
  const counts = useMemo(() => treatmentCounts(data?.definitions ?? []), [data]);
  const visible = useMemo(() => filterRuntimes(data?.definitions ?? [], treatment, query), [data, treatment, query]);
  return <section className="runtime-catalog" aria-label="Runtime catalog">
    <div className="section-heading"><h2>Runtime catalog</h2><Button disabled={inventory.loading || !online} onClick={() => void inventory.reload()}>Refresh catalog</Button></div>
    <p className="field-help">Planning definitions and original native recipe mappings. Candidate compute paths are discovery leads, not installed packages or qualified support. Catalog browsing does not select a model policy or enable execution.</p>
    {inventory.error && <p role="alert" className="inline-error">{data ? "Showing the last catalog observation. " : ""}{inventory.error}</p>}
    {!data ? !inventory.error && <Empty title="Reading runtime definitions">The catalog describes identities and integration boundaries without installing an engine.</Empty> : <>
      <p className="catalog-summary">{data.definitions.length} definitions · {data.native_recipe_migrations.length} original recipes · {data.native_reference.support_row_count} native declaration rows</p>
      <div className="catalog-boundary"><Badge tone="warning">Planning only</Badge><span>No execution authority or qualification receipts · Package availability and hardware fit not established</span></div>
      <NativeResolver key={client.base} inventory={data} client={client} online={online} />
      <div className="catalog-toolbar"><div><label htmlFor="runtime-search">Search runtime definitions</label><input id="runtime-search" type="search" value={query} placeholder="Name, identity, task or candidate path…" onChange={(event) => { setQuery(event.target.value); setSelected(""); }} /></div><div><label htmlFor="runtime-treatment">Delivery treatment</label><select id="runtime-treatment" value={treatment} onChange={(event) => { setTreatment(event.target.value as Treatment | "all"); setSelected(""); }}><option value="all">All treatments ({data.definitions.length})</option>{Object.entries(treatmentNames).map(([key, label]) => <option key={key} value={key}>{label} ({counts[key as Treatment]})</option>)}</select></div></div>
      <p className="field-help">{treatment === "all" ? "All delivery treatments remain planning scope, independently of any configured workers above." : data.priority_definitions[treatment]}</p>
      <div className="section-heading catalog-count"><h3>Definitions</h3><span className="muted">{visible.length} matches</span></div>
      {visible.length === 0 ? <Empty title="No matching runtime definitions">Change the delivery treatment or clear the search. A missing definition does not change configured workers.</Empty> : visible.map((entry) => <article className="runtime-row" aria-label={`Runtime definition ${entry.id}`} key={entry.id}>
        <div className="row-heading"><div><button className="model-name" aria-expanded={selected === entry.id} aria-controls={`runtime-detail-${entry.id}`} onClick={() => setSelected(selected === entry.id ? "" : entry.id)}>{entry.name}</button><p className="field-help"><code>{entry.id}</code> · {entry.kind}</p></div><Badge tone={entry.adapter.status === "partial" ? "warning" : "quiet"}>{entry.adapter.status === "partial" ? "Partial adapter" : "Planned adapter"}</Badge></div>
        <p className="field-help">Discovery tasks: {entry.tasks.join(", ")}</p><p className="field-help">Candidate paths: {entry.candidate_backends.join(", ")}</p>
        {selected === entry.id && <RuntimeDetails entry={entry} />}
      </article>)}
      <details className="management-section"><summary>Planning vocabulary · {data.compute_paths.length} compute paths and {data.discovery_tasks.length} discovery tasks</summary><p className="field-help">{data.candidate_backend_semantics}</p><div className="vocabulary-list">{data.compute_paths.map((path) => <div key={path.id}><code>{path.id}</code><p className="field-help">{path.description}</p></div>)}</div><p className="field-help">Discovery tasks do not create API operations: {data.discovery_tasks.join(", ")}</p></details>
      <p className="field-help">Catalog reviewed {data.reviewed_at}. Native declarations are a recorded metadata snapshot, not evidence of this host's devices or an installed build.</p>
    </>}
  </section>;
}

function RuntimeDetails({ entry }: { entry: RuntimeDefinition }) {
  return <div className="runtime-detail" id={`runtime-detail-${entry.id}`}>
    <p>{entry.notes}</p><dl className="facts"><div><dt>Upstream lifecycle</dt><dd>{entry.upstream_lifecycle}</dd></div><div><dt>Adapter implementation</dt><dd>{entry.adapter.status}</dd></div><div><dt>Package availability</dt><dd>Not established</dd></div><div><dt>Hardware fit</dt><dd>Unknown</dd></div><div><dt>Qualification receipts</dt><dd>None</dd></div><div><dt>Model policy</dt><dd>Not set by this catalog</dd></div></dl><p className="field-help">{entry.adapter.scope}</p>
    <h4>Recorded sources</h4><ul className="runtime-sources">{entry.sources.map((source) => { const href = sourceLink(source); return <li key={source}>{href ? <a href={href} target="_blank" rel="noreferrer noopener">{source}</a> : <code>{source}</code>}</li>; })}</ul>
  </div>;
}

function NativeResolver({ inventory, client, online }: { inventory: RuntimeInventory; client: Client; online: boolean }) {
  const [recipe, setRecipe] = useState("llamacpp"); const [backend, setBackend] = useState("cpu");
  const [lookup, setLookup] = useState<{ recipe: string; backend: string; request: number } | null>(null);
  useEffect(() => { setLookup(null); }, [inventory]);
  const migration = inventory.native_recipe_migrations.find((item) => item.legacy_recipe === recipe);
  return <details className="native-resolver"><summary>Resolve a native recipe</summary><p className="field-help">Read the exact original recipe/backend labels. Resolution preserves identity and restrictions; it does not install, enable or produce an executable plan.</p>
    <form onSubmit={(event) => { event.preventDefault(); setLookup({ recipe, backend, request: (lookup?.request ?? 0) + 1 }); }}><div className="resolver-controls"><div><label htmlFor="native-recipe">Native recipe</label><select id="native-recipe" value={recipe} onChange={(event) => { const next = inventory.native_recipe_migrations.find((item) => item.legacy_recipe === event.target.value); setRecipe(event.target.value); setBackend(next?.native_backend_labels[0] ?? ""); setLookup(null); }}>{inventory.native_recipe_migrations.map((item) => <option key={item.legacy_recipe} value={item.legacy_recipe}>{item.legacy_recipe} · {item.display_name}</option>)}</select></div><div><label htmlFor="native-backend">Native backend label</label><select id="native-backend" value={backend} onChange={(event) => { setBackend(event.target.value); setLookup(null); }}>{migration?.native_backend_labels.map((label) => <option key={label} value={label}>{label}</option>)}</select></div><Button variant="primary" type="submit" disabled={!online || !migration?.native_backend_labels.includes(backend)}>Resolve mapping</Button></div></form>
    {lookup && <NativeResolution key={`${lookup.recipe}:${lookup.backend}:${lookup.request}`} recipe={lookup.recipe} backend={lookup.backend} client={client} online={online} />}
  </details>;
}

function NativeResolution({ recipe, backend, client, online }: { recipe: string; backend: string; client: Client; online: boolean }) {
  const resolution = useResource(client, resolutionPath(recipe, backend), parseRuntimeResolution, 0);
  const data = resolution.data;
  const matches = data?.legacy_recipe === recipe && data.legacy_backend === backend;
  return <section className="native-resolution" aria-label="Native recipe resolution" aria-busy={resolution.loading}>
    {resolution.error && <p role="alert" className="inline-error">{data ? "Showing the last mapping observation. " : ""}{resolution.error}</p>}
    {resolution.loading && <p className="field-help">Reading the original native mapping…</p>}
    {data && !matches && <p role="alert" className="inline-error">The returned mapping does not match this selection. Retry the mapping.</p>}
    {matches && <><div className="row-heading"><h3><code>{data.implementation_id}</code></h3><Badge tone="warning">Not executable</Badge></div><dl className="facts"><div><dt>Original recipe / backend</dt><dd>{data.legacy_recipe} / {data.legacy_backend}</dd></div><div><dt>Compute vocabulary</dt><dd>{data.compute_path_id ?? "Unresolved"}</dd></div><div><dt>Adapter implementation</dt><dd>{data.adapter.status}</dd></div><div><dt>Qualification receipts</dt><dd>None</dd></div></dl><p className="field-help">{data.evidence_scope}</p><p className="field-help">{data.migration_rule}</p>{data.override_reason && <p className="field-help">{data.override_reason}</p>}<p className="field-help">{data.adapter.scope}</p><h4>Execution blockers</h4><ul className="resolution-blockers">{data.blockers.map((blocker) => <li key={blocker.code}>{blocker.message}</li>)}</ul><h4>Original native restrictions</h4><p className="field-help">These rows preserve native metadata. They do not qualify the Rust adapter, this host or a model.</p><NativeRows rows={data.native_support_rows} /></>}
    <Button disabled={resolution.loading || !online} onClick={() => void resolution.reload()}>Retry mapping</Button>
  </section>;
}
function NativeRows({ rows }: { rows: NativeRow[] }) {
  return <div className="native-rows">{rows.map((row, index) => <div className="native-row" key={`${row.backend}:${index}`}><strong>{row.backend} · {row.tier}</strong><dl className="facts"><div><dt>Original format</dt><dd>{row.format}</dd></div><div><dt>Operating systems</dt><dd>{row.os.join(", ")}</dd></div><div><dt>Device declaration</dt><dd>{row.device_declaration}</dd></div></dl></div>)}</div>;
}
