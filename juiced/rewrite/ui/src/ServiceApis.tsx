import { useState } from "react";
import type { Client } from "./api";
import { parseEndpoints, parseProtocols } from "./management";
import { Badge, Button, Empty } from "./ui";
import { useResource } from "./useResource";
import type { Model } from "./domain";
import { WyomingSettings } from "./Wyoming";

const addresses: Record<string, (origin: string) => string> = {
  openai: (origin) => `${origin}/v1`, anthropic: (origin) => `${origin}/v1/messages`, ollama: (origin) => `${origin}/api`, mcp: (origin) => `${origin}/mcp`, automatic1111: (origin) => `${origin}/sdapi/v1`,
  realtime: (origin) => `${origin.replace(/^http/, "ws")}/v1/realtime?intent=transcription`,
};
const names: Record<string, string> = { openai: "OpenAI", anthropic: "Anthropic", ollama: "Ollama", mcp: "MCP", automatic1111: "AUTOMATIC1111", realtime: "Realtime", wyoming: "Wyoming" };

function snippets(origin: string): [string, string][] {
  const model = "YOUR_MODEL_ID";
  return [
    ["curl", `curl ${origin}/v1/chat/completions \\\n  -H 'Content-Type: application/json' \\\n  -d '{"model":"${model}","messages":[{"role":"user","content":"Hello"}]}'`],
    ["Python", `from openai import OpenAI\n\nclient = OpenAI(base_url="${origin}/v1", api_key="unused")\nreply = client.chat.completions.create(\n    model="${model}",\n    messages=[{"role": "user", "content": "Hello"}],\n)\nprint(reply.choices[0].message.content)`],
    ["JavaScript", `import OpenAI from "openai";\n\nconst client = new OpenAI({ baseURL: "${origin}/v1", apiKey: "unused" });\nconst reply = await client.chat.completions.create({\n  model: "${model}",\n  messages: [{ role: "user", content: "Hello" }],\n});\nconsole.log(reply.choices[0].message.content);`],
  ];
}

function Copy({ value, label }: { value: string; label: string }) {
  const [state, setState] = useState("");
  return <Button aria-label={label} onClick={() => void navigator.clipboard.writeText(value).then(() => setState("Copied")).catch(() => setState("Copy failed")).finally(() => setTimeout(() => setState(""), 1500))}>{state || "Copy"}</Button>;
}

export function ServiceApis({ base, client, tab, models, online }: { base: string; client: Client; tab: "protocols" | "endpoints"; models: Model[]; online: boolean }) {
  const origin = base || location.origin;
  const protocols = useResource(client, "juiced/protocols", parseProtocols, 0);
  const endpoints = useResource(client, "juiced/endpoints", parseEndpoints, 0);
  const [language, setLanguage] = useState(0);
  const examples = snippets(origin);
  if (tab === "endpoints") return <section className="page-body" aria-label="All endpoints">
    {endpoints.error && <div className="action-row"><p role="alert" className="inline-error">{endpoints.error}</p><Button onClick={() => void endpoints.reload()}>Retry</Button></div>}
    <p className="field-help">Management routes also answer under /api/v0, /api/v1 and /v0. Operator routes need an operator key when keys are configured.</p>
    {endpoints.data && <div className="data-table endpoint-table" role="table" aria-label="Endpoints">{endpoints.data.map((endpoint) => <div className="data-row" role="row" key={endpoint.method + endpoint.path}><code role="cell" className="break-word">{endpoint.method} {endpoint.prefixes.includes("/v1") ? "/v1" : endpoint.prefixes[0]}{endpoint.path}</code><span role="cell"><Badge>{endpoint.authority}</Badge></span></div>)}</div>}
  </section>;
  return <div className="page-body connect">
    {protocols.error && <div className="action-row"><p role="alert" className="inline-error">{protocols.error}</p><Button onClick={() => void protocols.reload()}>Retry</Button></div>}
    {protocols.data?.length === 0 && <Empty title="No protocols reported">The service didn't report any client protocols.</Empty>}
    <div className="protocol-grid">{protocols.data?.filter((protocol) => protocol.support !== "not-implemented").map((protocol) => {
      const address = addresses[protocol.id]?.(origin);
      return <article className="panel protocol-row" key={protocol.id} aria-label={names[protocol.id] ?? protocol.id}>
        <header className="panel-head"><h3>{names[protocol.id] ?? protocol.id}</h3><Badge tone={protocol.support === "supported" ? "good" : "warning"}>{protocol.support}</Badge></header>
        {address && <div className="copy-row"><input readOnly aria-label={`${names[protocol.id] ?? protocol.id} address`} value={address} /><Copy value={address} label={`Copy ${names[protocol.id] ?? protocol.id} address`} /></div>}
        {protocol.note && <p className="field-help">{protocol.note}</p>}
        {protocol.id === "wyoming" && <WyomingSettings client={client} models={models} online={online} />}
        {protocol.operations && <div className="chip-row">{protocol.operations.map((operation) => <code className="chip" key={operation}>{operation}</code>)}</div>}
      </article>;
    })}</div>
    {protocols.data?.some((protocol) => protocol.support === "not-implemented") && <p className="field-help">Not available yet: {protocols.data.filter((protocol) => protocol.support === "not-implemented").map((protocol) => names[protocol.id] ?? protocol.id).join(", ")}.</p>}
    <section className="panel" aria-label="Example request">
      <header className="panel-head"><h3>Example</h3><div className="segmented" role="group" aria-label="Example language">{examples.map(([name], index) => <button key={name} aria-pressed={language === index} onClick={() => setLanguage(index)}>{name}</button>)}</div></header>
      <pre className="code-block">{examples[language]![1]}</pre>
      <div className="action-row"><Copy value={examples[language]![1]} label="Copy example" /><span className="field-help">Replace YOUR_MODEL_ID with a model ID from Models. Send your key as a Bearer token if the service uses one.</span></div>
    </section>
  </div>;
}
