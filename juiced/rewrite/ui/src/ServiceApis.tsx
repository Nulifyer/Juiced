import { useState } from "react";
import type { Client } from "./api";
import { ApiKeys } from "./ApiKeys";
import { Protocols } from "./Connect";
import type { Model } from "./domain";
import { parseEndpoints } from "./management";
import { Badge, Button, HelpTip } from "./ui";
import { useResource } from "./useResource";

export type ConnectTab = "protocols" | "keys" | "endpoints";

function Endpoints({ client }: { client: Client }) {
  const endpoints = useResource(client, "juiced/endpoints", parseEndpoints, 0);
  const [filter, setFilter] = useState("");
  const rows = (endpoints.data ?? []).map((endpoint) => ({ ...endpoint, route: `${endpoint.method} ${endpoint.prefixes.includes("/v1") ? "/v1" : endpoint.prefixes[0] ?? ""}${endpoint.path}` }));
  const words = filter.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const shown = rows.filter((row) => words.every((word) => `${row.route} ${row.authority}`.toLowerCase().includes(word)));
  return <section className="page-body" aria-label="All endpoints">
    {endpoints.error && <div className="action-row"><p role="alert" className="inline-error">{endpoints.error}</p><Button onClick={() => void endpoints.reload()}>Retry</Button></div>}
    <div className="toolbar"><div className="page-title"><h2>Endpoints</h2><HelpTip label="Endpoints">Management routes also answer under /api/v0, /api/v1 and /v0. Routes of turned-off protocols are hidden. The API guide in the docs describes each one.</HelpTip></div>
      <input type="search" className="endpoint-filter" aria-label="Filter endpoints" placeholder="Filter, e.g. chat or operator" value={filter} onChange={(event) => setFilter(event.target.value)} />
      {endpoints.data && <span className="muted">{words.length ? `${shown.length} of ${rows.length}` : rows.length}</span>}</div>
    {endpoints.data && (shown.length ? <div className="data-table endpoint-table" role="table" aria-label="Endpoints">{shown.map((endpoint) => <div className="data-row" role="row" key={endpoint.method + endpoint.path}><code role="cell" className="break-word">{endpoint.route}</code><span role="cell"><Badge>{endpoint.authority}</Badge></span></div>)}</div> : <p className="muted">No endpoints match.</p>)}
  </section>;
}

export function ServiceApis({ base, client, tab, models, online, consoleKey, onAdoptKey }: { base: string; client: Client; tab: ConnectTab; models: Model[]; online: boolean; consoleKey: string; onAdoptKey: (key: string) => void }) {
  const keyed = useResource(client, "juiced/access", (value) => (value as { mode?: string })?.mode === "keys", 10000, true);
  if (tab === "endpoints") return <Endpoints client={client} />;
  if (tab === "keys") return <ApiKeys client={client} online={online} consoleKey={consoleKey} onAdoptKey={onAdoptKey} />;
  return <Protocols base={base} client={client} models={models} online={online} keyed={keyed.data ?? !!consoleKey} />;
}
