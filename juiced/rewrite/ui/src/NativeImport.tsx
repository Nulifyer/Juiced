import { useEffect, useState } from "react";
import { ApiError, type Client } from "./api";
import { bytes, policyNames, type Policy } from "./domain";
import { installEngine, type EngineJob } from "./engines";
import { backendNames } from "./hardware";
import { notify } from "./toast";
import { Badge, Button, Empty, HelpTip, Icon } from "./ui";

type Setting = { scope: string; key: string; state: string; value?: unknown; reason: string };
type Install = { engine: string; backend: string };
type Row = {
  native_id: string; proposed_id: string; recipe: string | null; checkpoint: string | null;
  weights: { found: true; path: string; bytes: number } | { found: false; reason: string };
  engine: { engine: string; backend: string; installed: boolean; install: Install | null } | null;
  settings: Setting[]; previous_policy: Policy | null; suggested_policy: Policy;
  status: "ready" | "engine_install_needed" | "blocked" | "already_imported"; importable: boolean; already_imported: string | null;
  conflicts: { kind: string; id: string; reason: string }[]; blockers: { code: string; message: string }[];
};
type Preview = { source: string; configuration_generation: number; models: Row[]; aliases: { alias: string; target: string; state: string }[]; global_settings: Setting[]; truncated: boolean };
type Listing = { enabled: boolean; directory: string | null; parent: string | null; entries: { name: string; path: string; kind: string }[] };
type Choice = { include: boolean; policy: Policy };
type Applied = { native_id: string; id: string; status: string; policy_error?: { message: string } };

const statusTone = { ready: "good", engine_install_needed: "warning", blocked: "bad", already_imported: "quiet" } as const;
const statusName = { ready: "Ready", engine_install_needed: "Needs engine", blocked: "Can't import", already_imported: "Already added" };
const failure = (error: unknown, fallback: string) => error instanceof Error ? error.message : fallback;

function settingsNote(settings: Setting[]) {
  const mapped = settings.filter((item) => item.state === "mapped").length;
  const skipped = settings.filter((item) => ["unsupported", "ignored", "unknown"].includes(item.state));
  return { mapped, skipped };
}

export function NativeImport({ client, online, onImported }: { client: Client; online: boolean; onImported: () => Promise<void> }) {
  const [dir, setDir] = useState("");
  const [listing, setListing] = useState<Listing | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [choices, setChoices] = useState<Record<string, Choice>>({});
  const [aliases, setAliases] = useState(true);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [applied, setApplied] = useState<Applied[] | null>(null);
  const [installing, setInstalling] = useState<EngineJob | null>(null);
  useEffect(() => {
    if (preview) return;
    const controller = new AbortController();
    client.request<Listing>(`juiced/library/import-path${dir ? `?dir=${encodeURIComponent(dir)}` : ""}`, undefined, controller.signal)
      .then(setListing).catch((caught) => { if (!controller.signal.aborted) setError(failure(caught, "The folder couldn't be listed.")); });
    return () => controller.abort();
  }, [client, dir, preview]);
  async function inspect(source: string) {
    setBusy("preview"); setError(""); setApplied(null);
    try {
      const next = await client.request<Preview>("juiced/import/native", { action: "preview", source });
      setPreview(next);
      setChoices((old) => Object.fromEntries(next.models.map((row) => [row.native_id, old[row.native_id] ?? { include: row.status === "ready" || row.status === "engine_install_needed", policy: "disabled" }])));
    } catch (caught) { setError(failure(caught, "The folder couldn't be read.")); }
    finally { setBusy(""); }
  }
  async function apply() {
    if (!preview) return;
    setBusy("apply"); setError("");
    try {
      const models = preview.models.filter((row) => row.importable && choices[row.native_id]?.include).map((row) => ({ native_id: row.native_id, id: row.proposed_id, policy: choices[row.native_id]!.policy, include: true }));
      const result = await client.request<{ models: Applied[] }>("juiced/import/native", { action: "apply", source: preview.source, expected_generation: preview.configuration_generation, aliases, models });
      setApplied(result.models);
      const count = result.models.filter((row) => row.status === "imported").length;
      notify(count ? `Imported ${count} ${count === 1 ? "model" : "models"}.` : "Nothing new to import.");
      await onImported();
      await inspect(preview.source);
    } catch (caught) { setError(caught instanceof ApiError && caught.code === "revision_conflict" ? "The model list changed. Review the refreshed preview and import again." : failure(caught, "The import failed.")); if (caught instanceof ApiError && caught.code === "revision_conflict") await inspect(preview.source); }
    finally { setBusy(""); }
  }
  async function install(target: Install) {
    if (!preview) return;
    setBusy("install"); setError("");
    try { await installEngine(client, target.engine, target.backend, setInstalling); notify(`Installed ${target.engine} ${backendNames[target.backend] ?? target.backend}.`); await inspect(preview.source); }
    catch (caught) { setError(failure(caught, "The engine didn't install.")); }
    finally { setInstalling(null); setBusy(""); }
  }

  if (!preview) {
    if (listing && !listing.enabled) return <Empty title="No import folder set">Mount your old Juiced data under <code>/data/import</code> in the container, or set <code>import_roots</code> in the service config.</Empty>;
    return <section className="page-body native-import" aria-label="Import from Juiced 0.9">
      <div className="page-title"><h2>Pick your old Juiced config folder</h2><HelpTip label="Import">Choose the <code>config</code> folder from a Juiced 0.9 or lemonade data volume. Models keep using their weights where they are; nothing in the old folder changes. Imported models start Disabled unless you pick another policy.</HelpTip></div>
      <div className="folder-path">{listing?.directory && <Button variant="ghost" aria-label="Up one folder" onClick={() => setDir(listing.parent ?? "")}><Icon name="back" size={14} /></Button>}<code className="break-word">{listing?.directory ?? "Import folders"}</code>{listing?.directory && <Button variant="primary" disabled={!online || !!busy} onClick={() => void inspect(listing.directory!)}>{busy ? "Reading…" : "Use this folder"}</Button>}</div>
      {error && <p className="inline-error" role="alert">{error}</p>}
      <ul className="folder-list" aria-label="Folders">{(listing?.entries ?? []).filter((entry) => entry.kind === "directory").map((entry) => <li key={entry.path}><button onClick={() => setDir(entry.path)}><Icon name="library" size={14} /><span className="break-word">{entry.name}</span></button></li>)}</ul>
      {listing && !listing.entries.some((entry) => entry.kind === "directory") && <p className="muted">No folders here.</p>}
    </section>;
  }

  const selected = preview.models.filter((row) => row.importable && choices[row.native_id]?.include);
  const importableAliases = preview.aliases.filter((alias) => alias.state === "importable");
  const skipped = preview.global_settings.filter((item) => ["unsupported", "ignored", "unknown"].includes(item.state));
  return <section className="page-body native-import" aria-label="Import from Juiced 0.9">
    <div className="folder-path"><Button variant="ghost" aria-label="Pick another folder" disabled={!!busy} onClick={() => { setPreview(null); setApplied(null); }}><Icon name="back" size={14} /></Button><code className="break-word">{preview.source}</code></div>
    {error && <p className="inline-error" role="alert">{error}</p>}
    {preview.models.length === 0 ? <Empty title="No models found">This folder has no Juiced model settings.</Empty> : <div className="data-table import-table" role="table" aria-label="Models to import">
      <div className="data-head" role="row"><span role="columnheader"><span className="sr-only">Include</span></span><span role="columnheader">Model</span><span role="columnheader">Weights</span><span role="columnheader">Engine</span><span role="columnheader">Make available as</span></div>
      {preview.models.map((row) => {
        const choice = choices[row.native_id] ?? { include: false, policy: "disabled" as Policy };
        const notes = settingsNote(row.settings);
        const set = (change: Partial<Choice>) => setChoices({ ...choices, [row.native_id]: { ...choice, ...change } });
        const result = applied?.find((item) => item.native_id === row.native_id);
        return <div className="data-row" role="row" key={row.native_id} aria-label={row.native_id}>
          <span role="cell"><input type="checkbox" aria-label={`Import ${row.native_id}`} checked={row.importable && choice.include} disabled={!row.importable || !!busy} onChange={(event) => set({ include: event.target.checked })} /></span>
          <span role="cell" className="break-word"><strong>{row.proposed_id}</strong> <Badge tone={statusTone[row.status]}>{statusName[row.status]}</Badge>
            <span className="field-help">{[row.checkpoint, notes.mapped ? `${notes.mapped} ${notes.mapped === 1 ? "setting" : "settings"} carried over` : null, notes.skipped.length ? `${notes.skipped.length} not supported` : null].filter(Boolean).join(" · ")}</span>
            {row.blockers.map((blocker) => <span className="field-help inline-warning" key={blocker.code}>{blocker.message}</span>)}
            {row.conflicts.map((conflict) => <span className="field-help inline-warning" key={conflict.kind + conflict.id}>{conflict.reason}</span>)}
            {result?.policy_error && <span className="field-help inline-warning">{result.policy_error.message}</span>}</span>
          <span role="cell" className="muted">{row.weights.found ? bytes(row.weights.bytes) : "Missing"}</span>
          <span role="cell">{row.engine ? <>{row.engine.engine} {backendNames[row.engine.backend] ?? row.engine.backend}{!row.engine.installed && row.engine.install && <Button variant="ghost" disabled={!online || !!busy} onClick={() => void install(row.engine!.install!)}>Install</Button>}</> : <span className="muted">None</span>}</span>
          <span role="cell"><select aria-label={`Policy for ${row.native_id}`} value={choice.policy} disabled={!row.importable || !choice.include || !!busy} onChange={(event) => set({ policy: event.target.value as Policy })}>{(["disabled", "on-demand", "hot"] as Policy[]).map((policy) => <option key={policy} value={policy}>{policyNames[policy]}{row.previous_policy === policy ? " (before)" : row.suggested_policy === policy && policy !== "disabled" && !row.previous_policy ? " (suggested)" : ""}</option>)}</select></span>
        </div>;
      })}
    </div>}
    {installing && <><progress aria-label="Engine install progress" max={installing.total_bytes ?? 1} value={installing.total_bytes ? installing.downloaded_bytes : undefined} /><p className="field-help">Installing engine… {installing.state}</p></>}
    {importableAliases.length > 0 && <label className="inline-field"><input type="checkbox" checked={aliases} disabled={!!busy} onChange={(event) => setAliases(event.target.checked)} /> Also import {importableAliases.length} {importableAliases.length === 1 ? "alias" : "aliases"}</label>}
    {skipped.length > 0 && <p className="field-help">Not carried over: {skipped.map((item) => item.key).join(", ")}.</p>}
    <div className="action-row"><Button variant="primary" disabled={!online || !!busy || selected.length === 0} onClick={() => void apply()}>{busy === "apply" ? "Importing…" : `Import ${selected.length} ${selected.length === 1 ? "model" : "models"}`}</Button><span className="muted">Weights stay where they are. The old folder isn't changed.</span></div>
  </section>;
}
