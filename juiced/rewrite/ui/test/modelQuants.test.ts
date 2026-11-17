import { expect, test } from "bun:test";
import { featured, parseSearch, quantKey, quantNames } from "../src/libraryData";
import { parseLocal } from "../src/localLibraryData";
import { quantRows } from "../src/ModelQuants";

const hub = (quants: Record<string, unknown>[]) => parseSearch({ sources: { huggingface: { status: "ok" } }, data: [{ repo: "unsloth/Demo-GGUF", name: "Demo-GGUF", quants }] }).models[0]!;
const remote = (label: string, file: string, bytes: number, extra: Record<string, unknown> = {}) => ({ label, file, bytes, pullable: true, ...extra });

test("quants that share a label stay separate and take the part of the file name that differs", () => {
  const model = hub([remote("UD-IQ4_NL", "Demo-UD-IQ4_NL.gguf", 10), remote("UD-IQ4_NL", "Demo-UD-IQ4_NL_XL.gguf", 12), remote("Q8_0", "mtp/Demo-MTP-Q8_0.gguf", 20)]);
  expect(new Set(model.quants.map(quantKey)).size).toBe(3);
  const names = quantNames(model.quants);
  expect(model.quants.map((quant) => names.get(quant) ?? quant.label)).toEqual(["Demo-UD-IQ4_NL", "Demo-UD-IQ4_NL_XL", "Q8_0"]);
  const patches = hub([0, 1, 2].map((index) => remote("File", `patch/Huihui-Qwen3.6-35B-A3B-abliterated-patch-all-${index}.gguf`, 10 + index)));
  expect(patches.quants.map((quant) => quantNames(patches.quants).get(quant))).toEqual(["patch-all-0", "patch-all-1", "patch-all-2"]);
});

test("featured quants include the common sizes under a UD- prefix", () => {
  const model = hub([remote("UD-IQ1_M", "a.gguf", 1), remote("UD-Q4_K_M", "b.gguf", 4), remote("UD-Q2_K_XL", "c.gguf", 2), remote("Q8_0", "d.gguf", 8)]);
  expect(featured(model.quants).map((quant) => quant.label)).toEqual(["UD-Q4_K_M", "Q8_0"]);
});

test("a model lists files on disk and a few downloads in one list ordered by size", () => {
  const group = parseLocal({ groups: [{ id: "hf:unsloth/Demo-GGUF", repo: "unsloth/Demo-GGUF", name: "Demo-GGUF",
    quants: [
      { label: "Q4_K_M", file: "f4", bytes: 40, files: ["f4"], used_by: ["demo-q4", "demo-q4-hybrid"] },
      { label: "Q6_K", file: "f6", bytes: 60, files: ["f6"], used_by: ["demo-q6"] },
      { label: "Q8_0", file: "f8", bytes: 80, files: ["f8"], used_by: [] },
    ],
    files: [{ id: "f4", name: "Demo-Q4_K_M.gguf" }, { id: "f6", name: "Demo-Q6_K.gguf" }, { id: "f8", name: "Demo-Q8_0.gguf" }] }] }).groups[0]!;
  const model = hub([
    remote("Q4_K_M", "Demo-Q4_K_M.gguf", 40, { local: { present: true } }), remote("Q6_K", "Demo-Q6_K.gguf", 60, { local: { present: true } }),
    remote("Q8_0", "Demo-Q8_0.gguf", 80), remote("Q5_K_M", "Demo-Q5_K_M.gguf", 50, { jobs: [{ id: "j", state: "downloading", downloaded_bytes: 25 }] }),
    remote("BF16", "Demo-BF16.gguf", 160), remote("IQ2_M", "Demo-IQ2_M.gguf", 20), remote("Q3_K_S", "Demo-Q3_K_S.gguf", 30, { pullable: false }),
  ]);
  const { rows, more } = quantRows({ id: "demo-q6" }, [{ id: "demo-q4", policy: "hot" }, { id: "demo-q4-hybrid", policy: "disabled" }], group, model);
  expect(rows.map((row) => [row.label, row.kind])).toEqual([["Q4_K_M", "added"], ["Q4_K_M", "added"], ["Q5_K_M", "remote"], ["Q6_K", "current"], ["Q8_0", "file"], ["BF16", "remote"]]);
  expect(rows.filter((row) => row.kind === "added").map((row) => [row.model, row.policy, row.shared])).toEqual([["demo-q4", "hot", true], ["demo-q4-hybrid", "disabled", true]]);
  expect(rows.find((row) => row.label === "Q5_K_M")).toMatchObject({ progress: 50, file: "Demo-Q5_K_M.gguf" });
  expect(more).toBe(2);
  expect(quantRows({ id: "demo-q6" }, [], group, null)).toMatchObject({ more: 0, rows: [{ kind: "added" }, { kind: "added" }, { kind: "current" }, { kind: "file" }] });
});

test("a split download that stopped part way offers to finish it, not to add it", () => {
  const group = parseLocal({ groups: [{ id: "hf:unsloth/Demo-GGUF", repo: "unsloth/Demo-GGUF", name: "Demo-GGUF",
    quants: [
      { label: "Q4_K_M", file: "f4", bytes: 40, files: ["f4"], used_by: ["demo-q4"] },
      { label: "UD-Q6_K_XL", file: "p1", parts: 3, complete: false, bytes: 1, files: ["p1"], used_by: [] },
    ],
    files: [{ id: "f4", name: "Demo-Q4_K_M.gguf" }, { id: "p1", name: "Demo-UD-Q6_K_XL-00001-of-00003.gguf" }] }] }).groups[0]!;
  const model = hub([remote("Q4_K_M", "Demo-Q4_K_M.gguf", 40, { local: { present: true } }), remote("UD-Q6_K_XL", "UD-Q6_K_XL/Demo-UD-Q6_K_XL-00001-of-00003.gguf", 66, { parts: 3 })]);
  const { rows } = quantRows({ id: "demo-q4" }, [], group, model);
  expect(rows.map((row) => [row.label, row.kind])).toEqual([["Q4_K_M", "current"], ["UD-Q6_K_XL", "partial"]]);
  expect(rows[1]).toMatchObject({ have: 1, parts: 3, bytes: 66, file: "UD-Q6_K_XL/Demo-UD-Q6_K_XL-00001-of-00003.gguf" });
  expect(quantRows({ id: "demo-q4" }, [], group, null).rows[1]).toMatchObject({ kind: "partial", file: null });
});
