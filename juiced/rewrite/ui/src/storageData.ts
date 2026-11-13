export type Volume = { path: string; paths: string[]; roles: string[]; total_bytes: number; available_bytes: number; used_bytes: number; read_only: boolean; low: boolean };
export type StorageWarning = { code: string; message: string; path?: string };
export type Category = { id: string; title: string; bytes: number; reclaimable_bytes: number; truncated?: boolean };
export type Reclaimable = { id: string; kind: string; title: string; path: string | null; bytes: number; reclaimable: boolean; refusal: string | null; message: string | null; references: string[] };
export type ModelUsage = { model: string; bytes: number; files: number; shared_bytes: number; read_only_bytes: number; removal_frees_bytes: number; shared_with: string[] };
export type SessionCache = { bytes: number; session_bytes: number | null; budget_bytes: number | null; sessions: number | null; over_budget: boolean };
export type StorageSummary = { volumes: Volume[]; warnings: StorageWarning[]; settings: { reserve_bytes: number; low_space_bytes: number } };
export type StorageView = StorageSummary & {
  revision: string; categories: Category[]; models: ModelUsage[]; unreferenced: Reclaimable[]; partials: Reclaimable[];
  engines: Reclaimable[]; legacy: Reclaimable[]; session_cache: SessionCache; truncated: boolean;
};
export type ReclaimPreview = { revision: string; freed_bytes: number; refused: number; items: Reclaimable[] };

const object = (value: unknown): Record<string, unknown> => value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
const count = (value: unknown): number => typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
const optional = (value: unknown): number | null => typeof value === "number" && Number.isFinite(value) ? value : null;
const text = (value: unknown): string => typeof value === "string" ? value : "";
const texts = (value: unknown): string[] => Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
const list = <T>(value: unknown, parse: (item: Record<string, unknown>) => T): T[] => Array.isArray(value) ? value.map((item) => parse(object(item))) : [];

function volume(row: Record<string, unknown>): Volume {
  return { path: text(row.path), paths: texts(row.paths), roles: texts(row.roles), total_bytes: count(row.total_bytes), available_bytes: count(row.available_bytes), used_bytes: count(row.used_bytes), read_only: row.read_only === true, low: row.low === true };
}

function reclaimable(row: Record<string, unknown>): Reclaimable {
  return { id: text(row.id), kind: text(row.kind), title: text(row.title) || text(row.id), path: typeof row.path === "string" ? row.path : null, bytes: count(row.bytes), reclaimable: row.reclaimable === true, refusal: typeof row.refusal === "string" ? row.refusal : null, message: typeof row.message === "string" ? row.message : null, references: texts(row.references) };
}

function summary(value: Record<string, unknown>): StorageSummary {
  if (!Array.isArray(value.volumes)) throw new Error("The storage view is invalid. Retry.");
  const settings = object(value.settings);
  return {
    volumes: list(value.volumes, volume),
    warnings: list(value.warnings, (row) => ({ code: text(row.code), message: text(row.message), path: typeof row.path === "string" ? row.path : undefined })),
    settings: { reserve_bytes: count(settings.reserve_bytes), low_space_bytes: count(settings.low_space_bytes) },
  };
}

export const parseStorageSummary = (value: unknown): StorageSummary => summary(object(value));

export function parseStorage(value: unknown): StorageView {
  const row = object(value);
  if (typeof row.revision !== "string" || !Array.isArray(row.categories)) throw new Error("The storage view is invalid. Retry.");
  const cache = object(row.session_cache);
  return {
    ...summary(row),
    revision: row.revision,
    categories: list(row.categories, (item) => ({ id: text(item.id), title: text(item.title), bytes: count(item.bytes), reclaimable_bytes: count(item.reclaimable_bytes), truncated: item.truncated === true })),
    models: list(row.models, (item) => ({ model: text(item.model), bytes: count(item.bytes), files: count(item.files), shared_bytes: count(item.shared_bytes), read_only_bytes: count(item.read_only_bytes), removal_frees_bytes: count(item.removal_frees_bytes), shared_with: texts(item.shared_with) })),
    unreferenced: list(row.unreferenced, reclaimable),
    partials: list(row.partials, reclaimable),
    engines: list(row.engines, reclaimable),
    legacy: list(row.legacy, reclaimable),
    session_cache: { bytes: count(cache.bytes), session_bytes: optional(cache.session_bytes), budget_bytes: optional(cache.budget_bytes), sessions: optional(cache.sessions), over_budget: cache.over_budget === true },
    truncated: row.truncated === true,
  };
}

export function parsePreview(value: unknown): ReclaimPreview {
  const row = object(value);
  if (typeof row.revision !== "string" || !Array.isArray(row.items)) throw new Error("The reclaim preview is invalid. Retry.");
  return { revision: row.revision, freed_bytes: count(row.freed_bytes), refused: count(row.refused), items: list(row.items, reclaimable) };
}

/** Everything the operator can choose to reclaim, largest first; refused rows stay visible with their reason. */
export function reclaimRows(view: StorageView): Reclaimable[] {
  return [...view.legacy, ...view.unreferenced, ...view.partials, ...view.engines.filter((row) => row.references.length === 0)]
    .sort((left, right) => Number(right.reclaimable) - Number(left.reclaimable) || right.bytes - left.bytes);
}

/** A short location: the Hugging Face repository for hub files, otherwise the last two folders. */
export function place(path: string | null): string | null {
  if (!path) return null;
  const hub = /models--([^/]+?)--([^/]+)/.exec(path);
  if (hub) return `${hub[1]}/${hub[2]}`;
  const parts = path.split("/").filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : path;
}

export const kindNames: Record<string, string> = { file: "Unused file", partial: "Partial download", engine: "Unused engine", legacy: "v0.9.2 leftover", model: "Model" };

export const roleNames: Record<string, string> = { data: "Data", models: "Models", engines: "Engines", session_cache: "Session cache", library: "Library" };

export function lowSpace(summary: StorageSummary | null): StorageWarning[] {
  return (summary?.warnings ?? []).filter((warning) => warning.code === "low_space" || warning.code === "session_cache_over_budget" || warning.code === "acquisition_journal_unwritable");
}
