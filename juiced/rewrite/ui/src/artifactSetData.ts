import type { Artifact } from "./management";
import { policyNames, type Policy } from "./domain";

export const artifactRoles = ["model", "weight-shard", "weight-index", "tokenizer", "configuration", "processor", "projector", "draft-model", "adapter", "voice", "voice-configuration", "vocabulary", "external-tensor", "encoder", "decoder", "denoiser", "vae"] as const;
export type ArtifactSet = {
  id: string;
  specification: { schema_version: 1; origin: { importer_id: string; importer_version: string; source_revision: string }; primary: string; members: { name: string; role: typeof artifactRoles[number]; artifact: string; bytes: number }[] };
  references: string[]; reference_count: number; active_holds: number; members_present: boolean; compatibility: "unqualified"; content_verification: "required-on-acquire";
};
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The artifact set inventory is invalid. Refresh before changing sets.");
  return value as Record<string, unknown>;
}
function integer(value: unknown): value is number { return Number.isSafeInteger(value) && (value as number) >= 0; }
function label(value: unknown, maximum: number) { return typeof value === "string" && value.length <= maximum && /^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(value); }
function revision(value: unknown) { return typeof value === "string" && value.length > 0 && value.length <= 256 && /^[a-zA-Z0-9._:+@-]+$/.test(value); }
export function parseArtifactSets(value: unknown, artifacts: Artifact[] = [], maximum = 1024): ArtifactSet[] {
  if (!Array.isArray(value) || value.length > maximum) throw new Error("The artifact set inventory is invalid. Refresh before changing sets.");
  const files = new Map(artifacts.map((artifact) => [artifact.id, artifact]));
  const ids = new Set<string>();
  for (const item of value) {
    const set = record(item), spec = record(set.specification), origin = record(spec.origin);
    if (typeof set.id !== "string" || !/^set-[a-f0-9]{64}$/.test(set.id) || ids.has(set.id) || !Array.isArray(set.references) || set.references.length > 64 || !set.references.every((item) => typeof item === "string") || !integer(set.reference_count) || set.reference_count < set.references.length || !integer(set.active_holds) || typeof set.members_present !== "boolean" || set.compatibility !== "unqualified" || set.content_verification !== "required-on-acquire" || spec.schema_version !== 1 || !label(spec.primary, 64) || !label(origin.importer_id, 128) || !revision(origin.importer_version) || !revision(origin.source_revision) || !Array.isArray(spec.members) || spec.members.length === 0 || spec.members.length > 64) throw new Error("The artifact set inventory is invalid. Refresh before changing sets.");
    ids.add(set.id);
    const names = new Set<string>(); let primary = false;
    for (const item of spec.members) {
      const member = record(item), artifact = files.get(member.artifact as string);
      if (!label(member.name, 64) || names.has(member.name as string) || !(artifactRoles as readonly unknown[]).includes(member.role) || !integer(member.bytes) || member.bytes === 0 || typeof member.artifact !== "string" || !/^[a-f0-9]{64}$/.test(member.artifact) || (set.members_present && artifact && (artifact.bytes !== member.bytes || artifact.removing)) || ((member.role === "model") !== (member.name === spec.primary))) throw new Error("The artifact set members are inconsistent. Refresh before changing stored files.");
      names.add(member.name as string); primary ||= member.name === spec.primary;
    }
    if (!primary) throw new Error("The artifact set has no primary file. Refresh before changing stored files.");
  }
  return value as ArtifactSet[];
}
export function primaryArtifactSet(artifact: Artifact): ArtifactSet["specification"] {
  if (!/^[a-f0-9]{64}$/.test(artifact.id) || artifact.sha256 !== artifact.id || !integer(artifact.bytes) || artifact.bytes === 0 || artifact.removing) throw new Error("Refresh and select a validated file before creating a set.");
  return { schema_version: 1, origin: { importer_id: "juiced-console", importer_version: "1", source_revision: `sha256:${artifact.sha256}` }, primary: "model", members: [{ name: "model", role: "model", artifact: artifact.id, bytes: artifact.bytes }] };
}

export type ArtifactBinding = { artifact: string; artifact_set: string };
export type ArtifactRevision = { schema_version: 1; model: string; revision: number; policy: Policy; current: ArtifactBinding | null; previous: ArtifactBinding | null; supported: boolean; worker_in_use: boolean; compatibility: "unqualified"; read_only: true };
function binding(value: unknown): ArtifactBinding | null {
  if (value === null) return null;
  const item = record(value);
  if (typeof item.artifact !== "string" || !/^[a-f0-9]{64}$/.test(item.artifact) || typeof item.artifact_set !== "string" || !/^set-[a-f0-9]{64}$/.test(item.artifact_set)) throw new Error("The model revision binding is invalid. Refresh before changing weights.");
  return item as ArtifactBinding;
}
export function parseArtifactRevision(value: unknown): ArtifactRevision {
  const item = record(value);
  const current = binding(item.current), previous = binding(item.previous);
  if (item.schema_version !== 1 || typeof item.model !== "string" || item.model.length === 0 || item.model.length > 256 || !integer(item.revision) || typeof item.policy !== "string" || !Object.hasOwn(policyNames, item.policy) || typeof item.supported !== "boolean" || typeof item.worker_in_use !== "boolean" || item.compatibility !== "unqualified" || item.read_only !== true || (item.supported && !current) || (previous && (!current || previous.artifact_set === current.artifact_set))) throw new Error("The model revision observation is invalid. Refresh before changing weights.");
  return { ...item, current, previous } as ArtifactRevision;
}
