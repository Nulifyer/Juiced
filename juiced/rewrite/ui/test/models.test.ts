import { expect, test } from "bun:test";
import type { Model } from "../src/domain";
import type { LocalGroup, LocalInventory } from "../src/localLibraryData";
import { loadProgress, loadProgressText, namePieces, primaryRepo } from "../src/Models";

const model: Model = { id: "z-image-turbo-GGUF-Q8_0", policy: "on-demand", revision: 1, state: "unloaded", downloaded: true, loaded: false, pid: null, active_requests: 0, max_inflight: 1, resident_bytes: 1, operations: ["images/generations"], error: null };

test("names split between parts and keep the quant whole", () => {
  expect(namePieces("Qwen3-Coder-Next-GGUF-Q4_K_M")).toEqual(["Qwen3-Coder-", "Next-GGUF-Q4_K_M"]);
  expect(namePieces("Ornith-1.5-35B-A3B-GGUF-Q4_K_M")).toEqual(["Ornith-1.5-", "35B-A3B-", "GGUF-Q4_K_M"]);
  expect(namePieces("fixture.chat-long-context-q8")).toEqual(["fixture.", "chat-long-", "context-q8"]);
  expect(namePieces("whisper")).toEqual(["whisper"]);
  expect(namePieces("").join("")).toBe("");
});

test("load progress shows time left only with an earlier load", () => {
  const loading = { ...model, state: "loading" as const, loading_started_unix_ms: 10_000 };
  expect(loadProgress(model, 20_000)).toBeNull();
  expect(loadProgress({ ...loading, loading_started_unix_ms: undefined }, 20_000)).toBeNull();
  const fresh = loadProgress(loading, 22_000)!;
  expect(fresh).toEqual({ elapsed: 12_000, left: null, share: null });
  expect(loadProgressText(fresh)).toBe("12 s");
  const known = loadProgress({ ...loading, last_load_ms: 42_000 }, 22_000)!;
  expect(loadProgressText(known)).toBe("12 s · about 30 s left");
  expect(known.share).toBeCloseTo(12 / 42);
  const slow = loadProgress({ ...loading, last_load_ms: 5_000 }, 22_000)!;
  expect(slow).toEqual({ elapsed: 12_000, left: null, share: 1 });
});

test("the model card link uses the weights' repository, not a companion's", () => {
  const file = (id: string, role: string) => ({ id, name: id, path: id, quant: null, role, bytes: 1, location: "", storage: "huggingface_cache" as const, artifact: null, readOnly: false, removable: false, refusal: null, unverified: false, usedBy: [model.id] });
  const group = (repo: string, files: ReturnType<typeof file>[]): LocalGroup => ({ id: repo, repo, name: repo, publisher: null, storage: ["huggingface_cache"], location: null, readOnly: false, bytes: 1, removableBytes: 0, usedBy: [model.id], quants: [], files });
  const inventory: LocalInventory = { totalBytes: 2, removableBytes: 0, truncated: false, groups: [group("unsloth/Qwen3-4B-Instruct-2507-GGUF", [file("encoder", "text_encoder")]), group("owner/z-image-turbo-GGUF", [file("weights", "model")])] };
  expect(primaryRepo(model, inventory)).toBe("owner/z-image-turbo-GGUF");
  expect(primaryRepo(model, { ...inventory, groups: [inventory.groups[0]!] })).toBeNull();
  expect(primaryRepo(model, null)).toBeNull();
  const byPath = { ...inventory, groups: [group("unsloth/Qwen3-4B-Instruct-2507-GGUF", [file("encoder", "model")]), group("owner/z-image-turbo-GGUF", [file("weights", "model")])] };
  expect(primaryRepo(model, byPath)).toBe("owner/z-image-turbo-GGUF");
});
