import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import { registration, positiveBytes } from "./management";
import { launchOptions, type LaunchDraft, type Provider } from "./profiles";
import { Button } from "./ui";
import { useOperation } from "./useResource";

export function Registration({ artifactId, artifactSet, unavailableReason, client, disabled, onClose, onRegistered }: { artifactId: string; artifactSet?: string; unavailableReason?: string; client: Client; disabled: boolean; onClose: () => void; onRegistered: () => Promise<void> }) {
  const form = useRef<HTMLFormElement>(null);
  useEffect(() => { form.current?.scrollIntoView({ block: "start" }); form.current?.querySelector<HTMLInputElement>("#registration-id")?.focus({ preventScroll: true }); }, []);
  const [id, setId] = useState(""); const [command, setCommand] = useState("");
  const [provider, setProvider] = useState<Provider>("external-http");
  const [args, setArgs] = useState('["--host", "127.0.0.1", "--port", "{port}", "--model", "{model}"]');
  const [resident, setResident] = useState(""); const [slots, setSlots] = useState("1");
  const [launch, setLaunch] = useState<LaunchDraft>({ context: "", threads: "", gpu: "" });
  const [operations, setOperations] = useState(["chat/completions", "completions"]);
  const operation = useOperation(client, onRegistered);
  function selectProvider(next: Provider) {
    if (next === "whisper-cpp") { setSlots("1"); setOperations(["audio/transcriptions"]); setArgs('["--host", "127.0.0.1", "--port", "{port}", "--model", "{model}", "--threads", "2", "--no-gpu"]'); }
    else if (provider === "whisper-cpp") { setOperations(["chat/completions", "completions"]); setArgs('["--host", "127.0.0.1", "--port", "{port}", "--model", "{model}"]'); }
    setProvider(next);
  }
  async function register() {
    try {
      const options = provider === "llama-cpp" ? launchOptions(launch, positiveBytes(slots, "Physical slots")) : undefined;
      const spec = registration(artifactId, id, command, args, resident, slots, operations, provider, options, artifactSet);
      await operation.run("models/register", spec, `Registered ${spec.id} as Disabled. Enable it in Loading policy when ready.`);
    } catch (error) { operation.fail((error as Error).message); }
  }
  const argumentFields = <><label htmlFor="registration-args">Engine arguments as JSON</label><textarea id="registration-args" rows={3} value={args} onChange={(event) => setArgs(event.target.value)} spellCheck={false} /><p className="field-help">Placeholders: {'{port}'}, {'{model}'}{provider === "whisper-cpp" ? "" : <>, {'{cache}'} for snapshots</>}. No shell is used.</p></>;
  return <form ref={form} className="management-section registration-form" aria-label="Register managed model" onSubmit={(event) => { event.preventDefault(); void register(); }}>
    <div className="section-heading"><h2>Register model</h2><Button onClick={onClose} disabled={operation.busy}>Close registration</Button></div>
    <p className="field-help break-word">Weights: {artifactId}</p>{artifactSet && <p className="field-help break-word">Artifact set: {artifactSet}</p>}
    <div className="form-grid">
      <div><label htmlFor="registration-id">New model ID</label><input id="registration-id" value={id} onChange={(event) => setId(event.target.value)} /></div>
      <div><label htmlFor="registration-provider">Engine provider</label><select id="registration-provider" value={provider} disabled={operation.busy} onChange={(event) => selectProvider(event.target.value as Provider)}><option value="external-http">External HTTP engine</option><option value="llama-cpp">llama.cpp</option><option value="whisper-cpp">whisper.cpp speech</option></select></div>
      <div className="form-wide"><label htmlFor="registration-command">Engine executable</label><input id="registration-command" value={command} onChange={(event) => setCommand(event.target.value)} spellCheck={false} /></div>
      <div><label htmlFor="registration-resident">Resident reservation in bytes</label><input id="registration-resident" inputMode="numeric" value={resident} onChange={(event) => setResident(event.target.value)} /></div>
      <div><label htmlFor="registration-slots">{provider === "external-http" ? "Request slots" : "Physical slots"}</label><input id="registration-slots" inputMode="numeric" disabled={provider === "whisper-cpp"} value={slots} onChange={(event) => setSlots(event.target.value)} /></div>
      {provider === "llama-cpp" && <>
        <div className="form-wide"><label htmlFor="registration-context">Initial total context budget in tokens</label><input id="registration-context" inputMode="numeric" value={launch.context} placeholder="Engine arguments or default" onChange={(event) => setLaunch({ ...launch, context: event.target.value })} /></div>
        <div><label htmlFor="registration-threads">Initial threads</label><input id="registration-threads" inputMode="numeric" value={launch.threads} placeholder="Engine default" onChange={(event) => setLaunch({ ...launch, threads: event.target.value })} /></div>
        <div><label htmlFor="registration-gpu">Initial GPU layers</label><input id="registration-gpu" inputMode="numeric" value={launch.gpu} placeholder="Engine default" onChange={(event) => setLaunch({ ...launch, gpu: event.target.value })} /></div>
      </>}
    </div>
    {provider === "llama-cpp" ? <details><summary>Advanced engine arguments</summary>{argumentFields}</details> : <div className="management-section">{argumentFields}</div>}
    {provider === "whisper-cpp" && <p className="field-help">Uses whisper-server with one slot for transcription.</p>}
    <fieldset><legend>Declared engine operations</legend><div className="checkbox-row">{(provider === "whisper-cpp" ? ["audio/transcriptions"] : ["chat/completions", "completions", "responses", "embeddings", "messages/count_tokens"]).map((value) => <label key={value}><input type="checkbox" disabled={provider === "whisper-cpp"} checked={operations.includes(value)} onChange={(event) => setOperations(event.target.checked ? [...operations, value] : operations.filter((item) => item !== value))} />{value}</label>)}</div></fieldset>
    {operation.error && <p role="alert" className="inline-error">{operation.error}</p>}{operation.notice && <p role="status" className="inline-success">{operation.notice}</p>}
    <div className="form-actions"><div>{unavailableReason && <p role="alert" className="inline-warning">{unavailableReason}</p>}<p className="field-help">New models start Disabled.</p></div><Button type="submit" variant="primary" disabled={disabled || operation.busy}>Register Disabled model</Button></div>
  </form>;
}
