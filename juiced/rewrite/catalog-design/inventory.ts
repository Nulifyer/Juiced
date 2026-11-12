import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, resolve } from "node:path";

type Entry = {
  id: string;
  name: string;
  kind: string;
  priority: string;
  candidate_backends: string[];
  tasks: string[];
  sources: string[];
  notes: string;
  upstream_lifecycle: string;
};
type SupportRow = {
  backend: string;
  tier: string;
  format: string;
  os: string[];
  device_declaration: string;
};
type Migration = {
  legacy_recipe: string;
  display_name: string;
  catalog_entry: string;
  native_selectable_backend: boolean;
  native_uses_ctx_size: boolean;
  native_backend_labels: string[];
  migration_rule: string;
  native_support_rows: SupportRow[];
  profile_overrides?: { legacy_backend: string; catalog_entry: string; reason: string }[];
};
type Inventory = {
  schema_version: number;
  reviewed_at: string;
  execution_enabled: boolean;
  priority_definitions: Record<string, string>;
  backends: { id: string; description: string }[];
  tasks: string[];
  entries: Entry[];
  native_reference: {
    source_head: string;
    recipe_count: number;
    support_row_count: number;
    source_files: { path: string; sha256: string }[];
  };
  native_recipe_migrations: Migration[];
  model_family_examples: { id: string; runtime_candidates: string[]; notes: string; source: string }[];
};

const root = dirname(import.meta.path);
const inventory = JSON.parse(await readFile(resolve(root, "inventory.json"), "utf8")) as Inventory;
const args = process.argv.slice(2);
const check = args.includes("--check");
const nativeIndex = args.indexOf("--native-root");
const nativeRoot = nativeIndex === -1 ? undefined : args[nativeIndex + 1];
const expectedArgs = check ? ["--check"] : [];
if (nativeRoot) expectedArgs.push("--native-root", nativeRoot);
if (JSON.stringify(args) !== JSON.stringify(expectedArgs)) {
  throw new Error("Usage: bun inventory.ts [--check] [--native-root PATH]");
}
if (nativeIndex !== -1 && !nativeRoot) throw new Error("--native-root requires a path");

function requireValue(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}
function ids<T extends { id: string }>(records: T[], label: string) {
  const values = new Set<string>();
  for (const record of records) {
    requireValue(/^[a-z][a-z0-9-]*$/.test(record.id), "Invalid " + label + " ID: " + record.id);
    requireValue(!values.has(record.id), "Duplicate " + label + " ID: " + record.id);
    values.add(record.id);
  }
  return values;
}
function unique(values: string[], label: string) {
  requireValue(values.length === new Set(values).size, "Duplicate " + label);
}
requireValue(inventory.schema_version === 1, "Unknown inventory schema");
requireValue(inventory.execution_enabled === false, "Design seed must not enable execution");
requireValue(/^\d{4}-\d{2}-\d{2}$/.test(inventory.reviewed_at), "Missing review date");
const entries = ids(inventory.entries, "entry");
const backends = ids(inventory.backends, "backend");
const families = ids(inventory.model_family_examples, "model family");
requireValue([...families].every(id => !entries.has(id)), "Model families must have independent identities");
unique(inventory.tasks, "task");
const tasks = new Set(inventory.tasks);
const kinds = new Set(["engine", "engine-distribution", "implementation", "framework", "vendor-stack", "deployment-runtime", "external-service", "packaging"]);
const priorities = ["parity", "first-expansion", "conditional", "attach", "reference-only", "legacy"];
requireValue(JSON.stringify(Object.keys(inventory.priority_definitions)) === JSON.stringify(priorities), "Priority definitions changed without updating treatment");
for (const entry of inventory.entries) {
  requireValue(kinds.has(entry.kind), "Unknown kind: " + entry.id);
  requireValue(priorities.includes(entry.priority), "Unknown priority: " + entry.id);
  requireValue(["active", "maintenance", "archived", "unknown"].includes(entry.upstream_lifecycle), "Unknown upstream lifecycle");
  requireValue(entry.name && entry.notes && entry.sources.length, "Incomplete entry: " + entry.id);
  unique(entry.candidate_backends, entry.id + " backend");
  unique(entry.tasks, entry.id + " task");
  requireValue(entry.candidate_backends.length && entry.candidate_backends.every(id => backends.has(id)), "Unknown/empty backend: " + entry.id);
  requireValue(entry.tasks.length && entry.tasks.every(id => tasks.has(id)), "Unknown/empty task: " + entry.id);
  for (const source of entry.sources) {
    if (source.startsWith("https://")) {
      requireValue(new URL(source).hostname, "Invalid source URL: " + entry.id);
    } else {
      requireValue(!isAbsolute(source), "Source must be repository-relative: " + entry.id);
      const file = resolve(root, source.split("#")[0]!);
      requireValue(await Bun.file(file).exists(), "Missing local source: " + source);
    }
  }
}
unique(inventory.native_recipe_migrations.map(row => row.legacy_recipe), "native recipe");
let supportCount = 0;
const migrated = new Set<string>();
for (const row of inventory.native_recipe_migrations) {
  requireValue(entries.has(row.catalog_entry), "Missing migration target: " + row.legacy_recipe);
  requireValue(inventory.entries.find(e => e.id === row.catalog_entry)!.priority === "parity", "Native migration target lost parity treatment");
  migrated.add(row.catalog_entry);
  unique(row.native_backend_labels, "native backend label");
  requireValue(row.migration_rule && row.native_support_rows.length, "Incomplete native migration");
  for (const support of row.native_support_rows) {
    requireValue(row.native_backend_labels.includes(support.backend), "Undeclared native support backend");
    requireValue(support.os.length && support.tier && support.format && support.device_declaration, "Incomplete support row");
    supportCount++;
  }
  for (const override of row.profile_overrides ?? []) {
    requireValue(row.native_backend_labels.includes(override.legacy_backend), "Override for unknown native backend");
    requireValue(entries.has(override.catalog_entry), "Unknown override target");
    requireValue(inventory.entries.find(e => e.id === override.catalog_entry)!.priority === "parity", "Override target lost parity treatment");
    requireValue(override.reason, "Override requires a reason");
    migrated.add(override.catalog_entry);
  }
}
requireValue(inventory.native_recipe_migrations.length === inventory.native_reference.recipe_count, "Native recipe count mismatch");
requireValue(supportCount === inventory.native_reference.support_row_count, "Native support row count mismatch");
requireValue(inventory.entries.filter(e => e.priority === "parity").every(e => migrated.has(e.id)), "Unmapped parity entry");
for (const family of inventory.model_family_examples) {
  requireValue(family.runtime_candidates.length && family.runtime_candidates.every(id => entries.has(id)), "Unknown family runtime: " + family.id);
  requireValue(family.notes && new URL(family.source).protocol === "https:", "Incomplete family example");
}

if (nativeRoot) {
  for (const source of inventory.native_reference.source_files) {
    const bytes = await readFile(resolve(nativeRoot, source.path));
    requireValue(createHash("sha256").update(bytes).digest("hex") === source.sha256, "Native source drift: " + source.path);
  }
  const reference = await readFile(resolve(nativeRoot, "juiced/BACKENDS.md"), "utf8");
  function rows(marker: string) {
    const start = "<!-- BEGIN GENERATED: " + marker + " -->";
    const end = "<!-- END GENERATED: " + marker + " -->";
    requireValue(reference.includes(start) && reference.includes(end), "Missing native table");
    return reference.split(start)[1]!.split(end)[0]!.split("\n")
      .filter(line => line.startsWith("|")).slice(2)
      .map(line => line.slice(1, -1).split("|").map(cell => cell.trim().replace(/^`|`$/g, "")));
  }
  const overview = inventory.native_recipe_migrations.map(row => [
    row.legacy_recipe, row.display_name, row.native_selectable_backend ? "yes" : "no",
    row.native_uses_ctx_size ? "yes" : "no", row.native_backend_labels.join(", "),
  ]);
  const support = inventory.native_recipe_migrations.flatMap(row => row.native_support_rows.map(cell => [
    row.legacy_recipe, cell.backend, cell.tier, cell.format, cell.os.join(", "), cell.device_declaration,
  ]));
  requireValue(JSON.stringify(rows("backends-overview")) === JSON.stringify(overview), "Native recipe matrix changed");
  const sorted = (data: string[][]) => data.map(row => JSON.stringify(row)).sort();
  requireValue(JSON.stringify(sorted(rows("backends-matrix"))) === JSON.stringify(sorted(support)), "Native support matrix changed");
}

function cell(value: string) {
  return value.replaceAll("|", "\\|").replaceAll("\n", " ");
}
const table = (headers: string[], rows: string[][]) => [
  "| " + headers.join(" | ") + " |",
  "| " + headers.map(() => "---").join(" | ") + " |",
  ...rows.map(row => "| " + row.map(cell).join(" | ") + " |"),
].join("\n");
const lines = [
  "# Proposed runtime integration list",
  "",
  "Generated from [inventory.json](inventory.json). Edit the inventory and run",
  "`bun inventory.ts`. Use `bun inventory.ts --check` to verify it.",
  "",
  "This is a planning list for the [catalog design](DESIGN.md). Entries and",
  "candidate compute paths do not establish package availability, adapter support,",
  "model compatibility, or hardware qualification. Upstream lifecycle is unknown",
  "unless the inventory records an inspected maintenance/archive notice.",
  "",
  "## Native recipes to preserve",
  "",
  inventory.priority_definitions.parity!,
  "",
  table(["Legacy recipe", "Catalog identity", "Native backend labels", "Migration boundary"],
    inventory.native_recipe_migrations.map(row => [row.legacy_recipe, row.catalog_entry, row.native_backend_labels.join(", "), row.migration_rule])),
  "",
  "The llamacpp/hybrid-rocm profile additionally resolves to the",
  "llama-xdna-hybrid distribution. The " + inventory.native_recipe_migrations.length + " legacy recipes therefore reference",
  migrated.size + " catalog identities. All " + supportCount + " native support rows, including tier, OS and",
  "device restrictions, remain in the inventory snapshot.",
  "",
  "Additional upstream compute paths within these projects are retained as",
  "candidate_backends in the inventory. They require additive qualification.",
  "",
];
const titles: Record<string, string> = {
  "first-expansion": "First new integrations",
  conditional: "Conditional integrations",
  attach: "Independently owned services",
  "reference-only": "Packaging and client-local references",
  legacy: "Legacy recognition",
};
for (const priority of priorities.filter(p => p !== "parity")) {
  lines.push("## " + titles[priority], "", inventory.priority_definitions[priority]!, "",
    table(["Project / implementation", "Catalog ID", "Kind", "Candidate compute paths", "Boundary"],
      inventory.entries.filter(e => e.priority === priority).map(e => [
        "[" + e.name + "](" + e.sources[0] + ")", e.id, e.kind, e.candidate_backends.join(", "), e.notes,
      ])), "");
}
lines.push("## Model family examples", "",
  "These examples identify weights and architectures. Runtime candidates still",
  "need a compatible artifact export, importer and selected-operation receipt.", "",
  table(["Model family", "Candidate implementations", "Boundary"],
    inventory.model_family_examples.map(f => ["[" + f.id + "](" + f.source + ")", f.runtime_candidates.join(", "), f.notes])),
  "");
const rendered = lines.join("\n");
const output = resolve(root, "INTEGRATIONS.md");
if (check) {
  requireValue(await readFile(output, "utf8") === rendered, "INTEGRATIONS.md is stale; run bun inventory.ts");
} else {
  await writeFile(output, rendered);
}
console.log(JSON.stringify({
  entries: inventory.entries.length,
  priorities: Object.fromEntries(priorities.map(p => [p, inventory.entries.filter(e => e.priority === p).length])),
  native_recipes: inventory.native_recipe_migrations.length,
  native_support_rows: supportCount,
  local_source_references: "valid",
  generated_list: check ? "current" : "written",
  native_reference: nativeRoot ? "exact rows and hashes match" : "not compared",
  execution_enabled: false,
}, null, 2));
