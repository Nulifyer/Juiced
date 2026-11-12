import { useState } from "react";
import type { Client } from "./api";
import { ago, parseAccess, parseCreated, scopeLabels, until, type AccessKey, type Scope } from "./connectData";
import { Copy } from "./Connect";
import { notify } from "./toast";
import { Badge, Button, Dialog, HelpTip } from "./ui";
import { useResource } from "./useResource";

const expiries = [["", "Never"], ["30", "30 days"], ["90", "90 days"], ["365", "1 year"]] as const;

export function ApiKeys({ client, online, consoleKey, onAdoptKey }: { client: Client; online: boolean; consoleKey: string; onAdoptKey: (key: string) => void }) {
  const access = useResource(client, "juiced/access", parseAccess, 10000, true);
  const [name, setName] = useState("");
  const [scope, setScope] = useState<Scope>("inference");
  const [expiry, setExpiry] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [revealed, setRevealed] = useState<{ key: AccessKey; secret: string; adopted: boolean } | null>(null);
  const [revoking, setRevoking] = useState<AccessKey | null>(null);
  const data = access.data;
  if (!data) return <div className="page-body">{access.error ? <div className="action-row"><p role="alert" className="inline-error">{access.error}</p><Button onClick={() => void access.reload()}>Retry</Button></div> : <p className="muted">Reading API keys…</p>}</div>;
  const issued = data.keys.filter((key) => key.source === "issued");
  const anchored = data.keys.some((key) => key.scope === "operator" && !key.expires);
  const first = !anchored;
  const effectiveScope: Scope = first ? "operator" : scope;
  const effectiveExpiry = first ? "" : expiry;
  const create = async () => {
    setBusy(true); setError("");
    try {
      const body: Record<string, unknown> = { name: name.trim(), scope: effectiveScope };
      if (effectiveExpiry) body.expires_in_days = Number(effectiveExpiry);
      const created = parseCreated(await client.request("juiced/access/keys", body));
      const adopted = data.mode === "trusted-lan" && created.access.mode === "keys";
      setRevealed({ key: created.key, secret: created.secret, adopted });
      setName("");
      if (adopted) onAdoptKey(created.secret);
      else await access.reload();
    } catch (failure) { setError(failure instanceof Error ? failure.message : "The key could not be created. Retry."); }
    finally { setBusy(false); }
  };
  const revoke = async (key: AccessKey) => {
    setBusy(true); setError("");
    try {
      await client.request("juiced/access/keys/revoke", { id: key.id });
      setRevoking(null);
      notify(`Revoked ${key.name}.`);
      await access.reload();
    } catch (failure) { setRevoking(null); setError(failure instanceof Error ? failure.message : "The key could not be revoked. Retry."); }
    finally { setBusy(false); }
  };
  const mine = (key: AccessKey) => !!consoleKey && !!key.prefix && consoleKey.startsWith(key.prefix);
  const lastIssued = revoking && issued.length === 1 && data.keys.every((key) => key.source === "issued");
  return <div className="page-body api-keys">
    {data.storeError && <p className="notice error" role="alert">The key store {data.storeError}. Issued keys are refused until it is fixed.</p>}
    <div className={`access-mode ${data.mode}`} role="status">
      <span className={`status-dot ${data.mode === "keys" ? "good" : "warning"}`} />
      {data.mode === "keys" ? <span><strong>Keys required.</strong> Every API route needs a key with the right scope.</span>
        : <span><strong>Open to this network.</strong> Anyone who can reach Juiced can use every API, including this console.</span>}
    </div>
    <form className="key-create" aria-label="Create key" onSubmit={(event) => { event.preventDefault(); void create(); }}>
      <div><label htmlFor="key-name">Name</label><input id="key-name" value={name} maxLength={data.limits.nameChars} placeholder={first ? "This console" : "Home Assistant"} disabled={busy || !online} onChange={(event) => setName(event.target.value)} /></div>
      <div><span className="label-row"><label htmlFor="key-scope">Scope</label><HelpTip label="Key scopes">Read only: models and discovery. Inference: also chat, embeddings, speech, images and transcription. Operator: everything, including this console.</HelpTip></span>
        <select id="key-scope" value={effectiveScope} disabled={busy || !online || first} onChange={(event) => setScope(event.target.value as Scope)}>{(["inference", "read", "operator"] as const).map((value) => <option key={value} value={value}>{scopeLabels[value]}</option>)}</select></div>
      <div><label htmlFor="key-expiry">Expires</label><select id="key-expiry" value={effectiveExpiry} disabled={busy || !online || first} onChange={(event) => setExpiry(event.target.value)}>{expiries.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></div>
      <Button variant="primary" type="submit" disabled={busy || !online || !name.trim() || issued.length >= data.limits.keys}>{first ? "Create operator key" : "Create key"}</Button>
    </form>
    {first && <p className="field-help">{data.mode === "keys" ? "Create an operator key without expiry first, so operators keep access." : "The first key is an operator key without expiry. Creating it turns on key checks, and this console switches to it."}</p>}
    {error && <p className="inline-error" role="alert">{error}</p>}
    <div className="data-table key-table" role="table" aria-label="API keys">
      <div className="data-head" role="row"><span role="columnheader">Name</span><span role="columnheader">Scope</span><span role="columnheader">Key</span><span role="columnheader">Last used</span><span role="columnheader">Expires</span><span role="columnheader"><span className="sr-only">Actions</span></span></div>
      {data.keys.length === 0 && <div className="data-row" role="row"><span role="cell" className="muted">No keys yet.</span></div>}
      {data.keys.map((key) => <div className={`data-row ${key.expired ? "expired" : ""}`} role="row" key={key.id} aria-label={key.name}>
        <span role="cell" className="key-name"><strong>{key.name}</strong>{key.source === "environment" && <Badge>Environment</Badge>}{mine(key) && <Badge tone="good">This console</Badge>}</span>
        <span role="cell"><Badge tone={key.scope === "operator" ? "warning" : "quiet"}>{scopeLabels[key.scope]}</Badge></span>
        <span role="cell">{key.prefix ? <code>{key.prefix}…</code> : <span className="muted">Set in the service environment</span>}</span>
        <span role="cell" className="muted">{key.source === "environment" ? "—" : key.lastUsed ? ago(key.lastUsed) : "Never"}</span>
        <span role="cell" className={key.expired ? "inline-error" : "muted"}>{key.expires ? until(key.expires) : "Never"}</span>
        <span role="cell" className="row-end">{key.source === "issued" ? <Button variant="danger" className="row-action" disabled={busy || !online} onClick={() => setRevoking(key)}>Revoke</Button> : <HelpTip label={`${key.name} environment key`}>Change or remove {key.name} in the service environment, then restart Juiced.</HelpTip>}</span>
      </div>)}
    </div>
    {revealed && <Dialog title="Copy the new key" onClose={() => setRevealed(null)}>
      <p>Store <strong>{revealed.key.name}</strong> now. Juiced keeps only a hash and can't show it again.</p>
      <div className="copy-row secret-row"><input readOnly aria-label="New API key" value={revealed.secret} onFocus={(event) => event.currentTarget.select()} /><Copy value={revealed.secret} label="Copy new key" /></div>
      {revealed.adopted && <p className="field-help">This console now uses this key in this tab. After a reload, enter it in Connection settings.</p>}
      <div className="dialog-actions"><Button variant="primary" onClick={() => setRevealed(null)}>Done</Button></div>
    </Dialog>}
    {revoking && <Dialog title={`Revoke ${revoking.name}?`} onClose={() => setRevoking(null)}>
      <p>Clients using this key stop working at once.</p>
      {mine(revoking) && !lastIssued && <p className="inline-warning">This console uses this key. Enter another operator key in Connection settings to keep managing Juiced.</p>}
      {lastIssued && <p className="inline-warning">It's the last key. Key checks turn off, and anyone on the network can use every API.</p>}
      <div className="dialog-actions"><Button onClick={() => setRevoking(null)}>Cancel</Button><Button variant="danger" disabled={busy} onClick={() => void revoke(revoking)}>Revoke key</Button></div>
    </Dialog>}
  </div>;
}
