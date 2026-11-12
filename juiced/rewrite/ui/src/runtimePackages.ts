import { policyNames, type Policy } from "./domain";

const id = (value: unknown): value is string => typeof value === "string" && /^package-[a-f0-9]{64}$/.test(value);
const integer = (value: unknown): value is number => typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The installed runtime observation is invalid. Refresh before selecting a runtime.");
  return value as Record<string, unknown>;
}
function token(value: unknown, max: number): value is string { return typeof value === "string" && value.length > 0 && value.length <= max && /^[a-zA-Z0-9._:+@-]+$/.test(value); }
export type RuntimePackage = {
  id: string; phase: "staging" | "installed"; reserved_bytes: number; member_count: number; implementation_id: "llama-cpp" | "whisper-cpp";
  provenance: { builder_id: string; source_revision: string; build_revision: string; declared_licenses: string[]; declared_sbom: string | null; release?: { engine?: string; backend?: string; tag?: string; gpu_target?: string | null } };
  declared_target: { operating_system: string; architecture: string; abi: string; compute_paths: string[] };
  compatibility: "unqualified"; verification: "required-on-acquire"; declared_host_match: boolean;
  binding_leases?: number;
};
export type RuntimeInventory = { schema_version: 1; generation: number; packages: RuntimePackage[]; installation_active?: boolean; read_only: true; compatibility: "unqualified" };
export function parseRuntimeInventory(value: unknown): RuntimeInventory {
  const data = record(value);
  if (data.schema_version !== 1 || !integer(data.generation) || data.read_only !== true || data.compatibility !== "unqualified" || !Array.isArray(data.packages) || data.packages.length > 256) throw new Error("The installed runtime inventory is invalid. Refresh before selecting a runtime.");
  if (data.installation_active !== undefined && typeof data.installation_active !== "boolean") throw new Error("The runtime installation state is invalid. Refresh before importing a bundle.");
  const seen = new Set<string>(); let bytes = 0;
  for (const entry of data.packages) {
    const item = record(entry), origin = record(item.provenance), target = record(item.declared_target);
    if (!id(item.id) || seen.has(item.id) || !["staging", "installed"].includes(item.phase as string) || !integer(item.reserved_bytes) || item.reserved_bytes === 0 || item.reserved_bytes > 64 * 1024 ** 3 || !integer(item.member_count) || item.member_count === 0 || item.member_count > 16384 || !token(item.implementation_id, 64) || item.compatibility !== "unqualified" || item.verification !== "required-on-acquire" || typeof item.declared_host_match !== "boolean"
      || !token(origin.builder_id, 128) || !token(origin.source_revision, 256) || !token(origin.build_revision, 256) || !Array.isArray(origin.declared_licenses) || origin.declared_licenses.length > 16 || !origin.declared_licenses.every((license) => typeof license === "string" && /^[\x20-\x7e]{1,128}$/.test(license)) || (origin.declared_sbom !== null && (typeof origin.declared_sbom !== "string" || origin.declared_sbom.length > 4096))
      || !token(target.operating_system, 64) || !token(target.architecture, 64) || !token(target.abi, 64) || !Array.isArray(target.compute_paths) || target.compute_paths.length === 0 || target.compute_paths.length > 16 || !target.compute_paths.every((path) => token(path, 64)) || new Set(target.compute_paths).size !== target.compute_paths.length) throw new Error("An installed runtime record is invalid. Refresh before selecting a runtime.");
    seen.add(item.id); bytes += item.reserved_bytes;
    if (item.binding_leases !== undefined && !integer(item.binding_leases)) throw new Error("The runtime binding count is invalid. Refresh the installed runtimes.");
  }
  if (bytes > 256 * 1024 ** 3) throw new Error("The installed runtime reservations exceed their bound. Refresh before selecting a runtime.");
  return data as RuntimeInventory;
}
export function runtimeImportRequest(inventory: RuntimeInventory, specification: string, generation: number | null) {
  if (inventory.installation_active !== false) throw new Error("Refresh installed runtimes and wait for any active installation before importing.");
  if (generation !== inventory.generation) throw new Error("The installed runtimes changed. Review the inventory, then refresh installed runtimes to retry this draft.");
  if (!specification.startsWith("/") || /[\x00-\x1f\x7f]/.test(specification) || new TextEncoder().encode(specification).length > 4096 || specification.split("/").length > 128) throw new Error("Enter an absolute specification-file path on the service host, within 4096 bytes and 128 path components.");
  return { specification_file: specification, expected_generation: generation };
}
export type RuntimeImportReceipt = { schema_version: 1; package: string; installed: true; newly_installed: boolean; reserved_bytes: number; reused_files: number; bundle_digest: string; compatibility: "unqualified" };
export function parseRuntimeImportReceipt(value: unknown): RuntimeImportReceipt {
  const item = record(value);
  if (item.schema_version !== 1 || !id(item.package) || item.installed !== true || typeof item.newly_installed !== "boolean" || !integer(item.reserved_bytes) || item.reserved_bytes === 0 || item.reserved_bytes > 64 * 1024 ** 3 || !integer(item.reused_files) || item.reused_files > 2048 || typeof item.bundle_digest !== "string" || !/^[a-f0-9]{64}$/.test(item.bundle_digest) || item.compatibility !== "unqualified") throw new Error("The installation result could not be verified. Preserve this draft and refresh installed runtimes before retrying.");
  return item as RuntimeImportReceipt;
}
export type RuntimeSelection = { schema_version: 1; model: string; revision: number; policy: Policy; adapter: "external-http" | "llama-cpp" | "whisper-cpp"; current: string | null; previous: string | null; supported: boolean; worker_in_use: boolean; read_only: true; compatibility: "unqualified" };
export function runtimeCandidate(item: RuntimePackage, selection: RuntimeSelection | null): boolean {
  return !!selection?.supported && item.phase === "installed" && item.declared_host_match && item.implementation_id === selection.adapter && item.id !== selection.current;
}
export function parseRuntimeSelection(value: unknown): RuntimeSelection {
  const item = record(value);
  if (item.schema_version !== 1 || typeof item.model !== "string" || item.model.length === 0 || item.model.length > 256 || !integer(item.revision) || typeof item.policy !== "string" || !Object.hasOwn(policyNames, item.policy) || !["external-http", "llama-cpp", "whisper-cpp"].includes(item.adapter as string) || (item.current !== null && !id(item.current)) || (item.previous !== null && (!id(item.previous) || !item.current || item.previous === item.current)) || typeof item.supported !== "boolean" || (item.supported && item.adapter === "external-http") || typeof item.worker_in_use !== "boolean" || item.read_only !== true || item.compatibility !== "unqualified") throw new Error("The model runtime selection is invalid. Refresh before changing its runtime.");
  return item as RuntimeSelection;
}

export function packageLabel(item: RuntimePackage): string {
  const release = item.provenance.release;
  if (release?.engine && release.backend) return `${release.engine} · ${release.backend.toUpperCase()}${release.tag ? ` · ${release.tag}` : ""}`;
  return `${item.implementation_id} · ${item.provenance.build_revision.slice(0, 24)}`;
}
