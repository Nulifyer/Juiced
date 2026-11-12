import { useState } from "react";
import type { Client } from "./api";
import { policyNames, type Model } from "./domain";
import { Badge, Button, Empty, HelpTip, Icon } from "./ui";
import { useOperation, useResource } from "./useResource";

export type Alias = { alias: string; target: string; target_state: string | null; target_policy: string | null; available: boolean };
export type AliasView = { revision: number; maximum: number; aliases: Alias[] };

const text = (value: unknown) => typeof value === "string" ? value : null;
export function parseAliases(value: unknown): AliasView {
  const body = value && typeof value === "object" ? value as Record<string, unknown> : {};
  if (typeof body.revision !== "number" || !Array.isArray(body.aliases)) throw new Error("The service returned an unexpected alias list.");
  const aliases = body.aliases.flatMap((entry): Alias[] => {
    const item = entry && typeof entry === "object" ? entry as Record<string, unknown> : {};
    const alias = text(item.alias); const target = text(item.target);
    return alias && target ? [{ alias, target, target_state: text(item.target_state), target_policy: text(item.target_policy), available: item.available === true }] : [];
  }).sort((left, right) => left.alias.localeCompare(right.alias));
  return { revision: body.revision, maximum: typeof body.maximum === "number" ? body.maximum : 1024, aliases };
}

function targetNote(alias: Alias) {
  if (alias.available) return <Badge tone="good">Serving</Badge>;
  if (alias.target_policy === "disabled") return <Badge tone="warning">Target disabled</Badge>;
  if (alias.target_state === null) return <Badge tone="bad">Target missing</Badge>;
  return <Badge>Not downloaded</Badge>;
}

export function Aliases({ client, models, online, onOpenModel }: { client: Client; models: Model[]; online: boolean; onOpenModel: (id: string) => void }) {
  const view = useResource(client, "juiced/aliases", parseAliases, 3000);
  const operation = useOperation(client, view.reload);
  const [name, setName] = useState("");
  const [target, setTarget] = useState("");
  const data = view.data;
  const blocked = !online || !data || operation.busy;
  const ids = models.map((model) => model.id).sort((left, right) => left.localeCompare(right));
  const nameTaken = !!data?.aliases.some((alias) => alias.alias === name.trim()) || ids.includes(name.trim());
  async function save(alias: string, next: string | null, success: string) {
    if (!data) return false;
    return (await operation.run("juiced/aliases", { alias, target: next, expected_revision: data.revision }, success)) !== undefined;
  }
  async function create() {
    const alias = name.trim();
    if (!alias || !target || nameTaken) return;
    if (await save(alias, target, `${alias} now points to ${target}.`)) { setName(""); setTarget(""); }
  }
  if (!data) return view.error ? <div className="action-row"><p className="inline-error" role="alert">{view.error}</p><Button onClick={() => void view.reload()}>Retry</Button></div> : <p className="muted">Loading aliases…</p>;
  return <section className="page-body aliases" aria-label="Aliases">
    <form className="alias-create toolbar" aria-label="New alias" onSubmit={(event) => { event.preventDefault(); void create(); }}>
      <input aria-label="Alias name" placeholder="Alias, e.g. default-chat" value={name} disabled={blocked} onChange={(event) => setName(event.target.value)} />
      <select aria-label="Alias target" value={target} disabled={blocked} onChange={(event) => setTarget(event.target.value)}><option value="">Choose a model…</option>{ids.map((id) => <option key={id} value={id}>{id}</option>)}</select>
      <Button type="submit" variant="primary" disabled={blocked || !name.trim() || !target || nameTaken || data.aliases.length >= data.maximum}>Add alias</Button>
      <HelpTip label="Aliases">Clients send the alias as the model name. Swap the target here and new requests go to the new model; requests already running finish on the old one.</HelpTip>
    </form>
    {nameTaken && <p className="field-help">That name is already a model or an alias.</p>}
    {operation.error && <p className="inline-error" role="alert">{operation.error}</p>}
    {view.error && <p className="inline-warning">Showing the last list. {view.error}</p>}
    {data.aliases.length === 0 ? <Empty title="No aliases yet">Create one so clients can keep a stable model name while you swap the model behind it.</Empty>
      : <div className="data-table alias-table" role="table" aria-label="Alias list">
        <div role="row" className="data-head"><span role="columnheader">Alias</span><span role="columnheader">Points to</span><span role="columnheader">Status</span><span role="columnheader"><span className="sr-only">Actions</span></span></div>
        {data.aliases.map((alias) => <div role="row" className="data-row" key={alias.alias} aria-label={alias.alias}>
          <span role="cell"><code className="break-word">{alias.alias}</code></span>
          <span role="cell"><select aria-label={`Target for ${alias.alias}`} value={alias.target} disabled={blocked} onChange={(event) => void save(alias.alias, event.target.value, `${alias.alias} now points to ${event.target.value}.`)}>{!ids.includes(alias.target) && <option value={alias.target}>{alias.target}</option>}{ids.map((id) => <option key={id} value={id}>{id}</option>)}</select></span>
          <span role="cell" className="alias-status">{targetNote(alias)}{alias.target_policy && alias.target_policy !== "disabled" && <span className="muted">{policyNames[alias.target_policy as keyof typeof policyNames] ?? alias.target_policy}</span>}</span>
          <span role="cell" className="row-end"><Button variant="ghost" disabled={!ids.includes(alias.target)} onClick={() => onOpenModel(alias.target)}>Open model</Button><Button variant="ghost" aria-label={`Remove ${alias.alias}`} disabled={blocked} onClick={() => void save(alias.alias, null, `Removed ${alias.alias}.`)}><Icon name="close" size={14} /></Button></span>
        </div>)}
      </div>}
  </section>;
}
