import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import { ApiError } from "./api";
import { baseUrl, checkText, clientHints, lastRequest, parseHealth, snippets, statusLabels, statusTones, summary, type ProtocolHealth } from "./connectData";
import type { Model } from "./domain";
import { parseProtocols } from "./management";
import { notify } from "./toast";
import { Badge, Button, HelpTip, Switch } from "./ui";
import { useResource } from "./useResource";
import { WyomingSettings } from "./Wyoming";

export function Copy({ value, label, text = "Copy" }: { value: string; label: string; text?: string }) {
  const [state, setState] = useState("");
  return <Button aria-label={label} onClick={() => void navigator.clipboard.writeText(value).then(() => setState("Copied")).catch(() => setState("Copy failed")).finally(() => setTimeout(() => setState(""), 1500))}>{state || text}</Button>;
}

const operatorOnly = (error: string) => /operator/i.test(error);

export function Protocols({ base, client, models, online, keyed }: { base: string; client: Client; models: Model[]; online: boolean; keyed: boolean }) {
  const origin = base || location.origin;
  const health = useResource(client, "juiced/protocols/health", parseHealth, 5000, true);
  const inventory = useResource(client, "juiced/protocols", parseProtocols, 0, true);
  const [selected, setSelected] = useState<string>("openai");
  const [pending, setPending] = useState<string | null>(null);
  const [language, setLanguage] = useState(0);
  const detail = useRef<HTMLElement>(null);
  useEffect(() => setLanguage(0), [selected]);
  const restricted = !health.data && operatorOnly(health.error);
  const rows: ProtocolHealth[] = health.data ?? (restricted ? (inventory.data ?? []).map((entry) => ({ id: entry.id as ProtocolHealth["id"], title: entry.id, enabled: true, status: "idle", models: 0, check: null, last: null, address: null, listen: null, support: entry.support, note: entry.note ?? null, operations: entry.operations ?? [] })) : []);
  const current = rows.find((row) => row.id === selected) ?? rows[0];
  const toggle = async (protocol: ProtocolHealth, enabled: boolean) => {
    setPending(protocol.id);
    try {
      await client.request("juiced/protocols", { protocol: protocol.id, enabled });
      await health.reload();
      notify(`${protocol.title} is ${enabled ? "on" : "off"}.`);
    } catch (error) {
      notify(error instanceof ApiError || error instanceof Error ? error.message : "The change failed. Retry.", "error");
    } finally { setPending(null); }
  };
  const select = (id: string) => { setSelected(id); if (matchMedia("(max-width: 860px)").matches) requestAnimationFrame(() => detail.current?.scrollIntoView({ block: "start", behavior: "smooth" })); };
  if (!rows.length) return <div className="page-body">{health.error ? <div className="action-row"><p role="alert" className="inline-error">{health.error}</p><Button onClick={() => void health.reload()}>Retry</Button></div> : <p className="muted">Checking protocols…</p>}</div>;
  const examples = current ? snippets(current.id, baseUrl(current, origin, location.hostname), keyed) : [];
  const example = examples[Math.min(language, examples.length - 1)];
  return <div className="connect-layout">
    <section className="connect-main" aria-label="Protocols">
      {restricted && <p className="notice">Use an operator key to see health and turn protocols on or off.</p>}
      {health.error && !restricted && <div className="action-row"><p role="alert" className="inline-error">{health.error}</p><Button onClick={() => void health.reload()}>Retry</Button></div>}
      <ul className="protocol-list" aria-label="Client protocols">{rows.map((row) => {
        const address = baseUrl(row, origin, location.hostname);
        return <li key={row.id} className={`protocol-item ${current?.id === row.id ? "selected" : ""} ${row.enabled ? "" : "off"}`} aria-label={row.title}>
          <button type="button" className="protocol-pick" aria-label={row.title} aria-describedby={restricted ? undefined : `protocol-${row.id}-state`} aria-current={current?.id === row.id ? "true" : undefined} onClick={() => select(row.id)}>
            <span className={`status-dot ${restricted ? "" : statusTones[row.status]}`} aria-hidden="true" />
            <span className="protocol-name"><strong>{row.title}</strong>{!restricted && <span className="muted" id={`protocol-${row.id}-state`}>{[statusLabels[row.status], summary(row)].filter(Boolean).join(" · ")}</span>}</span>
          </button>
          <code className="protocol-url" title={address}>{address}</code>
          <Copy value={address} label={`Copy ${row.title} URL`} />
          {!restricted && <Switch checked={row.enabled} label={`${row.title} enabled`} disabled={!online || pending !== null} onChange={(next) => void toggle(row, next)} />}
        </li>;
      })}</ul>
    </section>
    {current && <aside ref={detail} className="inspector connect-inspector" aria-label={`${current.title} details`}>
      <div className="inspector-body">
        <div className="inspector-title"><h2>{current.title}</h2>{!restricted && <Badge tone={statusTones[current.status] === "quiet" ? "quiet" : statusTones[current.status]}>{statusLabels[current.status]}</Badge>}</div>
        <p className="field-help">{clientHints[current.id]}</p>
        <div className="copy-row"><input readOnly aria-label={`${current.title} URL`} value={baseUrl(current, origin, location.hostname)} /><Copy value={baseUrl(current, origin, location.hostname)} label={`Copy ${current.title} URL from details`} /></div>
        {!restricted && <dl className="facts">
          <div><dt>Models</dt><dd>{current.models}</dd></div>
          <div><dt>Self-check</dt><dd>{checkText(current)}</dd></div>
          <div><dt>Last request</dt><dd>{lastRequest(current) ?? "None yet"}</dd></div>
        </dl>}
        {current.id === "wyoming" ? <WyomingSettings client={client} models={models} online={online} />
          : example && <div className="connect-example" role="region" aria-label="Example request">
            <div className="panel-head"><h3>How to connect</h3>{examples.length > 1 && <div className="segmented" role="group" aria-label="Example language">{examples.map((item, index) => <button key={item.label} aria-pressed={example === item} onClick={() => setLanguage(index)}>{item.label}</button>)}</div>}</div>
            <pre className="code-block">{example.text}</pre>
            <div className="action-row"><Copy value={example.text} label="Copy example" /><span className="field-help">Use a model ID from Models.{keyed ? " Replace YOUR_API_KEY with a key from API keys." : ""}</span></div>
          </div>}
        {(current.note || current.operations.length > 0) && <div className="connect-support">
          <span className="field-label">Support <HelpTip label={`${current.title} support`}>Juiced implements the parts of this protocol listed here. Other features answer with a clear error.</HelpTip></span>
          {current.note && <p className="field-help">{current.note}</p>}
          {current.operations.length > 0 && <div className="chip-row">{current.operations.map((operation) => <code className="chip" key={operation}>{operation}</code>)}</div>}
        </div>}
      </div>
    </aside>}
  </div>;
}
