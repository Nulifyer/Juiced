import { Copy } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "./t3/button";
import { HelpTip } from "./HelpTip";
import { api, type Model } from "../api";
import "./ServiceEndpoints.css";
type Protocol = {
  id: string;
  base_path?: string;
  features: string[];
  limits: string[];
  listener?: { enabled: boolean; process_running: boolean; uri: string };
};
const protocolNames: Record<string, string> = {
  openai: "OpenAI", ollama: "Ollama", anthropic: "Anthropic",
  automatic1111: "AUTOMATIC1111", wyoming: "Wyoming",
};
const endpoints = [
  ["Model discovery", "GET", "/v1/models", "Enabled models available to clients. Disabled models are excluded."],
  ["Chat", "POST", "/v1/chat/completions", "Text, vision and tools according to the selected model's capabilities."],
  ["Transcribe", "POST", "/v1/audio/transcriptions", "Upload audio for speech recognition."],
  ["Speak", "POST", "/v1/audio/speech", "Generate speech from text."],
  ["Voices", "GET", "/v1/audio/voices?model=kokoro-v1", "Voices reported by the selected speech model. Backend support applies."],
  ["Images", "POST", "/v1/images/generations", "Generate images from a prompt."],
];
export function ServiceEndpoints({ models, notify }: { models: Model[]; notify: (message: string) => void }) {
  const [protocols, setProtocols] = useState<Protocol[]>([]);
  const [selected, setSelected] = useState("openai");
  const [protocolError, setProtocolError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    const refresh = () => void api("juiced/protocols", undefined, controller.signal)
      .then((data) => {
        if (!Array.isArray(data.protocols)) throw Error("Protocol inventory is unavailable. Refresh to retry.");
        setProtocols(data.protocols); setProtocolError("");
      })
      .catch((error) => { if (!controller.signal.aborted) setProtocolError(error.message); });
    refresh();
    const timer = window.setInterval(refresh, 10000);
    return () => { controller.abort(); window.clearInterval(timer); };
  }, []);
  const origin = window.location.origin;
  const enabled = models.filter((m) => m.policy !== "disabled" && m.available !== false);
  const protocol = protocols.find((p) => p.id === selected) || protocols[0];
  const address = protocol?.id === "wyoming" ? "Private TCP · port 10300" : origin + (protocol?.base_path ?? "/v1");
  const copy = (value: string) => void navigator.clipboard.writeText(value)
    .then(() => notify("Endpoint copied."))
    .catch(() => notify("Select the address and copy it manually."));
  return (
    <div className="service-endpoints">
      <header className="page-heading">
        <div className="heading-with-help"><h1>Service APIs</h1><HelpTip label="Service APIs">Choose a protocol supported by your client. All interfaces share the same workers and loading policies. Support covers the reported features, rather than every feature of each provider.</HelpTip></div>
        <span className="endpoint-availability">{enabled.length} enabled · {enabled.filter((m) => m.loaded).length} loaded</span>
      </header>
      {protocolError && <p className="inline-error" role="alert">Could not read protocol support: {protocolError}</p>}
      {!protocols.length && !protocolError && <p role="status">Reading protocol support…</p>}
      <nav className="workspace-tabs" aria-label="Client protocols">
        {protocols.map((p) => <button key={p.id} className={protocol?.id === p.id ? "active" : ""} aria-current={protocol?.id === p.id ? "page" : undefined} onClick={() => setSelected(p.id)}>{protocolNames[p.id] || p.id}</button>)}
      </nav>
      <section className="endpoint-primary" aria-label="Client connection">
        <div className="endpoint-connection-heading">
          <h2>{protocol?.id === "wyoming" ? "Wyoming connection" : "Base URL"}</h2>
          {protocol && <HelpTip label={`${protocolNames[protocol.id] || protocol.id} compatibility`}>
            <ul>{protocol.limits.map((limit) => <li key={limit}>{limit}</li>)}</ul>
            {protocol.id === "wyoming" && <p>Use Home Assistant's Wyoming integration with the container's private network name and port 10300. Process status does not prove a completed voice request.</p>}
          </HelpTip>}
          {protocol?.listener && <span className="protocol-status">{protocol.listener.enabled ? protocol.listener.process_running ? "Process running" : "Starting / waiting" : "Disabled"}</span>}
        </div>
        <div className="endpoint-address">
          <code>{address}</code>
          {protocol?.id !== "wyoming" && <Button size="compact" variant="outline" onClick={() => copy(address)}><Copy size={14} />Copy base URL</Button>}
        </div>
        {!!protocol?.features.length && <div className="protocol-features">{protocol.features.map((feature) => <code key={feature}>{feature}</code>)}</div>}
      </section>
      <div className="endpoint-section-heading"><h2>OpenAI endpoints</h2><HelpTip label="Model availability">Disabled models reject inference. Enable a model with On demand or Hot before using it through a client.</HelpTip></div>
      <div className="endpoint-table">
        {endpoints.map(([name, method, path, description]) => <article key={path}>
          <div className="endpoint-name"><strong>{name}</strong><HelpTip label={name}>{description}</HelpTip></div>
          <span className="endpoint-path"><span className="http-method">{method}</span><code>{path}</code></span>
          <Button size="icon-sm" variant="ghost" title={`Copy ${name} endpoint`} aria-label={`Copy ${name} endpoint`} onClick={() => copy(origin + path)}><Copy size={14} /></Button>
        </article>)}
      </div>
      <div className="endpoint-section-heading"><h2>Consumer session caching</h2><HelpTip label="Consumer session caching">Clients own their conversations. For qualified persistent cache reuse, send the full current history with a stable X-Juiced-Session-Id header. Juiced caches computation; temporary UI tests use memory reuse only.</HelpTip></div>
      <code className="session-header">X-Juiced-Session-Id: &lt;stable session ID&gt;</code>
    </div>
  );
}
