import { describe, expect, test } from "bun:test";
import { lowSpace, parsePreview, place, parseStorage, parseStorageSummary, reclaimRows } from "../src/storageData";

const row = (id: string, kind: string, bytes: number, reclaimable: boolean, references: string[] = []) => ({ id, kind, title: id, path: `/data/${id}`, bytes, reclaimable, refusal: reclaimable ? null : "in_use", message: reclaimable ? null : "Used by a.", references });
const view = {
  schema_version: 1, revision: "00ff", settings: { reserve_bytes: 2, low_space_bytes: 20 },
  volumes: [{ path: "/data", paths: ["/data"], roles: ["data", "models"], total_bytes: 100, available_bytes: 10, used_bytes: 90, read_only: false, low: true }],
  warnings: [{ code: "low_space", message: "10 B free on /data.", path: "/data" }, { code: "future_warning", message: "ignored" }],
  categories: [{ id: "models", title: "Model weights", bytes: 40, reclaimable_bytes: 5 }],
  models: [{ model: "a", bytes: 40, files: 2, shared_bytes: 5, read_only_bytes: 0, removal_frees_bytes: 35, shared_with: ["b"] }],
  unreferenced: [row("file:x", "file", 5, true)], partials: [row("partial:p", "partial", 7, false)],
  engines: [row("engine:used", "engine", 50, false, ["a"]), row("engine:spare", "engine", 30, true)],
  legacy: [row("legacy:cache", "legacy", 900, true), row("legacy:config", "legacy", 3, false)],
  session_cache: { bytes: 12, session_bytes: 10, budget_bytes: 100, sessions: 2, over_budget: false }, truncated: false,
};

describe("storage view", () => {
  test("parses the full view and lists reclaimable items largest first, refused ones last", () => {
    const parsed = parseStorage(view);
    expect(parsed.volumes[0]!.low).toBe(true);
    expect(parsed.models[0]!.shared_with).toEqual(["b"]);
    expect(reclaimRows(parsed).map((item) => item.id)).toEqual(["legacy:cache", "engine:spare", "file:x", "partial:p", "legacy:config"]);
  });
  test("engines a model uses are not offered", () => {
    expect(reclaimRows(parseStorage(view)).some((item) => item.id === "engine:used")).toBe(false);
  });
  test("summary keeps only warnings the Dashboard knows how to explain", () => {
    expect(lowSpace(parseStorageSummary(view)).map((warning) => warning.code)).toEqual(["low_space"]);
    expect(lowSpace(null)).toEqual([]);
  });
  test("malformed bodies fail instead of rendering an empty disk", () => {
    expect(() => parseStorage({ volumes: [] })).toThrow();
    expect(() => parseStorageSummary({})).toThrow();
    expect(() => parsePreview({ items: [] })).toThrow();
    expect(parsePreview({ revision: "a", freed_bytes: 5, refused: 0, items: [row("file:x", "file", 5, true)] }).freed_bytes).toBe(5);
  });
  test("locations name the repository or the last folders", () => {
    expect(place("/data/huggingface/hub/models--unsloth--Qwen3-4B-GGUF/snapshots/abc/q.gguf")).toBe("unsloth/Qwen3-4B-GGUF");
    expect(place("/data/cache")).toBe("/data/cache");
    expect(place("/data/runtime-packages/package-1")).toBe("…/runtime-packages/package-1");
    expect(place(null)).toBeNull();
  });
});
