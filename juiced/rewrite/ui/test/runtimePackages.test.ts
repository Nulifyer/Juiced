import { describe, expect, test } from "bun:test";
import { parseRuntimeImportReceipt, parseRuntimeInventory, parseRuntimeSelection, runtimeCandidate, runtimeImportRequest } from "../src/runtimePackages";
const packageId = `package-${"a".repeat(64)}`;
const previous = `package-${"b".repeat(64)}`;
function inventory() {
  return { schema_version: 1, generation: 2, installation_active: false, read_only: true, compatibility: "unqualified", packages: [{
    id: packageId, phase: "installed", declared_host_match: true, reserved_bytes: 42, member_count: 1, implementation_id: "llama-cpp", compatibility: "unqualified", verification: "required-on-acquire",
    provenance: { builder_id: "fixture", source_revision: "source-1", build_revision: "build-1", declared_licenses: ["unknown"], declared_sbom: null },
    declared_target: { operating_system: "linux", architecture: "x86_64", abi: "unknown", compute_paths: ["cpu"] },
  }] };
}
function selection() { return { schema_version: 1, model: "fixture", revision: 4, policy: "disabled", adapter: "llama-cpp", current: packageId, previous, supported: true, worker_in_use: false, read_only: true, compatibility: "unqualified" }; }
describe("managed runtime observations", () => {
  test("installed and staging packages remain unqualified and a first local selection can have no managed history", () => {
    expect(parseRuntimeInventory(inventory()).packages[0]!.compatibility).toBe("unqualified");
    const staged = inventory(); staged.packages[0]!.phase = "staging";
    expect(parseRuntimeInventory(staged).packages[0]!.phase).toBe("staging");
    expect(parseRuntimeSelection(selection()).policy).toBe("disabled");
    expect(parseRuntimeSelection({ ...selection(), current: null, previous: null }).current).toBeNull();
    expect(parseRuntimeInventory({ ...inventory(), packages: [] }).packages).toEqual([]);
  });
  test("duplicate identities, incomplete metadata and physical reservation overflow cannot become candidates", () => {
    const base = inventory();
    for (const mutation of [
      (value: ReturnType<typeof inventory>) => value.packages.push(value.packages[0]!),
      (value: ReturnType<typeof inventory>) => { value.packages[0]!.reserved_bytes = 0; },
      (value: ReturnType<typeof inventory>) => { value.packages[0]!.member_count = 2049; },
      (value: ReturnType<typeof inventory>) => { (value.packages[0]! as any).declared_host_match = "true"; },
      (value: ReturnType<typeof inventory>) => { value.packages[0]!.declared_target.compute_paths.push("cpu"); },
      (value: ReturnType<typeof inventory>) => { value.packages[0]!.provenance.build_revision = "private\nvalue"; },
    ]) { const changed = structuredClone(base); mutation(changed); expect(() => parseRuntimeInventory(changed)).toThrow(); }
    expect(() => parseRuntimeInventory({ ...base, packages: undefined })).toThrow();
    const over = inventory(); over.packages = Array.from({ length: 5 }, (_, index) => ({ ...over.packages[0]!, id: `package-${String(index).repeat(64)}`, reserved_bytes: 8 * 1024 ** 3 }));
    expect(() => parseRuntimeInventory(over)).toThrow();
    expect(() => parseRuntimeInventory({ ...base, compatibility: "qualified" })).toThrow();
  });
  test("history is distinct, exact and attached to a current package; unsupported adapters cannot grant mutation authority", () => {
    for (const change of [{ previous: packageId }, { current: null }, { current: "../private" }, { adapter: "external-http" }, { revision: -1 }, { revision: Number.MAX_SAFE_INTEGER + 1 }, { supported: "true" }, { compatibility: "qualified" }]) expect(() => parseRuntimeSelection({ ...selection(), ...change })).toThrow();
    expect(parseRuntimeSelection({ ...selection(), adapter: "external-http", supported: false, current: null, previous: null }).supported).toBe(false);
    expect(() => parseRuntimeSelection(null)).toThrow();
  });
  test("only installed matching declarations for the selected concrete adapter become replacement candidates", () => {
    const view = parseRuntimeSelection({ ...selection(), current: previous, previous: null });
    const item = parseRuntimeInventory(inventory()).packages[0]!;
    expect(runtimeCandidate(item, view)).toBe(true);
    for (const changed of [{ ...item, declared_host_match: false }, { ...item, phase: "staging" as const }, { ...item, implementation_id: "whisper-cpp" as const }, { ...item, id: previous }]) expect(runtimeCandidate(changed, view)).toBe(false);
    expect(runtimeCandidate(item, null)).toBe(false);
    expect(runtimeCandidate(item, { ...view, supported: false })).toBe(false);
    expect(parseRuntimeInventory({ ...inventory(), packages: [{ ...item, declared_host_match: false }] }).packages[0]!.compatibility).toBe("unqualified");
  });
  test("imports keep exact host paths and require current generation plus known idle ownership", () => {
    const view = parseRuntimeInventory(inventory());
    expect(runtimeImportRequest(view, "/host folder/import.json", 2)).toEqual({ specification_file: "/host folder/import.json", expected_generation: 2 });
    for (const generation of [null, 0, 3]) expect(() => runtimeImportRequest(view, "/host/import.json", generation)).toThrow("changed");
    for (const path of ["", "relative.json", "https://example.invalid/import.json", "C:\\import.json", "/host/\u0000import.json", "/host/\nimport.json", "/" + "é".repeat(2048), "/" + "a/".repeat(128)]) expect(() => runtimeImportRequest(view, path, 2)).toThrow("absolute");
    for (const installation_active of [true, undefined]) expect(() => runtimeImportRequest({ ...view, installation_active }, "/host/import.json", 2)).toThrow("Refresh installed");
    expect(() => parseRuntimeInventory({ ...inventory(), installation_active: "false" })).toThrow("installation state");
    expect(() => parseRuntimeInventory({ ...inventory(), packages: [{ ...inventory().packages[0], binding_leases: -1 }] })).toThrow("binding count");
    expect(parseRuntimeInventory({ ...inventory(), packages: [{ ...inventory().packages[0], binding_leases: 0 }] }).packages[0]!.binding_leases).toBe(0);
  });
  test("only confirmed installed receipts become UI success and do not infer qualification", () => {
    const receipt = { schema_version: 1, package: packageId, installed: true, newly_installed: true, reserved_bytes: 42, reused_files: 0, bundle_digest: "c".repeat(64), compatibility: "unqualified" };
    expect(parseRuntimeImportReceipt(receipt).newly_installed).toBe(true);
    expect(parseRuntimeImportReceipt({ ...receipt, newly_installed: false, reused_files: 1 }).newly_installed).toBe(false);
    for (const mutation of [{ installed: false }, { package: "../package" }, { newly_installed: null }, { reserved_bytes: 0 }, { reserved_bytes: 8 * 1024 ** 3 + 1 }, { reused_files: 2049 }, { bundle_digest: "short" }, { compatibility: "qualified" }]) expect(() => parseRuntimeImportReceipt({ ...receipt, ...mutation })).toThrow("could not be verified");
  });

});
