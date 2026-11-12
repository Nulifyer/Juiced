import { describe, expect, test } from "bun:test";
import { parseArtifactRevision, parseArtifactSets, primaryArtifactSet, type ArtifactSet } from "../src/artifactSetData";
import { parseArtifactInventory, registration, type Artifact } from "../src/management";

const file: Artifact = { id: "a".repeat(64), sha256: "a".repeat(64), bytes: 256, references: [], reference_count: 0, active_holds: 0, removing: false };
const specification = primaryArtifactSet(file);
const set: ArtifactSet = { id: `set-${"b".repeat(64)}`, specification, references: [], reference_count: 0, active_holds: 0, members_present: true, compatibility: "unqualified", content_verification: "required-on-acquire" };

describe("artifact sets", () => {
  test("revision observations preserve distinct current and rollback identities without granting qualification", () => {
    const current = { artifact:file.id, artifact_set:set.id };
    const previous = { artifact:file.id, artifact_set:`set-${"c".repeat(64)}` };
    const view = { schema_version:1,model:"public-model",revision:3,policy:"on-demand",current,previous,supported:true,worker_in_use:false,compatibility:"unqualified",read_only:true };
    expect(parseArtifactRevision(view).previous).toEqual(previous);
    expect(parseArtifactRevision({ ...view,current:null,previous:null,supported:false }).supported).toBe(false);
    for (const invalid of [null, {...view,revision:-1}, {...view,current:null}, {...view,previous:current}, {...view,current:{...current,artifact:"../other"}}, {...view,previous:{...previous,artifact_set:"set-bad"}}, {...view,supported:"yes"}, {...view,policy:"auto"}, {...view,compatibility:"qualified"}, {...view,read_only:false}]) expect(() => parseArtifactRevision(invalid)).toThrow();
  });
  test("a primary descriptor identifies content without mutating the source or claiming engine qualification", () => {
    expect(specification.members).toEqual([{ name: "model", role: "model", artifact: file.id, bytes: file.bytes }]);
    expect(specification.origin.source_revision).toBe(`sha256:${file.sha256}`);
    expect(parseArtifactSets([set], [file])).toEqual([set]);
    expect(file.references).toEqual([]);
    for (const invalid of [{ ...file, bytes: 0 }, { ...file, removing: true }, { ...file, id: "../other" }, { ...file, sha256: "c".repeat(64) }]) expect(() => primaryArtifactSet(invalid)).toThrow();
  });
  test("malformed, duplicated, missing and incompatible metadata refuse the inventory before mutation", () => {
    const cases: unknown[] = [
      null, [null], [set, set], [{ ...set, id: "set-../other" }], [{ ...set, active_holds: -1 }], [{ ...set, compatibility: "qualified" }],
      [{ ...set, specification: { ...specification, schema_version: 2 } }], [{ ...set, specification: { ...specification, primary: "missing" } }],
      [{ ...set, specification: { ...specification, members: [...specification.members, ...specification.members] } }],
      [{ ...set, specification: { ...specification, members: [{ ...specification.members[0]!, role: "future" }] } }],
      [{ ...set, specification: { ...specification, members: [{ ...specification.members[0]!, bytes: 257 }] } }],
      [{ ...set, specification: { ...specification, origin: { ...specification.origin, source_revision: "https://secret.invalid/private" } } }],
      [{ ...set, members_present: "yes" }], [{ ...set, reference_count: undefined }], [{ ...set, references: ["model:a"], reference_count: 0 }],
      [{ ...set, specification: { ...specification, members: [{ ...specification.members[0]!, artifact: "../other" }] } }],
      Array.from({ length: 1025 }, () => set),
    ];
    for (const value of cases) expect(() => parseArtifactSets(value, [file])).toThrow();
    expect(parseArtifactSets([set], [])).toEqual([set]);
    expect(() => parseArtifactSets([set], [{ ...file, removing: true }])).toThrow();
    expect(parseArtifactSets([{ ...set, members_present: false }], [{ ...file, removing: true }])[0]!.members_present).toBe(false);
    const page = { total: 1, next_after: null };
    const inventory = { generation: 1, view: "c".repeat(64), limit: 64, charged_bytes: 256, source_policy: "offline", jobs: [], artifacts: [], artifact_sets: [set], quarantined_files: [], pages: { jobs: { total: 0, next_after: null }, artifacts: { total: 1, next_after: null }, artifact_sets: page, quarantined_files: { total: 0, next_after: null } }, selection: { artifact: file, artifact_set: set } };
    expect(parseArtifactInventory(inventory).artifact_sets).toEqual([set]);
    expect(parseArtifactInventory(inventory).selection.artifact).toEqual(file);
    expect(() => parseArtifactInventory({ ...inventory, artifact_sets: cases[7] })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, limit: 1, artifact_sets: [set, { ...set, id: `set-${"c".repeat(64)}` }] })).toThrow();
    expect(() => parseArtifactInventory({ ...inventory, selection: { artifact: { ...file, removing: true }, artifact_set: set } })).toThrow();
  });
  test("every member is checked while repeated content stays a valid declared role binding", () => {
    const companion: ArtifactSet = { ...set, specification: { ...specification, members: [...specification.members, { ...specification.members[0]!, name: "projector", role: "projector" }] } };
    expect(parseArtifactSets([companion], [file])[0]!.specification.members).toHaveLength(2);
    expect(() => parseArtifactSets([{ ...companion, specification: { ...companion.specification, members: companion.specification.members.map((member) => ({ ...member, role: "model" })) } }], [file])).toThrow();
  });
  test("registration carries the selected set and rejects a noncanonical reference", () => {
    const create = (set?: string) => registration(file.id, "bound-model", "engine", '["--port","{port}","--model","{model}"]', "1024", "1", ["completions"], "external-http", undefined, set);
    expect(create(set.id).artifact_set).toBe(set.id);
    expect(create().artifact_set).toBeUndefined();
    for (const invalid of ["", "../set", `set-${"B".repeat(64)}`]) expect(() => create(invalid)).toThrow();
  });
});
