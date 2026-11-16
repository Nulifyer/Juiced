import { useEffect, useMemo, useState } from "react";
import type { Client } from "./api";
import { bytes, policyNames, type Model } from "./domain";
import type { DiscoverTarget } from "./FindModels";
import { engineNames, featured, parseSearch, progress, type LibraryModel } from "./libraryData";
import type { LocalGroup, LocalInventory } from "./localLibraryData";
import { weightsGroup } from "./LocalModels";
import { Button, HelpTip } from "./ui";
import { useResource } from "./useResource";

export type QuantRow =
  | { kind: "current"; key: string; label: string; bytes: number | null }
  | { kind: "added"; key: string; label: string; bytes: number | null; model: string; policy: string | null; shared: boolean }
  | { kind: "file"; key: string; label: string; bytes: number | null; file: string }
  | { kind: "remote"; key: string; label: string; bytes: number | null; file: string; progress: number | null };

const baseName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** One size-ordered list: this model's quant, the others on disk, and a few that can still be downloaded. */
export function quantRows(model: Pick<Model, "id">, models: Pick<Model, "id" | "policy">[], group: LocalGroup, hub: LibraryModel | null): { rows: QuantRow[]; more: number } {
  const rows: QuantRow[] = [];
  for (const quant of group.quants) {
    const others = quant.usedBy.filter((id) => id !== model.id);
    if (quant.usedBy.includes(model.id)) rows.push({ kind: "current", key: `disk:${quant.file}`, label: quant.label, bytes: quant.bytes });
    else if (others.length === 0) rows.push({ kind: "file", key: `disk:${quant.file}`, label: quant.label, bytes: quant.bytes, file: quant.file });
    else for (const id of others) rows.push({ kind: "added", key: `disk:${quant.file}:${id}`, label: quant.label, bytes: quant.bytes, model: id, policy: models.find((entry) => entry.id === id)?.policy ?? null, shared: others.length > 1 });
  }
  const onDisk = new Set(group.files.map((file) => file.name));
  const missing = (hub?.quants ?? []).filter((quant) => !quant.present && !onDisk.has(baseName(quant.file)));
  const offered = featured(missing.filter((quant) => quant.pullable || progress(quant) !== null));
  for (const quant of offered) {
    rows.push({ kind: "remote", key: `hub:${quant.kind}:${quant.engine ?? ""}:${quant.file}`, label: quant.kind === "checkpoint" && quant.engine ? `${quant.label} · ${engineNames[quant.engine] ?? quant.engine}` : quant.label, bytes: quant.bytes, file: quant.file, progress: progress(quant) });
  }
  const size = (row: QuantRow) => row.bytes ?? Number.MAX_SAFE_INTEGER;
  return { rows: rows.map((row, index) => [row, index] as const).sort(([left, a], [right, b]) => size(left) - size(right) || a - b).map(([row]) => row), more: missing.length - offered.length };
}

/** The other quants of a model: the ones on this machine and the ones Discover can download. */
export function ModelQuants({ model, models, inventory, client, online, onOpenModel, onOpenFile, onDiscover }: { model: Model; models: Model[]; inventory: LocalInventory | null; client: Client; online: boolean; onOpenModel: (id: string) => void; onOpenFile: (group: string, quant: string) => void; onDiscover: (target: DiscoverTarget) => void }) {
  const group = weightsGroup(model, inventory);
  const repo = group?.repo && group.repo.length <= 128 ? group.repo : null;
  const path = `juiced/library/search?${new URLSearchParams({ repo: repo ?? "", limit: "1" })}`;
  const [period, setPeriod] = useState(0);
  const search = useResource(client, path, parseSearch, period, true, !!repo && online);
  const hub = useMemo(() => search.data?.models.find((entry) => entry.repo.toLowerCase() === repo?.toLowerCase()) ?? null, [search.data, repo]);
  const downloading = hub?.quants.some((quant) => progress(quant) !== null) ?? false;
  useEffect(() => { setPeriod(downloading ? 2000 : 0); }, [downloading]);
  const view = useMemo(() => group ? quantRows(model, models, group, hub) : null, [model, models, group, hub]);
  if (!group || !view) return null;
  const { rows, more } = view;
  const reachable = !!search.data && (search.data.hub === "ok" || search.data.hub === "skipped");
  const others = rows.some((row) => row.kind !== "current") || more > 0;
  const note = !repo ? "Not from Hugging Face, so no downloads are listed."
    : !online ? null
    : !search.data ? search.error ? null : "Checking Hugging Face…"
    : search.data.offline ? "Offline mode. Only quants on this machine are listed."
    : !reachable ? "Hugging Face is unreachable. Only quants on this machine are listed."
    : !hub ? `Hugging Face has no repository named ${repo}.`
    : !others ? "No other quants."
    : null;
  return <section className="inspector-section model-quants" aria-label="Quants">
    <div className="page-title"><h3>Quants</h3><HelpTip label="Quants">Other sizes of {group.name}. Each added quant is its own model with its own settings and loading policy. Get opens Discover, where you choose the loading policy before the download starts.</HelpTip></div>
    <ul className="model-quant-list" aria-label={`Quants of ${group.name}`}>{rows.map((row) => <li key={row.key} className={row.kind === "current" ? "current" : undefined}>
      <span className="model-quant-name"><strong>{row.label}</strong>{row.kind === "added" && row.shared && <span className="muted break-word">{row.model}</span>}</span>
      <span className="muted numeric">{bytes(row.bytes)}</span>
      <span className="model-quant-state muted">{row.kind === "current" ? "This model" : row.kind === "added" ? row.policy && Object.hasOwn(policyNames, row.policy) ? policyNames[row.policy as keyof typeof policyNames] : "Added" : row.kind === "file" ? "Not added" : row.progress !== null ? `Downloading ${Math.round(row.progress)}%` : ""}</span>
      <span className="model-quant-action">
        {row.kind === "added" && <Button className="row-action" aria-label={`Open ${row.model}`} title={row.model} onClick={() => onOpenModel(row.model)}>Open</Button>}
        {row.kind === "file" && <Button className="row-action" aria-label={`Add ${row.label}`} onClick={() => onOpenFile(group.id, row.file)}>Add</Button>}
        {row.kind === "remote" && repo && <Button className="row-action" aria-label={row.progress !== null ? `View ${row.label} in Discover` : `Get ${row.label}`} onClick={() => onDiscover({ repo, file: row.file })}>{row.progress !== null ? "View" : "Get"}</Button>}
      </span>
      {row.kind === "remote" && row.progress !== null && <span className="quant-progress" style={{ width: `${row.progress}%` }} />}
    </li>)}</ul>
    {repo && more > 0 && <button className="link-button model-quant-more" onClick={() => onDiscover({ repo })}>+{more} more in Discover</button>}
    {note && <p className="field-help" role="status">{note}</p>}
    {repo && online && (search.error || (search.data && !search.data.offline && !reachable)) && <div className="action-row">{search.error && <p className="inline-error" role="alert">Couldn't check Hugging Face. {search.error}</p>}<Button disabled={search.loading} onClick={() => void search.reload()}>Retry</Button></div>}
  </section>;
}
