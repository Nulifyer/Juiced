import { useEffect, useState } from "react";
import type { Client } from "./api";
import type { Model } from "./domain";
import { parseVoices } from "./MediaTests";
import { Badge, Button } from "./ui";
import { useOperation, useResource } from "./useResource";

type Settings = { enabled: boolean; listen: string; stt_model: string | null; tts_model: string | null; tts_voice: string | null };
type View = { generation: number; settings: Settings; listening: boolean; address: string | null; error: string | null; connections: number; last: { type: string; outcome: string; duration_ms: number; at_unix_ms: number } | null; ttsModel: string | null; voices: string[] };

const text = (value: unknown) => typeof value === "string" ? value : null;
export function parseWyoming(value: unknown): View {
  const body = value && typeof value === "object" ? value as Record<string, any> : {};
  const settings = body.settings ?? {};
  if (typeof body.configuration_generation !== "number" || typeof settings.listen !== "string") throw new Error("The service returned unexpected Wyoming settings.");
  return {
    generation: body.configuration_generation,
    settings: { enabled: settings.enabled === true, listen: settings.listen, stt_model: text(settings.stt_model), tts_model: text(settings.tts_model), tts_voice: text(settings.tts_voice) },
    listening: body.listener?.listening === true, address: text(body.listener?.address), error: text(body.listener?.error?.message),
    connections: typeof body.active_connections === "number" ? body.active_connections : 0,
    last: body.last_event && typeof body.last_event.type === "string" ? body.last_event : null,
    ttsModel: text(body.resolved?.tts_model), voices: Array.isArray(body.resolved?.tts_voices) ? body.resolved.tts_voices.filter((voice: unknown) => typeof voice === "string") : [],
  };
}

function hostFor(listen: string, address: string | null) {
  const value = address ?? listen;
  const port = value.slice(value.lastIndexOf(":") + 1);
  const host = value.slice(0, value.lastIndexOf(":")).replace(/^\[|\]$/g, "");
  return `${["0.0.0.0", "::", ""].includes(host) ? location.hostname : host}:${port}`;
}

export function WyomingSettings({ client, models, online }: { client: Client; models: Model[]; online: boolean }) {
  const view = useResource(client, "juiced/wyoming", parseWyoming, 3000);
  const operation = useOperation(client, view.reload);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [voices, setVoices] = useState<{ model: string; list: string[] } | null>(null);
  const data = view.data;
  const current = draft ?? data?.settings ?? null;
  const ttsTarget = current?.tts_model ?? data?.ttsModel ?? null;
  useEffect(() => {
    if (!ttsTarget || ttsTarget === data?.ttsModel) return;
    const controller = new AbortController();
    client.request(`juiced/voices?model=${encodeURIComponent(ttsTarget)}`, undefined, controller.signal).then((value) => setVoices({ model: ttsTarget, list: parseVoices(value) })).catch(() => undefined);
    return () => controller.abort();
  }, [client, ttsTarget, data?.ttsModel]);
  if (!data || !current) return view.error ? <p className="inline-error" role="alert">{view.error}</p> : null;
  const voiceList = ttsTarget === data.ttsModel ? data.voices : voices?.model === ttsTarget ? voices.list : [];
  const speech = models.filter((model) => model.operations.includes("audio/transcriptions"));
  const tts = models.filter((model) => model.operations.includes("audio/speech"));
  const dirty = !!draft && JSON.stringify(draft) !== JSON.stringify(data.settings);
  const blocked = !online || operation.busy;
  const save = (settings: Settings, success: string) => void operation.run("juiced/wyoming", { settings, expected_generation: data.generation }, success).then((result) => { if (result !== undefined) setDraft(null); });
  const edit = (change: Partial<Settings>) => setDraft({ ...current, ...change });
  const address = hostFor(data.settings.listen, data.address);
  return <div className="wyoming">
    <div className="action-row wrap">
      <div className="segmented" role="radiogroup" aria-label="Wyoming server">{([false, true] as const).map((value) => <button key={String(value)} role="radio" aria-checked={data.settings.enabled === value} disabled={blocked || dirty} onClick={() => data.settings.enabled !== value && save({ ...data.settings, enabled: value }, value ? "Wyoming server is on." : "Wyoming server is off.")}>{value ? "On" : "Off"}</button>)}</div>
      {data.settings.enabled && (data.error ? <Badge tone="bad">Not listening</Badge> : data.listening ? <Badge tone="good">Listening</Badge> : <Badge tone="warning">Starting</Badge>)}
      {data.listening && <span className="muted">{data.connections} {data.connections === 1 ? "connection" : "connections"}</span>}
    </div>
    {data.error && <p className="inline-error" role="alert">{data.error}</p>}
    {data.settings.enabled && data.listening && <p className="field-help">In Home Assistant, add the Wyoming Protocol integration with host and port <code>{address}</code>.</p>}
    <div className="form-grid wyoming-grid">
      <div><label htmlFor="wyoming-listen">Listen on</label><input id="wyoming-listen" value={current.listen} disabled={blocked} spellCheck={false} onChange={(event) => edit({ listen: event.target.value })} /></div>
      <div><label htmlFor="wyoming-stt">Speech to text</label><select id="wyoming-stt" value={current.stt_model ?? ""} disabled={blocked} onChange={(event) => edit({ stt_model: event.target.value || null })}><option value="">Every enabled speech model</option>{speech.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}</select></div>
      <div><label htmlFor="wyoming-tts">Text to speech</label><select id="wyoming-tts" value={current.tts_model ?? ""} disabled={blocked} onChange={(event) => edit({ tts_model: event.target.value || null, tts_voice: null })}><option value="">First enabled voice model</option>{tts.map((model) => <option key={model.id} value={model.id}>{model.id}</option>)}</select></div>
      <div><label htmlFor="wyoming-voice">Voice</label><select id="wyoming-voice" value={current.tts_voice ?? ""} disabled={blocked || voiceList.length === 0} onChange={(event) => edit({ tts_voice: event.target.value || null })}><option value="">{voiceList[0] ? `Default (${voiceList[0]})` : "Default"}</option>{voiceList.map((voice) => <option key={voice} value={voice}>{voice}</option>)}</select></div>
    </div>
    {dirty && <div className="action-row"><Button variant="primary" disabled={blocked} onClick={() => save(current, "Wyoming settings saved.")}>Save</Button><Button disabled={operation.busy} onClick={() => setDraft(null)}>Cancel</Button></div>}
    {operation.error && <p className="inline-error" role="alert">{operation.error}</p>}
    {data.last && <p className="field-help">Last {data.last.type === "transcribe" ? "transcription" : "speech"}: {data.last.outcome === "completed" ? `done in ${(data.last.duration_ms / 1000).toFixed(1)} s` : data.last.outcome.replaceAll("_", " ")}.</p>}
  </div>;
}
