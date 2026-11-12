import { describe, expect, test } from "bun:test";
import { filterRuntimes, parseRuntimeInventory, parseRuntimeResolution, resolutionPath, sourceLink, treatmentCounts, treatmentNames } from "../src/runtimeCatalogData";

const adapter = { status: "planned", scope: "Definition only. No qualified adapter." };
const row = { backend: "cpu", tier: "experimental", format: "native", os: ["linux"], device_declaration: "cpu (x86_64)" };
const catalog = () => ({
  schema_version: 1, execution_enabled: false, qualification_receipts: [], reviewed_at: "2026-11-10", purpose: "Planning definitions", candidate_backend_semantics: "Discovery leads only",
  priority_definitions: Object.fromEntries(Object.keys(treatmentNames).map((key) => [key, "Planning treatment"])),
  definitions: [
    { id: "futo-transcribe-cpp", name: "FUTO transcribe.cpp", kind: "engine", priority: "parity", candidate_backends: ["cpu"], tasks: ["speech-recognition"], sources: ["https://gitlab.futo.org/keyboard/transcribe.cpp"], notes: "FUTO GitLab origin; CPU and ASR4ALL only.", upstream_lifecycle: "unknown", adapter },
    { id: "handy-transcribe-cpp", name: "Handy transcribe.cpp", kind: "engine", priority: "conditional", candidate_backends: ["cpu", "metal"], tasks: ["speech-recognition"], sources: ["https://github.com/handy-computer/transcribe.cpp"], notes: "Separate origin from FUTO.", upstream_lifecycle: "unknown", adapter },
  ],
  compute_paths: [{ id: "cpu", description: "CPU compute vocabulary" }], discovery_tasks: ["speech-recognition"],
  native_recipe_migrations: [{ legacy_recipe: "asr4all", display_name: "FUTO transcribe.cpp", catalog_entry: "futo-transcribe-cpp", native_backend_labels: ["cpu"], migration_rule: "Preserve FUTO identity and CPU restriction", native_support_rows: [row] }],
  native_reference: { recipe_count: 1, support_row_count: 1, source_head: "recorded-snapshot" },
});
const resolution = () => ({ legacy_recipe: "asr4all", legacy_backend: "cpu", implementation_id: "futo-transcribe-cpp", compute_path_id: "cpu", evidence_scope: "Native declaration only", migration_rule: "Preserve FUTO", override_reason: null, native_support_rows: [row], adapter, execution_enabled: false, qualification_receipts: [], blockers: [{ code: "bundle_identity_required", message: "Verify the immutable runtime bundle." }] });

describe("catalog identity and delivery treatment", () => {
  test("similar display names retain distinct origins, restrictions and treatment counts", () => {
    const inventory = parseRuntimeInventory(catalog());
    expect(filterRuntimes(inventory.definitions, "parity", " transcribe ").map((entry) => entry.id)).toEqual(["futo-transcribe-cpp"]);
    expect(filterRuntimes(inventory.definitions, "conditional", "TRANSCRIBE").map((entry) => entry.id)).toEqual(["handy-transcribe-cpp"]);
    expect(filterRuntimes(inventory.definitions, "all", "metal").map((entry) => entry.id)).toEqual(["handy-transcribe-cpp"]);
    expect(treatmentCounts(inventory.definitions)).toMatchObject({ parity: 1, conditional: 1, attach: 0 });
    expect(inventory.definitions[0]?.upstream_lifecycle).toBe("unknown");
    expect(inventory.native_recipe_migrations[0]?.native_support_rows[0]).toEqual(row);
  });
  test("execution and qualification claims fail closed instead of promoting candidate paths", () => {
    expect(() => parseRuntimeInventory({ ...catalog(), execution_enabled: true })).toThrow("execution or qualification");
    expect(() => parseRuntimeInventory({ ...catalog(), qualification_receipts: [{ qualified: true }] })).toThrow("execution or qualification");
    const invalid = catalog(); invalid.definitions[0]!.adapter = { status: "supported", scope: "automatic" };
    expect(() => parseRuntimeInventory(invalid)).toThrow("runtime definition");
  });
  test("duplicate identities and inconsistent native snapshots are rejected", () => {
    const duplicate = catalog(); duplicate.definitions.push(duplicate.definitions[0]!);
    expect(() => parseRuntimeInventory(duplicate)).toThrow();
    const invalid = catalog(); invalid.native_reference.support_row_count = 44;
    expect(() => parseRuntimeInventory(invalid)).toThrow("inconsistent");
    const wrongLabel = catalog(); wrongLabel.native_recipe_migrations[0]!.native_support_rows[0] = { ...row, backend: "npu" };
    expect(() => parseRuntimeInventory(wrongLabel)).toThrow("original backend labels");
  });
});

describe("native mapping is bounded read-only evidence", () => {
  test("unresolved provenance or vendor paths remain unknown and blocked", () => {
    const result = parseRuntimeResolution({ ...resolution(), legacy_recipe: "llamacpp", legacy_backend: "system", implementation_id: "llama-cpp", compute_path_id: null });
    expect(result.compute_path_id).toBeNull();
    expect(result.execution_enabled).toBe(false);
    expect(result.qualification_receipts).toEqual([]);
    expect(result.blockers).toHaveLength(1);
    expect(() => parseRuntimeResolution({ ...resolution(), blockers: [] })).toThrow("incomplete");
    expect(() => parseRuntimeResolution({ ...resolution(), execution_enabled: true })).toThrow();
  });
  test("native recipe and backend labels travel literally in the encoded query", () => {
    expect(resolutionPath("llamacpp", "hybrid-rocm")).toBe("juiced/runtimes/resolve?recipe=llamacpp&backend=hybrid-rocm");
    expect(resolutionPath("literal recipe", "path&backend")).toContain("recipe=literal+recipe&backend=path%26backend");
    expect(() => resolutionPath("recipe\n", "cpu")).toThrow();
    expect(() => resolutionPath("x".repeat(257), "cpu")).toThrow();
  });
  test("only explicit HTTP source references become links; local references stay text", () => {
    expect(sourceLink("https://gitlab.futo.org/keyboard/transcribe.cpp")).toBe("https://gitlab.futo.org/keyboard/transcribe.cpp");
    for (const source of ["../../BACKENDS.md", "javascript:alert(1)", "file:///etc/passwd", "https://user:secret@example.com/source"]) expect(sourceLink(source)).toBeUndefined();
  });
});
