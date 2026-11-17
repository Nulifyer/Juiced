import { expect, test } from "bun:test";
import { collapseFinished, nextStep } from "../src/Downloading";
import type { LocalGroup, LocalInventory, PullView } from "../src/localLibraryData";

const pull = (id: string, state = "complete"): PullView => ({ id, state, error: null, repo: "unsloth/Qwen3-0.6B-GGUF", label: "Q4_K_M", files: [{ file: "Qwen3-0.6B-Q4_K_M.gguf", bytes: 10, state: "present", downloaded: 10, job: null, source: null }], then: null });
const group = (usedBy: string[]): LocalGroup => ({
  id: "hf:unsloth/qwen3-0.6b-gguf", repo: "unsloth/Qwen3-0.6B-GGUF", name: "Qwen3-0.6B-GGUF", publisher: "unsloth", storage: ["huggingface_cache"], location: null, readOnly: false, bytes: 20, removableBytes: 0, usedBy,
  quants: [
    { label: "Q8_0", file: "f2", parts: 1, complete: true, bytes: 10, files: ["f2"], usedBy: [], removable: true, add: { path: "/b" } },
    { label: "Q4_K_M", file: "f1", parts: 1, complete: true, bytes: 10, files: ["f1"], usedBy, removable: true, add: { path: "/a" } },
  ],
  files: [
    { id: "f1", name: "Qwen3-0.6B-Q4_K_M.gguf", path: "/a", quant: "Q4_K_M", role: "model", bytes: 10, location: "/a", storage: "huggingface_cache", artifact: null, readOnly: false, removable: true, refusal: null, unverified: false, usedBy },
    { id: "f2", name: "Qwen3-0.6B-Q8_0.gguf", path: "/b", quant: "Q8_0", role: "model", bytes: 10, location: "/b", storage: "huggingface_cache", artifact: null, readOnly: false, removable: true, refusal: null, unverified: false, usedBy: [] },
  ],
});
const inventory = (usedBy: string[]): LocalInventory => ({ totalBytes: 20, removableBytes: 0, truncated: false, groups: [group(usedBy)] });

test("a finished pull opens the model that uses its files, or adds the pulled quant", () => {
  expect(nextStep(pull("a"), inventory([]))).toEqual({ kind: "add", group: "hf:unsloth/qwen3-0.6b-gguf", quant: "f1" });
  expect(nextStep(pull("a"), inventory(["Qwen3-0.6B-Q4_K_M"]))).toEqual({ kind: "open", model: "Qwen3-0.6B-Q4_K_M" });
  expect(nextStep(pull("a", "downloading"), inventory([]))).toBeNull();
  expect(nextStep(pull("a"), null)).toBeNull();
});

test("repeated finished pulls of one quant collapse into a row that dismisses them all", () => {
  const row = (key: string, state: string, finished: boolean) => ({ key, title: "Qwen3-0.6B-GGUF Q4_K_M", state, finished, pulls: [key], jobs: [] as unknown[] });
  const rows = collapseFinished([row("a", "downloading", false), row("b", "complete", true), row("c", "complete", true), row("d", "failed", true)]);
  expect(rows.map((item) => item.key)).toEqual(["a", "b", "d"]);
  expect(rows[1]!.pulls).toEqual(["b", "c"]);
});

test("a pull with a pending add offers nothing until the service settles it", () => {
  const adding = { ...pull("a"), then: { kind: "add" as const, model: "Qwen3-0.6B-Q4_K_M", policy: "on-demand", state: "pending" as const, message: null } };
  expect(nextStep(adding, inventory([]))).toBeNull();
  const added = { ...adding, then: { ...adding.then, state: "done" as const, model: "Custom-Name" } };
  expect(nextStep(added, { ...inventory([]), groups: [{ ...group([]), usedBy: ["Custom-Name"] }] })).toEqual({ kind: "open", model: "Custom-Name" });
  expect(nextStep(added, inventory([])), "a removed model falls back to adding the files").toEqual({ kind: "add", group: "hf:unsloth/qwen3-0.6b-gguf", quant: "f1" });
  const failed = { ...adding, then: { ...adding.then, state: "failed" as const, message: "No engine." } };
  expect(nextStep(failed, inventory([]))).toEqual({ kind: "add", group: "hf:unsloth/qwen3-0.6b-gguf", quant: "f1" });
});
