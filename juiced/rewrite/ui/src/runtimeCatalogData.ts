export const treatmentNames = { parity: "Native parity", "first-expansion": "First expansion", conditional: "Conditional", attach: "Service attachment", "reference-only": "Reference only", legacy: "Legacy" } as const;
export type Treatment = keyof typeof treatmentNames;
export type RuntimeDefinition = { id: string; name: string; kind: string; priority: Treatment; candidate_backends: string[]; tasks: string[]; sources: string[]; notes: string; upstream_lifecycle: string; adapter: { status: "planned" | "partial"; scope: string } };
export type NativeRow = { backend: string; tier: string; format: string; os: string[]; device_declaration: string };
export type NativeMigration = { legacy_recipe: string; display_name: string; catalog_entry: string; native_backend_labels: string[]; migration_rule: string; native_support_rows: NativeRow[] };
export type RuntimeInventory = { schema_version: number; execution_enabled: false; qualification_receipts: []; reviewed_at: string; purpose: string; candidate_backend_semantics: string; priority_definitions: Record<Treatment, string>; definitions: RuntimeDefinition[]; compute_paths: { id: string; description: string }[]; discovery_tasks: string[]; native_recipe_migrations: NativeMigration[]; native_reference: { recipe_count: number; support_row_count: number; source_head: string } };
export type RuntimeResolution = { legacy_recipe: string; legacy_backend: string; implementation_id: string; compute_path_id: string | null; evidence_scope: string; migration_rule: string; override_reason: string | null; native_support_rows: NativeRow[]; adapter: RuntimeDefinition["adapter"]; execution_enabled: false; qualification_receipts: []; blockers: { code: string; message: string }[] };
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
const text = (value: unknown): value is string => typeof value === "string" && value.length <= 32768;
const strings = (value: unknown): value is string[] => Array.isArray(value) && value.length <= 256 && value.every(text);
const id = (value: unknown): value is string => typeof value === "string" && /^[a-z0-9][a-z0-9-]{0,127}$/.test(value);
const adapter = (value: unknown): boolean => object(value) && ["planned", "partial"].includes(value.status as string) && text(value.scope);
const row = (value: unknown): boolean => object(value) && text(value.backend) && text(value.tier) && text(value.format) && strings(value.os) && text(value.device_declaration);
function readOnly(value: Record<string, unknown>) {
  if (value.execution_enabled !== false || !Array.isArray(value.qualification_receipts) || value.qualification_receipts.length !== 0) throw new Error("This catalog contains execution or qualification claims this console cannot present. Check the matching service version.");
}
export function parseRuntimeInventory(value: unknown): RuntimeInventory {
  if (!object(value) || value.schema_version !== 1) throw new Error("The runtime catalog schema is invalid. Refresh or check the service version.");
  readOnly(value);
  if (!text(value.reviewed_at) || !text(value.purpose) || !text(value.candidate_backend_semantics) || !object(value.priority_definitions) || !Object.keys(treatmentNames).every((key) => text((value.priority_definitions as Record<string, unknown>)[key])) || !Array.isArray(value.definitions) || value.definitions.length > 1024) throw new Error("The runtime catalog definitions are invalid. Refresh the catalog.");
  const ids = new Set<string>();
  for (const entry of value.definitions) {
    if (!object(entry) || !id(entry.id) || ids.has(entry.id) || !text(entry.name) || !text(entry.kind) || !Object.hasOwn(treatmentNames, entry.priority as string) || !strings(entry.candidate_backends) || !strings(entry.tasks) || !strings(entry.sources) || !text(entry.notes) || !text(entry.upstream_lifecycle) || !adapter(entry.adapter)) throw new Error("A runtime definition is invalid. No support or execution can be inferred from this response.");
    ids.add(entry.id);
  }
  if (!Array.isArray(value.compute_paths) || value.compute_paths.length > 256 || !value.compute_paths.every((path) => object(path) && id(path.id) && text(path.description)) || !strings(value.discovery_tasks) || !Array.isArray(value.native_recipe_migrations) || value.native_recipe_migrations.length > 256) throw new Error("The catalog discovery vocabulary is invalid.");
  for (const migration of value.native_recipe_migrations) {
    if (!object(migration) || !id(migration.legacy_recipe) || !text(migration.display_name) || !id(migration.catalog_entry) || !ids.has(migration.catalog_entry) || !strings(migration.native_backend_labels) || !text(migration.migration_rule) || !Array.isArray(migration.native_support_rows)) throw new Error("A native recipe mapping is invalid. Refresh before resolving its labels.");
    const labels = migration.native_backend_labels;
    if (!migration.native_support_rows.every((entry) => row(entry) && labels.includes(entry.backend))) throw new Error("A native declaration does not match its original backend labels.");
  }
  const reference = value.native_reference;
  if (!object(reference) || !Number.isSafeInteger(reference.recipe_count) || !Number.isSafeInteger(reference.support_row_count) || !text(reference.source_head) || reference.recipe_count !== value.native_recipe_migrations.length || reference.support_row_count !== value.native_recipe_migrations.reduce((sum: number, migration: NativeMigration) => sum + migration.native_support_rows.length, 0)) throw new Error("The native declaration snapshot counts are inconsistent.");
  return value as RuntimeInventory;
}
export function parseRuntimeResolution(value: unknown): RuntimeResolution {
  if (!object(value)) throw new Error("The service returned an invalid native recipe resolution.");
  readOnly(value);
  if (!text(value.legacy_recipe) || !text(value.legacy_backend) || !id(value.implementation_id) || !(value.compute_path_id === null || id(value.compute_path_id)) || !text(value.evidence_scope) || !text(value.migration_rule) || !(value.override_reason === null || text(value.override_reason)) || !adapter(value.adapter) || !Array.isArray(value.native_support_rows) || !value.native_support_rows.every(row) || !Array.isArray(value.blockers) || value.blockers.length === 0 || !value.blockers.every((blocker) => object(blocker) && text(blocker.code) && text(blocker.message))) throw new Error("The native recipe resolution is incomplete. No executable plan was established.");
  return value as RuntimeResolution;
}
export function filterRuntimes(definitions: RuntimeDefinition[], treatment: Treatment | "all", query: string): RuntimeDefinition[] {
  const needle = query.trim().toLowerCase();
  return definitions.filter((entry) => (treatment === "all" || entry.priority === treatment) && [entry.id, entry.name, entry.kind, entry.notes, ...entry.candidate_backends, ...entry.tasks].join(" ").toLowerCase().includes(needle));
}
export function treatmentCounts(definitions: RuntimeDefinition[]): Record<Treatment, number> {
  const counts = Object.fromEntries(Object.keys(treatmentNames).map((key) => [key, 0])) as Record<Treatment, number>;
  for (const entry of definitions) counts[entry.priority] += 1;
  return counts;
}
export function sourceLink(value: string): string | undefined {
  try {
    const url = new URL(value);
    if (!["https:", "http:"].includes(url.protocol) || url.username || url.password) return;
    return url.href;
  } catch { return; }
}
export function resolutionPath(recipe: string, backend: string): string {
  if (![recipe, backend].every((value) => value.length > 0 && value.length <= 256 && !/[\x00-\x1f\x7f]/.test(value))) throw new Error("Choose the original bounded native recipe and backend labels.");
  return `juiced/runtimes/resolve?${new URLSearchParams({ recipe, backend })}`;
}
