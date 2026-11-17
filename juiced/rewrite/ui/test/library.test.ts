import { expect, test } from "bun:test";
import { filterResults, fitLabel, flmJob, flmResults, npuEngine, parseFlm, parseSearch, progress, quantFit, quantMatches, type LibraryModel, type ResultFilters } from "../src/libraryData";

test("checkpoint variants keep their engine, member files and hub credential state", () => {
  const result = parseSearch({
    artifact_generation: 4, offline: false,
    sources: { huggingface: { status: "ok", authenticated: true } },
    data: [{ repo: "Qwen/Qwen3-0.6B", name: "Qwen3-0.6B", gated: false, quants: [{
      label: "Checkpoint", kind: "checkpoint", format: "transformers", engine: "vllm", file: "config.json", parts: 1, bytes: 1519182365, pullable: true,
      files: [{ file: "config.json", bytes: 726, member: "config.json" }, { file: "model.safetensors", bytes: 1503300328, member: "model.safetensors" }],
      local: { present: true, present_files: 2, set: "set-abc" },
    }, { label: "Q4_K_M", file: "m-Q4_K_M.gguf", bytes: 10 }] }],
  });
  expect(result.authenticated).toBe(true);
  const [checkpoint, quant] = result.models[0]!.quants;
  expect(checkpoint).toMatchObject({ kind: "checkpoint", engine: "vllm", set: "set-abc", present: true });
  expect(checkpoint!.files.map((file) => file.member)).toEqual(["config.json", "model.safetensors"]);
  expect(quant).toMatchObject({ kind: "quant", engine: null, files: [] });
});

const flm = parseFlm({
  available: true, validation: { problems: ["Memlock limits are too low."] },
  models: [
    { checkpoint: "gemma3:1b", installed: false, footprint_gb: 1.2, kind: "chat", labels: [], registered: [] },
    { checkpoint: "gemma3:4b", installed: true, footprint_gb: 4.4, kind: "chat", labels: ["vision"], registered: ["gemma3-4b-FLM"] },
    { checkpoint: "qwen3:8b", installed: false, footprint_gb: 5.4, kind: "chat", labels: ["reasoning"], registered: [] },
    { checkpoint: "embed-gemma:300m", installed: false, footprint_gb: 0.6, kind: "embeddings", labels: [], registered: [] },
  ],
  jobs: [{ checkpoint: "gemma3:1b", state: "failed", percent: null, error: "old" }, { checkpoint: "gemma3:1b", state: "pulling", percent: 40, then: { policy: "hot", state: "pending" } }],
});
const GiB = 1024 ** 3;
const quant = (label: string, gib: number | null, engine: string | null = null) => ({ ...parseSearch({ data: [{ repo: "a/b", quants: [{ label, file: `${label}.gguf`, bytes: gib === null ? null : gib * GiB, engine }] }] }).models[0]!.quants[0]! });
const model = (repo: string, downloads: number | null, quants: ReturnType<typeof quant>[]): LibraryModel => ({ ...parseSearch({ data: [{ repo, downloads }] }).models[0]!, quants });

test("FastFlowLM chat checkpoints become one result per family with a quant per size", () => {
  const results = flmResults(flm, "", null);
  expect(results.map((item) => [item.name, item.source, item.publisher, item.task, item.quants.map((entry) => entry.label)])).toEqual([["gemma3", "flm", "FastFlowLM", "chat", ["1b", "4b"]], ["qwen3", "flm", "FastFlowLM", "chat", ["8b"]]]);
  const [small, large] = results[0]!.quants;
  expect(small).toMatchObject({ engine: npuEngine, file: "gemma3:1b", present: false, pullable: true, users: [] });
  expect(Math.round(progress(small!)!)).toBe(40);
  expect(large).toMatchObject({ file: "gemma3:4b", bytes: Math.round(4.4 * GiB), present: true, users: ["gemma3-4b-FLM"], traits: ["vision"] });
  expect(progress(large!)).toBeNull();
  expect(flmJob(flm, "gemma3:1b")?.state).toBe("pulling");
  expect(flm.problems).toEqual(["Memlock limits are too low."]);
});

test("FastFlowLM results follow the search text and the type, and need an available engine", () => {
  expect(flmResults(flm, "QWEN", null).map((item) => item.name)).toEqual(["qwen3"]);
  expect(flmResults(flm, "vision", "chat").flatMap((item) => item.quants.map((entry) => entry.file))).toEqual(["gemma3:4b"]);
  expect(flmResults(flm, "npu", null)).toHaveLength(2);
  expect(flmResults(flm, "", "embeddings")).toEqual([]);
  expect(flmResults(flm, "nothing-like-this", null)).toEqual([]);
  expect(flmResults({ ...flm, available: false }, "", null)).toEqual([]);
  expect(flmResults(null, "", null)).toEqual([]);
});

test("a quant's fit names where it runs on this machine", () => {
  const machine = { gpu: 8 * GiB, ram: 32 * GiB };
  expect(quantFit(4 * GiB, null, machine)).toBe("gpu");
  expect(quantFit(7.5 * GiB, null, machine), "weights need headroom above their size").toBe("memory");
  expect(quantFit(20 * GiB, "vllm", machine), "a GPU-only engine never falls back to the CPU").toBe("none");
  expect(quantFit(40 * GiB, null, machine)).toBe("none");
  expect(quantFit(null, null, machine)).toBe("unknown");
  expect(quantFit(4 * GiB, npuEngine, machine)).toBe("npu");
  expect(quantFit(40 * GiB, npuEngine, machine)).toBe("none");
  expect(fitLabel("gpu", null, machine)).toEqual(["Fits in GPU memory", "good"]);
  expect(fitLabel("memory", null, machine)).toEqual(["Too big for the GPU; runs partly on CPU", "warning"]);
  expect(fitLabel("memory", null, { gpu: null, ram: 32 * GiB })).toEqual(["Runs on the CPU", "warning"]);
  expect(fitLabel("none", "vllm", { gpu: null, ram: 32 * GiB })).toEqual(["Needs a supported GPU", "bad"]);
  expect(fitLabel("none", "vllm", machine)).toEqual(["Too big for the GPU", "bad"]);
  expect(fitLabel("npu", npuEngine, machine)).toEqual(["Runs on the NPU", "good"]);
});

test("hardware and size filters combine per quant and drop models with no match", () => {
  const machine = { gpu: 8 * GiB, ram: 32 * GiB };
  const models = [
    model("a/big", 500, [quant("Q4", 6), quant("Q8", 12), quant("F16", 30)]),
    model("a/small", 9000, [quant("Q4", 0.5), quant("Q8", 1)]),
    model("a/unknown", null, [quant("Q4", null)]),
    ...flmResults(flm, "qwen", null),
  ];
  const run = (filters: Partial<ResultFilters>) => filterResults(models, { fit: null, size: null, sort: "match", ...filters }, machine).map((view) => [view.model.repo, view.quants.map((entry) => entry.label)]);
  expect(run({}).map(([repo]) => repo), "no filter keeps the service's order and every quant").toEqual(["a/big", "a/small", "a/unknown", "flm:qwen3"]);
  expect(run({ fit: "gpu" })).toEqual([["a/big", ["Q4"]], ["a/small", ["Q4", "Q8"]]]);
  expect(run({ fit: "memory" }), "fits memory includes what fits the GPU, not the NPU").toEqual([["a/big", ["Q4", "Q8"]], ["a/small", ["Q4", "Q8"]]]);
  expect(run({ fit: "npu" })).toEqual([["flm:qwen3", ["8b"]]]);
  expect(run({ size: "small" })).toEqual([["a/small", ["Q4", "Q8"]]]);
  expect(run({ size: "medium" })).toEqual([["a/big", ["Q4"]], ["flm:qwen3", ["8b"]]]);
  expect(run({ size: "large", fit: "memory" })).toEqual([["a/big", ["Q8"]]]);
  expect(run({ size: "huge", fit: "gpu" })).toEqual([]);
  expect(quantMatches(models[0]!.quants[2]!, { fit: null, size: "huge" }, machine)).toBe(true);
});

test("results sort by downloads or by their smallest matching quant", () => {
  const machine = { gpu: 8 * GiB, ram: 32 * GiB };
  const models = [model("a/big", 500, [quant("Q4", 6), quant("Q8", 12)]), model("a/small", 9000, [quant("Q4", 0.5)]), model("a/unknown", null, [quant("Q4", null)]), model("a/mid", 500, [quant("Q4", 3), quant("Q8", 9)])];
  const order = (sort: ResultFilters["sort"], size: ResultFilters["size"] = null) => filterResults(models, { fit: null, size, sort }, machine).map((view) => view.model.repo);
  expect(order("downloads"), "ties keep the service's order and unknown counts go last").toEqual(["a/small", "a/big", "a/mid", "a/unknown"]);
  expect(order("size")).toEqual(["a/small", "a/mid", "a/big", "a/unknown"]);
  expect(order("size", "large"), "size compares only the quants that pass the filters").toEqual(["a/mid", "a/big"]);
});
