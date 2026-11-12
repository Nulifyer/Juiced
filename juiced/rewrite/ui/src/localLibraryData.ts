export type Storage = "huggingface_cache" | "artifact_store" | "local";
export type AddSource = { path: string } | { artifact: string } | { artifact_set: string };
export type LocalFile = { id: string; name: string; path: string; quant: string | null; role: string; bytes: number; location: string; storage: Storage; artifact: string | null; readOnly: boolean; removable: boolean; refusal: string | null; usedBy: string[] };
export type LocalQuant = { label: string; file: string; parts: number; complete: boolean; bytes: number; files: string[]; usedBy: string[]; removable: boolean; add: AddSource | null };
export type LocalGroup = { id: string; repo: string | null; name: string; publisher: string | null; storage: Storage[]; location: string | null; readOnly: boolean; bytes: number; removableBytes: number; usedBy: string[]; quants: LocalQuant[]; files: LocalFile[] };
export type LocalInventory = { totalBytes: number; removableBytes: number; truncated: boolean; groups: LocalGroup[] };

const text = (value: unknown) => typeof value === "string" ? value : null;
const number = (value: unknown) => typeof value === "number" && Number.isFinite(value) ? value : 0;
const strings = (value: unknown, limit = 64) => Array.isArray(value) ? value.filter((item): item is string => typeof item === "string").slice(0, limit) : [];
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The service returned an invalid local library response.");
  return value as Record<string, unknown>;
}
const list = (value: unknown, limit: number) => Array.isArray(value) ? value.slice(0, limit).map(record) : [];
const storage = (value: unknown): Storage => value === "artifact_store" || value === "local" ? value : "huggingface_cache";

function addSource(value: unknown): AddSource | null {
  if (!value || typeof value !== "object") return null;
  const item = value as Record<string, unknown>;
  if (typeof item.path === "string") return { path: item.path };
  if (typeof item.artifact_set === "string") return { artifact_set: item.artifact_set };
  if (typeof item.artifact === "string") return { artifact: item.artifact };
  return null;
}

export function parseLocal(value: unknown): LocalInventory {
  const item = record(value);
  return {
    totalBytes: number(item.total_bytes), removableBytes: number(item.removable_bytes), truncated: item.truncated === true,
    groups: list(item.groups, 512).map((group) => ({
      id: text(group.id) ?? "", repo: text(group.repo), name: text(group.name) ?? "", publisher: text(group.publisher), storage: strings(group.storage, 4).map(storage),
      location: text(group.location), readOnly: group.read_only === true, bytes: number(group.bytes), removableBytes: number(group.removable_bytes), usedBy: strings(group.used_by),
      quants: list(group.quants, 128).map((quant) => ({ label: text(quant.label) ?? "File", file: text(quant.file) ?? "", parts: number(quant.parts) || 1, complete: quant.complete !== false, bytes: number(quant.bytes), files: strings(quant.files, 128), usedBy: strings(quant.used_by), removable: quant.removable === true, add: addSource(quant.add) })),
      files: list(group.files, 512).map((file) => ({ id: text(file.id) ?? "", name: text(file.name) ?? "", path: text(file.path) ?? "", quant: text(file.quant), role: text(file.role) ?? "model", bytes: number(file.bytes), location: text(file.location) ?? "", storage: storage(file.storage), artifact: text(file.artifact), readOnly: file.read_only === true, removable: file.removable === true, refusal: text(file.refusal), usedBy: strings(file.used_by) })),
    })),
  };
}

export const roleNames: Record<string, string> = { model: "Model", shard: "Part", projector: "Vision projector", vae: "VAE", text_encoder: "Text encoder", voices: "Voices", tokenizer: "Tokenizer", config: "Config" };
export const storageNames: Record<Storage, string> = { huggingface_cache: "Hugging Face cache", artifact_store: "Juiced storage", local: "Folder" };

export function refusalText(file: Pick<LocalFile, "refusal" | "usedBy">): string | undefined {
  switch (file.refusal) {
    case null: return undefined;
    case "read_only": return "This folder is mounted read-only.";
    case "in_use": return `Used by ${file.usedBy.join(", ")}. Remove the model first.`;
    case "not_owned": return "Juiced only deletes files in its own data folder.";
    default: return "A worker or download is using this file.";
  }
}

export function quantRefusal(group: LocalGroup, quant: LocalQuant): string | undefined {
  if (quant.removable) return undefined;
  const blocked = group.files.find((file) => quant.files.includes(file.id) && !file.removable);
  return blocked ? refusalText(blocked) : "This quant can't be deleted.";
}

export type PullFile = { file: string; bytes: number; state: string; downloaded: number; job: string | null };
export type PullView = { id: string; state: string; error: string | null; repo: string; label: string | null; files: PullFile[] };

export function parsePulls(value: unknown): PullView[] {
  return list(record(value).data, 64).map((pull) => ({
    id: text(pull.pull) ?? "", state: text(pull.state) ?? "unknown", error: text(pull.error), repo: text(pull.repo) ?? "", label: text(pull.label),
    files: list(pull.files, 64).map((file) => ({ file: text(file.file) ?? "", bytes: number(file.bytes), state: text(file.state) ?? "", downloaded: file.state === "present" ? number(file.bytes) : number(file.downloaded_bytes), job: text(file.job) })),
  }));
}
