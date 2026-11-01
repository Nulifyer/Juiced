import { Copy, Server } from "lucide-react";
import { Button } from "./t3/button";
import type { Model } from "../api";
import "./ServiceEndpoints.css";
export function ServiceEndpoints({
  models,
  notify,
}: {
  models: Model[];
  notify: (message: string) => void;
}) {
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
        <section>
          <h2>Other client protocols</h2>
          <p>
            Ollama-compatible clients use <code>{origin}</code> with{" "}
            <code>/api/tags</code> and <code>/api/chat</code>.
            Anthropic-compatible clients use <code>{origin}/v1/messages</code>.
            Model and backend support still applies.
          </p>
        </section>
      </div>
    </div>
  );
}
