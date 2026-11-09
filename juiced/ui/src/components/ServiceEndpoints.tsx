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
type Endpoint = [name: string, method: string, path: string, description: string];
const protocolNames: Record<string, string> = {
  openai: "OpenAI", ollama: "Ollama", anthropic: "Anthropic",
  automatic1111: "AUTOMATIC1111", wyoming: "Wyoming",
};
const endpoints: Record<string, Endpoint[]> = {
  openai: [
    ["Juiced capabilities", "GET", "/v1/juiced/capabilities", "Optional native discovery extension. Lists enabled downloaded models, declared operations, restrictions and qualification status; not an OpenAI standard endpoint."],
    ["Model discovery", "GET", "/v1/models", "Enabled models available to clients. Disabled models are excluded."],
    ["Chat", "POST", "/v1/chat/completions", "Text, vision and tools according to the selected model's capabilities."],
    ["Responses", "POST", "/v1/responses", "The supported Responses subset uses the same model workers."],
    ["Embeddings", "POST", "/v1/embeddings", "Requires an enabled compatible embedding model."],
    ["Transcribe", "POST", "/v1/audio/transcriptions", "Upload audio for speech recognition."],
    ["Speak", "POST", "/v1/audio/speech", "Generate speech from text."],
    ["Images", "POST", "/v1/images/generations", "Generate images from a prompt."],
  ],
  ollama: [
    ["Model discovery", "GET", "/api/tags", "Lists enabled models using the Ollama response format."],
    ["Model details", "POST", "/api/show", "Read metadata for a named model."],
    ["Loaded models", "GET", "/api/ps", "Reports currently loaded workers."],
    ["Chat", "POST", "/api/chat", "Chat, streaming and supported tool calls through the selected model."],
    ["Generate", "POST", "/api/generate", "Prompt completion through an enabled language model."],
    ["Embeddings", "POST", "/api/embed", "Requires a compatible embedding model."],
    ["Legacy embeddings", "POST", "/api/embeddings", "Legacy single-prompt embedding interface."],
  ],
  anthropic: [
    ["Messages", "POST", "/v1/messages", "Messages, tool use and streaming according to model capabilities."],
    ["Count tokens", "POST", "/v1/messages/count_tokens", "Requires a backend with native message token counting."],
  ],
  automatic1111: [
    ["Image models", "GET", "/sdapi/v1/sd-models", "Lists compatible enabled sd-cpp models."],
    ["Options", "GET/POST", "/sdapi/v1/options", "Read or select the image model using the supported options subset."],
    ["Generate image", "POST", "/sdapi/v1/txt2img", "Text-to-image requests through the same sd-cpp workers."],
    ["Samplers", "GET", "/sdapi/v1/samplers", "Reports supported sampler choices."],
    ["Schedulers", "GET", "/sdapi/v1/schedulers", "Reports supported scheduler choices."],
    ["Progress", "GET", "/sdapi/v1/progress", "Reads current image-generation progress."],
  ],
};
export function ServiceEndpoints({ models, notify, initialModel }: { models: Model[]; notify: (message: string) => void; initialModel?: string }) {
  const [modelId, setModelId] = useState(initialModel || "");
  const [protocols, setProtocols] = useState<Protocol[]>([]);
  const [selected, setSelected] = useState("openai");
  const [protocolError, setProtocolError] = useState("");
  const [revision, setRevision] = useState(0);
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
  }, [revision]);
  const origin = window.location.origin;
  const enabled = models.filter((m) => m.downloaded && m.policy !== "disabled" && m.available !== false);
  const connectionModel = enabled.find((m) => m.id === modelId) || enabled.find((m) => m.loaded && m.service === "Chat") || enabled[0];
  const protocol = protocols.find((p) => p.id === selected) || protocols[0];
  const routes = [...(endpoints[protocol?.id || ""] || [])];
  const speech = enabled.find((m) => m.service === "Speak" && m.loaded) || enabled.find((m) => m.service === "Speak");
  if (protocol?.id === "openai" && speech)
    routes.splice(6, 0, ["Voices", "GET", `/v1/audio/voices?model=${encodeURIComponent(speech.id)}`, "Voices reported by this speech model. Backend support applies."]);
  let voicePort = "10300";
  if (protocol?.listener?.uri) {
    try { voicePort = new URL(protocol.listener.uri).port || voicePort; } catch { /* Use the documented listener port. */ }
  }
  const address = protocol?.id === "wyoming" ? `Private TCP · port ${voicePort}` : origin + (protocol?.base_path ?? "/v1");
  const copy = (value: string, message = "Endpoint copied.") => void navigator.clipboard.writeText(value)
    .then(() => notify(message))
    .catch(() => notify("Select the address and copy it manually."));
  return (
    <div className="service-endpoints">
      <header className="page-heading">
        <div className="heading-with-help"><h1>Service APIs</h1><HelpTip label="Service APIs">Choose a protocol supported by your client. All interfaces share workers and loading policies. Disabled models are unavailable to clients.</HelpTip></div>
        <span className="endpoint-availability">{enabled.length} enabled · {enabled.filter((m) => m.loaded).length} loaded</span>
      </header>
      {protocolError && <div className="inline-error endpoint-inventory-error" role="alert"><span>Could not read protocol support: {protocolError}</span><Button variant="outline" size="compact" onClick={() => setRevision(value => value + 1)}>Retry support</Button></div>}
      {!protocols.length && !protocolError && <p role="status">Reading protocol support…</p>}
      <nav className="workspace-tabs" aria-label="Client protocols">
        {protocols.map((p) => <button key={p.id} className={protocol?.id === p.id ? "active" : ""} aria-current={protocol?.id === p.id ? "page" : undefined} onClick={() => setSelected(p.id)}>{protocolNames[p.id] || p.id}</button>)}
      </nav>
      {protocol && <>
        <section className="endpoint-primary" aria-label="Client connection">
          <div className="endpoint-connection-heading">
            <h2>{protocol.id === "wyoming" ? "Wyoming connection" : "Base URL"}</h2>
            <HelpTip label={`${protocolNames[protocol.id] || protocol.id} compatibility`}>
              <ul>{protocol.limits.map((limit) => <li key={limit}>{limit}</li>)}</ul>
              <p>Supported operations: {protocol.features.join(", ")}.</p>
              {protocol.id === "wyoming" && <p>Use Home Assistant's Wyoming integration with a private container or host address reachable by the client. The HTTPS address above the app does not proxy this TCP listener. Process status does not prove a completed voice request.</p>}
            </HelpTip>
            {protocol.listener && <span className="protocol-status">{protocol.listener.enabled ? protocol.listener.process_running ? "Process running" : "Starting / waiting" : "Disabled"}</span>}
          </div>
          <div className="endpoint-address">
            <code>{address}</code>
            {protocol.id !== "wyoming" && <Button size="compact" variant="outline" onClick={() => copy(address)}><Copy size={14} />Copy base URL</Button>}
          </div>
        </section>
        {protocol.id !== "wyoming" && <section className="endpoint-primary" aria-label="Client model selection">
          <h2>Model ID</h2>
          {connectionModel ? <>
            <label htmlFor="client-model-id" className="sr-only">Client model</label>
            <div className="endpoint-address">
              <select id="client-model-id" value={connectionModel.id} onChange={(event) => setModelId(event.target.value)}>
                {enabled.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}
              </select>
              <Button size="compact" variant="outline" onClick={() => copy(connectionModel.id, "Model ID copied.")}><Copy size={14} />Copy model ID</Button>
            </div>
            <p className="page-note">Use this exact ID in your client. {connectionModel.service} · {connectionModel.loaded ? "Running" : connectionModel.policy === "hot" ? "Hot · not running" : "On demand · not running"}. Check that the selected protocol supports the operation you need.</p>
          </> : <p className="page-note">Download and enable a model in Library before configuring its ID in a client.</p>}
        </section>}
        {routes.length > 0 && <>
          <div className="endpoint-section-heading"><h2>{protocolNames[protocol.id] || protocol.id} endpoints</h2></div>
          <div className="endpoint-table">
            {routes.map(([name, method, path, description]) => <article key={path}>
              <div className="endpoint-name"><strong>{name}</strong><HelpTip label={name}>{description}</HelpTip></div>
              <span className="endpoint-path"><span className="http-method">{method}</span><code>{path}</code></span>
              <Button size="icon-sm" variant="ghost" title={`Copy ${name} endpoint`} aria-label={`Copy ${name} endpoint`} onClick={() => copy(origin + path)}><Copy size={14} /></Button>
            </article>)}
          </div>
        </>}
        {protocol.id === "openai" && <>
          <div className="endpoint-section-heading"><h2>Consumer session caching</h2><HelpTip label="Consumer session caching">Clients own their conversations. For qualified persistent Chat Completions cache reuse, send the full current history with a stable X-Juiced-Session-Id header. Temporary UI tests use memory reuse only.</HelpTip></div>
          <code className="session-header">X-Juiced-Session-Id: &lt;stable session ID&gt;</code>
        </>}
      </>}
    </div>
  );
}
