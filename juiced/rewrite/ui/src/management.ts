import { parseArtifactSets, type ArtifactSet } from "./artifactSetData";
import type { Model } from "./domain";
import type { LaunchOptions, Provider } from "./profiles";

export type ArtifactJob = {
  id: string; revision: number; state: string; source_url: string | null; expected_sha256: string;
  expected_bytes: number | null; maximum_bytes: number; downloaded_bytes: number; artifact: string | null;
  error: string | null; writer_active: boolean;
};
export type Artifact = { id: string; bytes: number; sha256: string; references: string[]; reference_count: number; active_holds: number; removing: boolean };
export const inventorySections = ["jobs", "artifacts", "artifact_sets", "quarantined_files"] as const;
export type InventorySection = typeof inventorySections[number];
export type InventoryPageInfo = { total: number; next_after: string | null };
export type ArtifactInventory = {
  generation: number; view: string; limit: number; source_policy: string; charged_bytes: number;
  jobs: ArtifactJob[]; artifacts: Artifact[]; artifact_sets: ArtifactSet[]; quarantined_files: { filename: string; reserved_bytes: number; reason: string }[];
  pages: Record<InventorySection, InventoryPageInfo | null>;
  selection: { artifact: Artifact | null; artifact_set: ArtifactSet | null };
};
export type InventoryCursors = { view: string | null; stacks: Record<InventorySection, string[]> };
export const firstInventoryPages = (): InventoryCursors => ({ view: null, stacks: { jobs: [], artifacts: [], artifact_sets: [], quarantined_files: [] } });
export function inventoryPath(cursors: InventoryCursors, selection: { artifact?: string; artifact_set?: string } = {}, section?: InventorySection, limit = 64) {
  const params = new URLSearchParams({ limit: String(limit) });
  if (section) params.set("section", section);
  for (const name of inventorySections) { const after = cursors.stacks[name].at(-1); if (after !== undefined) params.set(`${name}_after`, after); }
  if (cursors.view && inventorySections.some((name) => cursors.stacks[name].length > 0)) params.set("expected_view", cursors.view);
  if (selection.artifact) params.set("artifact", selection.artifact);
  if (selection.artifact_set) params.set("artifact_set", selection.artifact_set);
  return `juiced/artifacts?${params}`;
}
export function nextInventoryPage(cursors: InventoryCursors, inventory: ArtifactInventory, section: InventorySection): InventoryCursors {
  const after = inventory.pages[section]?.next_after;
  if (!after || (cursors.view !== null && cursors.view !== inventory.view)) return cursors;
  return { view: inventory.view, stacks: { ...cursors.stacks, [section]: [...cursors.stacks[section], after] } };
}
export function previousInventoryPage(cursors: InventoryCursors, section: InventorySection): InventoryCursors {
  const stacks = { ...cursors.stacks, [section]: cursors.stacks[section].slice(0, -1) };
  return { view: inventorySections.some((name) => stacks[name].length > 0) ? cursors.view : null, stacks };
}
export type RetainedReader = { id: string; revision: number; writer_current_owner: boolean; writer_actor_present: boolean; release_eligible?: boolean };
export type CacheSnapshot = { id: string; revision: number; state: string; reserved_bytes: number; bytes: number | null; protocol: string; session_digest: string; active_readers: number; writer_bound?: boolean; writer_current_owner?: boolean; writer_actor_present?: boolean; cleanup_eligible?: boolean; retained_readers?: RetainedReader[] };
export type CachePage = { view: string; limit: number; next_after: string | null };
export type OrphanPage = Omit<CachePage, "view"> & { observed_count: number; scan_complete: boolean };
export type CacheView = { schema_version: number; enabled: boolean; configuration_generation: number; generation: number; qualification: string; data: CacheSnapshot[]; page?: CachePage };
export type CacheInspection = { generation: number; page?: CachePage; orphan_page?: OrphanPage; read_only: boolean; recovery_authorized: boolean; charged_bytes: number; snapshots: { snapshot: CacheSnapshot; issues: string[]; checksum_valid: boolean | null }[]; orphan_files: string[]; issues: string[] };
export type Protocol = { id: string; support: string; operations?: string[]; note?: string };
export type Endpoint = { method: string; path: string; authority: string; prefixes: string[] };

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The service returned an invalid management response. Check its version, then refresh.");
  return value as Record<string, unknown>;
}
function integer(value: unknown): value is number { return Number.isSafeInteger(value) && (value as number) >= 0; }
function strings(value: unknown): value is string[] { return Array.isArray(value) && value.every((item) => typeof item === "string"); }
function artifactRow(value: unknown): value is Artifact {
  if (!value || typeof value !== "object") return false;
  const artifact = value as Record<string, unknown>;
  return typeof artifact.id === "string" && /^[a-f0-9]{64}$/.test(artifact.id) && artifact.sha256 === artifact.id && integer(artifact.bytes) && strings(artifact.references) && artifact.references.length <= 64 && integer(artifact.reference_count) && artifact.reference_count >= artifact.references.length && integer(artifact.active_holds) && typeof artifact.removing === "boolean";
}
function pageInfo(value: unknown, rows: number, limit: number): boolean {
  if (value === null) return rows === 0;
  const page = record(value);
  return integer(page.total) && rows <= limit && rows <= page.total && (page.next_after === null || (typeof page.next_after === "string" && page.next_after.length > 0 && page.next_after.length <= 255));
}
export function parseArtifactInventory(value: unknown): ArtifactInventory {
  const item = record(value);
  if (!integer(item.generation) || !integer(item.charged_bytes) || typeof item.source_policy !== "string" || typeof item.view !== "string" || !/^[a-f0-9]{64}$/.test(item.view) || !integer(item.limit) || item.limit < 1 || item.limit > 128 || !Array.isArray(item.jobs) || !Array.isArray(item.artifacts) || !Array.isArray(item.quarantined_files)) throw new Error("The artifact inventory is invalid. Refresh before changing stored files.");
  const limit = item.limit;
  for (const job of item.jobs) {
    if (!job || typeof job.id !== "string" || !/^[a-zA-Z0-9]{32}$/.test(job.id) || !integer(job.revision) || typeof job.state !== "string" || !integer(job.downloaded_bytes) || !integer(job.maximum_bytes) || typeof job.writer_active !== "boolean") throw new Error("The artifact job inventory is invalid. Refresh before changing jobs.");
  }
  if (!item.artifacts.every(artifactRow)) throw new Error("The artifact inventory is invalid. Refresh before removing files.");
  const sets = parseArtifactSets(item.artifact_sets, item.artifacts, limit);
  for (const file of item.quarantined_files) {
    if (!file || typeof file.filename !== "string" || !/^(job-[a-zA-Z0-9]{32}\.part|artifact-[a-f0-9]{64}\.bin)$/.test(file.filename) || !integer(file.reserved_bytes) || typeof file.reason !== "string" || file.reason.length > 128) throw new Error("The retained file inventory is invalid. Refresh before changing stored files.");
  }
  const pages = record(item.pages), selection = record(item.selection);
  const rows: Record<InventorySection, number> = { jobs: item.jobs.length, artifacts: item.artifacts.length, artifact_sets: sets.length, quarantined_files: item.quarantined_files.length };
  if (!inventorySections.every((name) => pageInfo(pages[name] ?? null, rows[name], limit))) throw new Error("The service returned an invalid library page. Refresh from the first page.");
  if (selection.artifact !== null && !artifactRow(selection.artifact)) throw new Error("The selected weight file is invalid. Refresh before registering it.");
  if (selection.artifact_set !== null) parseArtifactSets([selection.artifact_set], selection.artifact ? [selection.artifact] : [], 1);
  return value as ArtifactInventory;
}
function cacheSnapshots(value: unknown): CacheSnapshot[] {
  if (!Array.isArray(value) || value.length > 16384) throw new Error("The snapshot inventory exceeds its row bound.");
  const data = value;
  for (const snapshot of data) {
    if (snapshot?.retained_readers !== undefined) {
      const readers = snapshot.retained_readers;
      if (!Array.isArray(readers) || readers.length > 16384 || new Set(readers.map((reader) => reader?.id)).size !== readers.length || !readers.every((reader) => reader && typeof reader.id === "string" && /^[a-z0-9]{32}$/.test(reader.id) && integer(reader.revision) && reader.revision > 0 && typeof reader.writer_current_owner === "boolean" && typeof reader.writer_actor_present === "boolean" && (reader.release_eligible === undefined || typeof reader.release_eligible === "boolean"))) throw new Error("The service returned invalid restore protection. Refresh before recovering or forgetting snapshots.");
    }
    if (!snapshot || typeof snapshot.id !== "string" || !/^[a-zA-Z0-9]{32}$/.test(snapshot.id) || !integer(snapshot.revision) || typeof snapshot.state !== "string" || !integer(snapshot.reserved_bytes) || typeof snapshot.protocol !== "string" || typeof snapshot.session_digest !== "string" || !integer(snapshot.active_readers) || [snapshot.writer_bound, snapshot.writer_current_owner, snapshot.writer_actor_present, snapshot.cleanup_eligible].some((flag) => flag !== undefined && typeof flag !== "boolean")) throw new Error("The service returned invalid snapshot metadata. Refresh before restoring or forgetting snapshots.");
  }
  if (new Set(data.map(snapshot => snapshot.id)).size !== data.length) throw new Error("The snapshot inventory repeats an identifier.");
  return data as CacheSnapshot[];
}
type PageWindow = Pick<CachePage, "limit" | "next_after">;
function cachePage(value: unknown, orphan?: false): CachePage | undefined;
function cachePage(value: unknown, orphan: true): PageWindow | undefined;
function cachePage(value: unknown, orphan = false): PageWindow | undefined {
  if (value === undefined) return undefined;
  const page = record(value);
  const next = page.next_after;
  if ((!orphan && (typeof page.view !== "string" || !/^[a-f0-9]{64}$/.test(page.view))) || !integer(page.limit) || page.limit < 1 || page.limit > 16384 || (next !== null && (typeof next !== "string" || !(orphan ? next.length > 0 && next.length <= 4096 : /^[a-zA-Z0-9]{32}$/.test(next))))) throw new Error("The service returned an invalid cache continuation. Refresh from the first page.");
  return page as CachePage;
}
function validatePageRows(page: PageWindow | undefined, ids: string[]) {
  if (page && (ids.length > page.limit || ids.some((id,index) => index > 0 && id <= ids[index - 1]!) || (page.next_after !== null && (ids.length !== page.limit || page.next_after !== ids.at(-1))))) throw new Error("The cache page does not match its continuation. Refresh from the first page.");
}
export function parseCacheView(value: unknown): CacheView {
  const item = record(value);
  if (item.schema_version !== 1 || typeof item.enabled !== "boolean" || !integer(item.configuration_generation) || !integer(item.generation) || typeof item.qualification !== "string") throw new Error("The snapshot inventory is invalid. Refresh before changing cache policy.");
  const data = cacheSnapshots(item.data);
  const page = cachePage(item.page);
  validatePageRows(page, data.map(snapshot => snapshot.id));
  return { ...item, data, ...(page ? { page } : {}) } as CacheView;
}
export function parseCacheInspection(value: unknown): CacheInspection {
  const item = record(value);
  if (item.read_only !== true || item.recovery_authorized !== false || !integer(item.generation) || !integer(item.charged_bytes) || !Array.isArray(item.snapshots) || !Array.isArray(item.orphan_files) || !strings(item.issues)) throw new Error("The service returned an invalid storage inspection. Check its version.");
  const rows = item.snapshots as Record<string, unknown>[];
  const data = cacheSnapshots(rows.map(row => row?.snapshot));
  if (!rows.every(row => strings(row.issues) && (row.checksum_valid === null || typeof row.checksum_valid === "boolean")) || item.orphan_files.length > 32784 || !item.orphan_files.every(name => typeof name === "string" && name.length > 0 && name.length <= 4096)) throw new Error("The storage inspection has invalid file observations.");
  const page = cachePage(item.page);
  validatePageRows(page, data.map(snapshot => snapshot.id));
  const orphan = cachePage(item.orphan_page, true);
  if (orphan) {
    const info = item.orphan_page as OrphanPage;
    if (!integer(info.observed_count) || info.observed_count < item.orphan_files.length || typeof info.scan_complete !== "boolean" || (!info.scan_complete && info.next_after !== null)) throw new Error("The untracked-file observation is invalid or incomplete.");
    validatePageRows(orphan, item.orphan_files as string[]);
  }
  return value as CacheInspection;
}
export function parseProtocols(value: unknown): Protocol[] {
  const item = record(value);
  if (!Array.isArray(item.data) || !item.data.every((entry) => entry && typeof entry.id === "string" && typeof entry.support === "string" && (entry.note === undefined || typeof entry.note === "string"))) throw new Error("The protocol inventory is invalid. Refresh the service capabilities.");
  return item.data;
}
export function parseEndpoints(value: unknown): Endpoint[] {
  const item = record(value);
  if (item.schema_version !== 1 || !Array.isArray(item.endpoints) || !item.endpoints.every((entry) => entry && typeof entry.method === "string" && typeof entry.path === "string" && typeof entry.authority === "string" && strings(entry.prefixes))) throw new Error("The endpoint inventory is invalid. Refresh the service capabilities.");
  return item.endpoints;
}
export function positiveBytes(value: string, label: string): number {
  if (!/^[0-9]+$/.test(value.trim())) throw new Error(`${label} must be a positive whole number of bytes.`);
  const bytes = Number(value);
  if (!Number.isSafeInteger(bytes) || bytes <= 0) throw new Error(`${label} must be a positive whole number of bytes.`);
  return bytes;
}
export function downloadRequest(url: string, sha: string, expected: string, ceiling: string) {
  let source: URL;
  try { source = new URL(url.trim()); } catch { throw new Error("Enter a complete public HTTPS download URL."); }
  if (!["https:", "http:"].includes(source.protocol) || source.username || source.password || source.search || source.hash) throw new Error("Use a download URL without credentials, query or fragment. The service checks permitted sources.");
  if (!/^[a-fA-F0-9]{64}$/.test(sha.trim())) throw new Error("Enter the publisher's 64-character SHA256 checksum.");
  const maximum_bytes = positiveBytes(ceiling, "Storage ceiling");
  const expected_bytes = expected.trim() ? positiveBytes(expected, "Expected size") : null;
  if (expected_bytes !== null && expected_bytes > maximum_bytes) throw new Error("Expected size exceeds the storage ceiling.");
  return { source_url: source.href, expected_sha256: sha.trim().toLowerCase(), expected_bytes, maximum_bytes };
}
export function jobActions(job: ArtifactJob): ("pause" | "resume" | "retry" | "cancel" | "forget")[] {
  if (job.error === "artifact_publication_uncertain") return [];
  if (!job.writer_active && ["complete", "cancelled"].includes(job.state)) return ["forget"];
  if (job.writer_active && ["queued", "downloading"].includes(job.state)) return ["pause", "cancel"];
  if (job.writer_active && job.state === "pausing") return ["cancel"];
  if (!job.writer_active && job.state === "paused") return ["resume", "cancel"];
  if (!job.writer_active && job.state === "interrupted") return ["resume", "retry", "cancel"];
  if (!job.writer_active && ["failed", "quarantined"].includes(job.state)) return ["retry", "cancel"];
  return [];
}
export function removalReason(model: Model): string | undefined {
  if (model.policy !== "disabled") return "Set the model to Disabled before removing its registration.";
  if (model.loaded || model.state === "loading") return "Unload the worker before removing its registration.";
  if (model.active_requests) return "Wait for active requests to finish.";
}
export function sessionValue(value: string): string {
  if (!value || new TextEncoder().encode(value).length > 512 || /[\x00-\x1f\x7f]/.test(value)) throw new Error("Provide an opaque session identifier of at most 512 bytes, without control characters.");
  return value;
}
export function registration(artifact: string, id: string, command: string, argsText: string, reservation: string, inflight: string, operations: string[], provider: Provider = "external-http", options?: LaunchOptions, artifactSet?: string) {
  if (!/^[a-f0-9]{64}$/.test(artifact)) throw new Error("Choose a completed managed weight file.");
  if (!id.trim() || new TextEncoder().encode(id.trim()).length > 256) throw new Error("Enter a model ID of at most 256 bytes.");
  if (!command.trim()) throw new Error("Enter the engine executable available on the service host.");
  let args: unknown;
  try { args = JSON.parse(argsText); } catch { throw new Error("Engine arguments must be a JSON array of strings."); }
  if (!strings(args) || !args.some((arg) => arg.includes("{port}")) || !args.some((arg) => arg.includes("{model}"))) throw new Error("Engine arguments must include {port} and {model} in a JSON array of strings.");
  const max_inflight = positiveBytes(inflight, "Request slots");
  if (max_inflight > 0xffff_ffff || !operations.length) throw new Error("Choose supported operations and a valid request slot count.");
  if (provider !== "external-http" && provider !== "llama-cpp" && provider !== "whisper-cpp") throw new Error("Select an explicit supported engine provider.");
  if (provider === "llama-cpp" && max_inflight > 4096) throw new Error("llama.cpp physical slots must be between 1 and 4096.");
  if (provider !== "llama-cpp" && options && Object.values(options).some((value) => value !== null)) throw new Error("Typed launch options require the explicit llama.cpp provider.");
  if (provider === "whisper-cpp" && (max_inflight !== 1 || operations.length !== 1 || operations[0] !== "audio/transcriptions" || !args.some((arg, index) => arg === "--host" && args[index + 1] === "127.0.0.1") || args.some((arg) => arg === "--translate" || arg === "-tr" || arg.startsWith("--translate=")))) throw new Error("The selected whisper.cpp profile needs one slot, only audio/transcriptions and explicit loopback --host 127.0.0.1, without translation mode.");
  if (provider !== "whisper-cpp" && operations.includes("audio/transcriptions")) throw new Error("Choose the explicit whisper.cpp provider for audio/transcriptions.");
  if (options?.parallel_slots != null && options.parallel_slots !== max_inflight) throw new Error("Launch parallel slots must match the physical slot registration.");
  if (artifactSet !== undefined && !/^set-[a-f0-9]{64}$/.test(artifactSet)) throw new Error("Choose a current artifact set before registration.");
  return {
    id: id.trim(), artifact, model_file: "managed", ...(artifactSet ? { artifact_set: artifactSet } : {}),
    execution_profile: {
      schema_version: 1, adapter: provider, implementation_id: provider === "external-http" ? null : provider,
      command: command.trim(), args, options: options ?? {}, placement: "unresolved",
    },
    resident_bytes: positiveBytes(reservation, "Resident reservation"), max_inflight, operations, policy: "disabled", revision: 0,
  };
}
