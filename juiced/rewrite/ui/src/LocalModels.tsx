import { useEffect, useState } from "react";
import type { Client } from "./api";
import { bytes, type Model } from "./domain";
import { parseLocal, roleNames, type LocalFile, type LocalGroup, type LocalInventory } from "./localLibraryData";
import { useResource } from "./useResource";

export function useLocalFiles(client: Client, enabled: boolean) {
  const [fresh, setFresh] = useState(true);
  useEffect(() => { if (!enabled) setFresh(true); }, [enabled]);
  const local = useResource(client, fresh ? "juiced/library/local?fresh=true" : "juiced/library/local", parseLocal, 5000, true, enabled);
  useEffect(() => { if (local.data) setFresh(false); }, [local.data]);
  return local;
}

/** Complete quants on disk the service could not make into a model, with its reason. Files that can never be one are left to Storage. */
export function installFailures(inventory: LocalInventory | null | undefined): { name: string; message: string }[] {
  return (inventory?.groups ?? []).flatMap((group) => group.quants.filter((quant) => quant.usedBy.length === 0 && quant.install?.state === "failed").map((quant) => ({ name: `${group.name} ${quant.label}`, message: quant.install!.message ?? "It couldn't be installed." })));
}

export function modelFiles(model: Model, inventory: LocalInventory | null): { groups: LocalGroup[]; files: LocalFile[] } {
  const groups: LocalGroup[] = [], files: LocalFile[] = [];
  for (const group of inventory?.groups ?? []) {
    const parts = new Set(group.quants.filter((quant) => quant.usedBy.includes(model.id)).flatMap((quant) => quant.files));
    const used = group.files.filter((file) => file.usedBy.includes(model.id) || parts.has(file.id));
    if (used.length) { groups.push(group); files.push(...used); }
  }
  return { groups, files };
}

/** The files of the model's own weights, not of a companion such as a text encoder. */
export function weightsGroup(model: Pick<Model, "id">, inventory: LocalInventory | null): LocalGroup | null {
  const { groups } = modelFiles(model as Model, inventory);
  const id = model.id.toLowerCase();
  // A GGUF text encoder found by path reads as a model file too, so prefer the repository named like the model.
  const shared = (group: LocalGroup) => { const name = (group.repo?.split("/").pop() ?? group.name).toLowerCase(); let length = 0; while (length < name.length && name[length] === id[length]) length++; return length; };
  const weights = groups.filter((group) => group.files.some((file) => (file.role === "model" || file.role === "shard") && file.usedBy.includes(model.id)));
  return weights.reduce<LocalGroup | null>((best, group) => !best || shared(group) > shared(best) ? group : best, null);
}

export function ModelFiles({ model, inventory }: { model: Model; inventory: LocalInventory | null }) {
  const { groups, files } = modelFiles(model, inventory);
  if (!files.length) return null;
  const others = [...new Set(files.flatMap((file) => file.usedBy))].filter((id) => id !== model.id);
  const location = groups.length === 1 ? groups[0]!.location ?? files[0]!.location : null;
  return <section className="inspector-section"><h3>Files</h3>
    <ul className="file-list" aria-label="Model files">{files.map((file) => <li key={file.id}><span className="break-word">{file.name}</span><span className="muted">{file.quant ?? roleNames[file.role] ?? file.role}{file.unverified ? " · unverified" : ""}</span><span className="muted numeric">{bytes(file.bytes)}</span></li>)}</ul>
    <dl className="facts"><div><dt>On disk</dt><dd>{bytes(files.reduce((sum, file) => sum + file.bytes, 0))}{files.some((file) => file.readOnly) ? " · read-only" : ""}</dd></div>
      {location && <div className="fact-path"><dt>Location</dt><dd><code className="break-word">{location}</code></dd></div>}</dl>
    {others.length > 0 && <p className="field-help">Also used by {others.join(", ")}.</p>}
  </section>;
}
