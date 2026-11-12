import type { Client } from "./api";
import { ApiKeys } from "./ApiKeys";
import { Protocols } from "./Connect";
import type { Model } from "./domain";
import { parseEndpoints } from "./management";
import { Badge, Button } from "./ui";
import { useResource } from "./useResource";

export type ConnectTab = "protocols" | "keys" | "endpoints";

function Endpoints({ client }: { client: Client }) {
  const endpoints = useResource(client, "juiced/endpoints", parseEndpoints, 0);
  return <section className="page-body" aria-label="All endpoints">
    {endpoints.error && <div className="action-row"><p role="alert" className="inline-error">{endpoints.error}</p><Button onClick={() => void endpoints.reload()}>Retry</Button></div>}
    <p className="field-help">Management routes also answer under /api/v0, /api/v1 and /v0. Routes of turned-off protocols are hidden.</p>
    {endpoints.data && <div className="data-table endpoint-table" role="table" aria-label="Endpoints">{endpoints.data.map((endpoint) => <div className="data-row" role="row" key={endpoint.method + endpoint.path}><code role="cell" className="break-word">{endpoint.method} {endpoint.prefixes.includes("/v1") ? "/v1" : endpoint.prefixes[0]}{endpoint.path}</code><span role="cell"><Badge>{endpoint.authority}</Badge></span></div>)}</div>}
  </section>;
}

export function ServiceApis({ base, client, tab, models, online, consoleKey, onAdoptKey }: { base: string; client: Client; tab: ConnectTab; models: Model[]; online: boolean; consoleKey: string; onAdoptKey: (key: string) => void }) {
  const keyed = useResource(client, "juiced/access", (value) => (value as { mode?: string })?.mode === "keys", 10000, true);
  if (tab === "endpoints") return <Endpoints client={client} />;
  if (tab === "keys") return <ApiKeys client={client} online={online} consoleKey={consoleKey} onAdoptKey={onAdoptKey} />;
  return <Protocols base={base} client={client} models={models} online={online} keyed={keyed.data ?? !!consoleKey} />;
}
