import { expect, test } from "bun:test";
import { groupEntries, planUpload, splitPart } from "../src/localFilesData";

const named = (...names: string[]) => names.map((name) => ({ name }));

test("split part names keep the llama.cpp spelling", () => {
  expect(splitPart("Qwen3-Q4_K_M-00002-of-00003.gguf")).toEqual({ prefix: "Qwen3-Q4_K_M", part: 2, parts: 3 });
  for (const name of ["m-00001-of-00001.gguf", "m-00002-OF-00002.gguf", "m-1-of-2.gguf", "m-00003-of-00002.gguf", "plain.gguf"]) expect(splitPart(name)).toBeNull();
});

test("an upload is one model or every part of one split model, plus at most one projector", () => {
  const split = planUpload(named("mmproj-V-F16.gguf", "V-00002-of-00002.gguf", "V-00001-of-00002.gguf"));
  expect(split).toEqual({ files: named("V-00001-of-00002.gguf", "V-00002-of-00002.gguf", "mmproj-V-F16.gguf"), label: "V-00001-of-00002.gguf", parts: 2 });
  expect(planUpload(named("one.gguf"))).toEqual({ files: named("one.gguf"), label: "one.gguf", parts: 1 });
  const refused = (names: string[]) => { const plan = planUpload(named(...names)); return "error" in plan ? plan.error : ""; };
  expect(refused(["V-00001-of-00003.gguf", "V-00003-of-00003.gguf"])).toBe("Pick all 3 parts of V. Missing part 2.");
  expect(refused(["a.gguf", "b.gguf"])).toBe("Pick one model file, or every part of one split model.");
  expect(refused(["mmproj-a.gguf"])).toBe("This is a vision projector. Pick its model file too.");
  expect(refused(["m.gguf", "mmproj-a.gguf", "mmproj-b.gguf"])).toBe("Pick at most one vision projector (mmproj) file.");
});

test("the server folder lists a split model once with its total size", () => {
  const entries = groupEntries([
    { name: "V-00001-of-00002.gguf", bytes: 10, split: true },
    { name: "V-00002-of-00002.gguf", bytes: 5, split: true },
    { name: "W-00002-of-00002.gguf", bytes: 7, split: true },
    { name: "mmproj-V.gguf", bytes: 3, split: false },
  ]);
  expect(entries.map((entry) => [entry.name, entry.parts, entry.bytes])).toEqual([
    ["V-00001-of-00002.gguf", 2, 15],
    ["W-00002-of-00002.gguf", 1, 7],
    ["mmproj-V.gguf", 1, 3],
  ]);
});
