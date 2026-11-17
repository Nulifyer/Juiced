import { expect, test } from "bun:test";
import { weightView } from "../src/ArtifactRevisionInspector";
import type { ArtifactSet } from "../src/artifactSetData";
import type { LocalInventory } from "../src/localLibraryData";
import { wordsMatch } from "../src/search";
import { describeVoice, voiceAliases, voiceView } from "../src/VoiceSelect";

const labels = (view: ReturnType<typeof voiceView>) => view.groups.map((group) => [group.label, group.items.map((item) => item.value)]);

test("coded voices group by language and say their gender", () => {
  expect(describeVoice("bf_emma")).toEqual({ id: "bf_emma", language: "British English", gender: "Female" });
  expect(describeVoice("lessac")).toEqual({ id: "lessac", language: null, gender: null });
  const view = voiceView(["zm_yunxi", "bf_emma", "af_heart", "am_adam"], {}, "");
  expect(labels(view)).toEqual([["American English", ["af_heart", "am_adam"]], ["British English", ["bf_emma"]], ["Mandarin Chinese", ["zm_yunxi"]]]);
  expect(view.groups[0]!.items[1]!.kind).toBe("Male");
  expect(view.columns).toBe(true);
});

test("voice search matches every word against name, language, gender and client names", () => {
  const voices = ["af_heart", "am_adam", "bm_george", "bf_emma"];
  const aliases = voiceAliases({ aliases: { ash: "am_adam", alloy: "af_alloy", bad: 3 } });
  expect(aliases).toEqual({ am_adam: ["ash"], af_alloy: ["alloy"] });
  expect(labels(voiceView(voices, aliases, "british male"))).toEqual([["British English", ["bm_george"]]]);
  expect(labels(voiceView(voices, aliases, "ash"))).toEqual([["American English", ["am_adam"]]]);
  const none = voiceView(voices, aliases, "japanese");
  expect([none.shown, none.total]).toEqual([0, 4]);
});

test("voices an engine does not code stay in one unlabeled list", () => {
  const view = voiceView(["lessac", "amy"], {}, "");
  expect(labels(view)).toEqual([["", ["lessac", "amy"]]]);
  expect(view.columns).toBe(false);
});

const set = (id: string, artifact: string, bytes: number) => ({ id: `set-${id.padEnd(64, "0")}`, specification: { schema_version: 1, origin: { importer_id: "test", importer_version: "1", source_revision: `rev-${id}` }, primary: "model", members: [{ name: "model", role: "model", artifact, bytes }] } }) as unknown as ArtifactSet;
const file = (artifact: string, name: string, quant: string) => ({ id: artifact, name, path: `/m/${name}`, quant, role: "model", bytes: 1, location: "", storage: "artifact_store", artifact, readOnly: false, removable: true, refusal: null, usedBy: [], unverified: false });
const local = { totalBytes: 0, removableBytes: 0, truncated: false, groups: [
  { id: "g-other", repo: "a/Other-GGUF", name: "Other-GGUF", publisher: "a", storage: [], location: null, readOnly: false, bytes: 0, removableBytes: 0, usedBy: [], quants: [], files: [file("art-3", "Other-Q8_0.gguf", "Q8_0")] },
  { id: "g-own", repo: "b/Own-GGUF", name: "Own-GGUF", publisher: "b", storage: [], location: null, readOnly: false, bytes: 0, removableBytes: 0, usedBy: [], quants: [], files: [file("art-1", "Own-Q4_K_M.gguf", "Q4_K_M"), file("art-2", "Own-Q8_0.gguf", "Q8_0")] },
] } as unknown as LocalInventory;

test("weight sets show their file and quant, the model's own repository first", () => {
  const sets = [set("c", "art-3", 3 * 1024 ** 3), set("b", "art-2", 2 * 1024 ** 3), set("a", "art-1", 1024 ** 3), set("d", "art-9", 1024)];
  const view = weightView(sets, local, "g-own", "");
  expect(view.groups.map((group) => [group.label, group.items.map((item) => item.label)])).toEqual([
    ["Own-GGUF", ["Own-Q4_K_M.gguf", "Own-Q8_0.gguf"]], ["Other-GGUF", ["Other-Q8_0.gguf"]], ["Other files", [`rev-d · ${"d".padEnd(12, "0")}`]],
  ]);
  expect(view.groups[0]!.items[0]).toMatchObject({ kind: "Q4_K_M", fact: "1.0 GiB" });
  expect(weightView(sets, local, "g-own", "q8 other").groups.map((group) => group.items.map((item) => item.label))).toEqual([["Other-Q8_0.gguf"]]);
  expect(weightView(sets, null, null, "").groups.map((group) => group.label)).toEqual(["Other files"]);
});

test("search words match from the start of a word", () => {
  expect(wordsMatch("bm_george British English Male", "male")).toBe(true);
  expect(wordsMatch("bf_emma British English Female", "male")).toBe(false);
  expect(wordsMatch("unsloth/Qwen3-Coder-Next-GGUF UD-Q6_K_XL", "coder q6")).toBe(true);
  expect(wordsMatch("fixture.missing-weights", "MISSING")).toBe(true);
  expect(wordsMatch("Qwen3-Coder", "wen")).toBe(false);
  expect(wordsMatch("anything", "   ")).toBe(true);
});
