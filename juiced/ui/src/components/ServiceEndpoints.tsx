import { ChevronRight, Copy, Server } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "./t3/button";
import { api, type Model } from "../api";
import "./ServiceEndpoints.css";
export function ServiceEndpoints({
  models,
  notify,
}: {
  models: Model[];
  notify: (message: string) => void;
}) {
  const [protocols, setProtocols] = useState<{
    id: string;
    base_path?: string;
    features: string[];
    limits: string[];
    listener?: { enabled: boolean; process_running: boolean; uri: string };
  }[]>([]);
  const [protocolError, setProtocolError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    const refresh = () => void api("juiced/protocols", undefined, controller.signal)
      .then((data) => { setProtocols(data.protocols); setProtocolError(""); })
      .catch((error) => { if (!controller.signal.aborted) setProtocolError(error.message); });
    refresh();
    const timer = window.setInterval(refresh, 10000);
    return () => { controller.abort(); window.clearInterval(timer); };
  }, []);
  const origin = window.location.origin;
  const enabled = models.filter(
    (m) => m.policy !== "disabled" && m.available !== false,
  );
  const copy = (value: string) =>
    void navigator.clipboard
      .writeText(value)
      .then(() => notify("Endpoint copied."))
      .catch(() => notify("Select the address and copy it manually."));
  return (
    <div className="service-endpoints">
      <header className="page-heading">
        <div>
          <h1>Service APIs</h1>
          <p>
            Connect harnesses, agents and home automation to the model runtime.
          </p>
        </div>
      </header>
      <section className="endpoint-primary">
        <Server size={23} />
        <div>
          <h2>OpenAI-compatible base URL</h2>
          <p>
            Use this address in clients that accept a custom OpenAI endpoint.
          </p>
          <div className="endpoint-address">
            <code>{origin}/v1</code>
            <Button
              size="compact"
              variant="outline"
              onClick={() => copy(origin + "/v1")}
            >
              <Copy size={14} />
              Copy
            </Button>
          </div>
        </div>
      </section>
      <h2>Client protocols</h2>
      <p className="endpoint-intro">Choose the provider your client already supports. All interfaces use the same model workers and loading policies.</p>
      {protocolError && <p role="alert">Could not read protocol support: {protocolError}</p>}
      {!protocols.length && !protocolError && <p className="endpoint-intro">Reading protocol support…</p>}
      <div className="protocol-list">
        {protocols.map((protocol) => {
          const names: Record<string, string> = { openai: "OpenAI-compatible", ollama: "Ollama", anthropic: "Anthropic Messages", automatic1111: "AUTOMATIC1111 images", wyoming: "Wyoming speech" };
          const address = protocol.id === "wyoming" ? "Juiced container · private TCP 10300" : origin + (protocol.base_path || "");
          return <details className="protocol-entry" key={protocol.id}>
            <summary>
              <span className="protocol-label"><ChevronRight size={13} />{names[protocol.id] || protocol.id}</span>
              <code>{address}</code>
              <span className="protocol-status">{protocol.listener ? (protocol.listener.enabled ? (protocol.listener.process_running ? "Process running" : "Starting / waiting") : "Disabled") : "Supported subset"}</span>
            </summary>
            <div className="protocol-body">
              <div className="protocol-features">{protocol.features.map((feature) => <code key={feature}>{feature}</code>)}</div>
              <ul>{protocol.limits.map((limit) => <li key={limit}>{limit}</li>)}</ul>
              {protocol.id !== "wyoming" && <Button variant="outline" size="compact" onClick={() => copy(address)}><Copy size={14} />Copy base URL</Button>}
              {protocol.id === "wyoming" && <p>Home Assistant uses the Wyoming integration with the container's private network name and port 10300. No HTTP URL is needed. Process status does not prove a completed voice request.</p>}
            </div>
          </details>;
        })}
      </div>
      <h2>Model services</h2>
      <div className="endpoint-table">
        {[
          [
            "Model discovery",
            "GET",
            "/v1/models",
            "Enabled models available to downstream clients.",
          ],
          [
            "Chat",
            "POST",
            "/v1/chat/completions",
            "Text, vision and tool-calling requests supported by the selected model.",
          ],
          [
            "Transcribe",
            "POST",
            "/v1/audio/transcriptions",
            "Audio uploads for speech recognition.",
          ],
          ["Speak", "POST", "/v1/audio/speech", "Speech synthesis from text."],
          [
            "Voices",
            "GET",
            "/v1/audio/voices?model=kokoro-v1",
            "Available voices reported by a loaded speech model. Backend support applies.",
          ],
          [
            "Images",
            "POST",
            "/v1/images/generations",
            "Image generation from a prompt.",
          ],
        ].map(([name, method, path, description]) => (
          <article key={path}>
            <div>
              <strong>{name}</strong>
              <p>{description}</p>
            </div>
            <code>
              {method} {path}
            </code>
            <Button
              size="icon-sm"
              variant="ghost"
              aria-label={"Copy " + name + " endpoint"}
              onClick={() => copy(origin + path)}
            >
              <Copy size={14} />
            </Button>
          </article>
        ))}
      </div>
      <div className="endpoint-notes">
        <section>
          <h2>Model availability</h2>
          <p>
            {enabled.length} enabled models,{" "}
            {enabled.filter((m) => m.loaded).length} currently loaded. Disabled
            models reject inference and are excluded from OpenAI model
            discovery. Move a model to On demand or Hot to enable it.
          </p>
        </section>
        <section>
          <h2>Consumer session caching</h2>
          <p>
            Clients own their conversations. For qualified persistent cache
            reuse, send the full current history with a stable{" "}
            <code>X-Juiced-Session-Id</code> header. Juiced keeps computation,
            not chat messages. Temporary tests in this UI use memory reuse only.
          </p>
        </section>
      </div>
    </div>
  );
}
