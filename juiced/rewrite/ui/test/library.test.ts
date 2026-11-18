import { expect, test } from "bun:test";
import { filterResults, fitLabel, flmJob, flmResults, isSearch, mergePages, npuEngine, parseFlm, parseSearch, progress, quantFit, quantMatches, rowQuant, searchPath, suggest, suggestedTasks, suggestionPath, type LibraryModel, type ResultFilters, type Search } from "../src/libraryData";

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

const catalogModel = (id: string, task: string, gib: number | null, extra: Record<string, unknown> = {}, local: Record<string, unknown> = {}) => parseSearch({ data: [{ id, repo: `catalog/${id}`, name: id, task, source: "catalog", ...extra, quants: [
  { label: "Q2_K", file: `${id}-Q2_K.gguf`, bytes: gib === null ? null : gib * GiB / 2, pullable: true },
  { label: "Q4_K_M", file: `${id}-Q4_K_M.gguf`, bytes: gib === null ? null : gib * GiB, recommended: true, pullable: true, local },
] }] }).models[0]!;
const picks = (models: LibraryModel[], machine: { gpu: number | null; ram: number | null }) => suggest(models, machine).map((entry) => `${entry.model.id} ${entry.quant.label}`);

test("suggestions take the largest recommended quant of each type that fits a share of this machine", () => {
  const models = [
    catalogModel("chat-1", "chat", 1), catalogModel("chat-2.5", "chat", 2.5), catalogModel("chat-5", "chat", 5), catalogModel("chat-9", "chat", 9), catalogModel("chat-17", "chat", 17), catalogModel("chat-60", "chat", 60),
    catalogModel("embed-0.6", "embeddings", 0.6), catalogModel("embed-8", "embeddings", 8),
    catalogModel("whisper-1.5", "transcription", 1.5), catalogModel("whisper-3", "transcription", 3),
    catalogModel("kokoro", "tts", 0.3), catalogModel("sd-turbo", "image", 1.9),
    catalogModel("music", "audio-generation", 1), catalogModel("trellis", "3d", 1),
  ];
  expect(picks(models, { gpu: 24 * GiB, ram: 64 * GiB }), "half of 24 GiB for the first chat model, an eighth for the rest").toEqual(["chat-9 Q4_K_M", "chat-2.5 Q4_K_M", "embed-0.6 Q4_K_M", "whisper-1.5 Q4_K_M", "kokoro Q4_K_M", "sd-turbo Q4_K_M"]);
  expect(picks(models, { gpu: 88 * GiB, ram: 107 * GiB }), "the eighth stops at 4 GiB however large the machine").toEqual(["chat-17 Q4_K_M", "chat-2.5 Q4_K_M", "embed-0.6 Q4_K_M", "whisper-3 Q4_K_M", "kokoro Q4_K_M", "sd-turbo Q4_K_M"]);
  expect(picks(models, { gpu: 8 * GiB, ram: 16 * GiB }), "a small GPU gets no image model: none fits an eighth of it").toEqual(["chat-2.5 Q4_K_M", "embed-0.6 Q4_K_M", "kokoro Q4_K_M"]);
  expect(picks(models, { gpu: null, ram: 32 * GiB }), "without a GPU, system memory counts").toEqual(["chat-9 Q4_K_M", "chat-2.5 Q4_K_M", "embed-0.6 Q4_K_M", "whisper-3 Q4_K_M", "kokoro Q4_K_M", "sd-turbo Q4_K_M"]);
  expect(picks(models, { gpu: null, ram: null }), "an unknown machine gets only small models").toEqual(["chat-2.5 Q4_K_M", "chat-1 Q4_K_M", "embed-0.6 Q4_K_M", "whisper-3 Q4_K_M", "kokoro Q4_K_M", "sd-turbo Q4_K_M"]);
  expect(suggest(models, { gpu: 24 * GiB, ram: 64 * GiB }).map((entry) => entry.model.task), "chat first, then the Type filter's order; audio and 3D are not suggested").toEqual(["chat", "chat", ...suggestedTasks.slice(1)]);
});

test("the first chat suggestion takes the largest quant up to 8 bits that fits its half of the machine", () => {
  const quant = (label: string, gib: number, extra: Record<string, unknown> = {}) => ({ label, file: `coder-${label}.gguf`, bytes: gib * GiB, pullable: true, ...extra });
  const coder = parseSearch({ data: [{ id: "coder", repo: "catalog/coder", name: "coder", task: "chat", source: "catalog", quants: [
    quant("Q4_K_M", 17.3, { recommended: true }), quant("Q6_K", 23.4), quant("Q8_0", 30.3), quant("UD-Q8_K_XL", 33.5), quant("BF16", 56.9), quant("Q5_K_M", 20.2, { pullable: false }),
  ] }] }).models[0]!;
  const small = catalogModel("chat-2.5", "chat", 2.5);
  expect(picks([coder, small], { gpu: 88 * GiB, ram: 107 * GiB }), "44 GiB share: the 8-bit quant fits, the unquantized export is never taken").toEqual(["coder UD-Q8_K_XL", "chat-2.5 Q4_K_M"]);
  expect(picks([coder, small], { gpu: 64 * GiB, ram: 64 * GiB }), "32 GiB share: 8 bits is too big, 6 bits fits").toEqual(["coder Q6_K", "chat-2.5 Q4_K_M"]);
  expect(picks([coder, small], { gpu: 48 * GiB, ram: 64 * GiB }), "24 GiB share: only the recommended quant fits").toEqual(["coder Q4_K_M", "chat-2.5 Q4_K_M"]);
  expect(picks([coder, small], { gpu: null, ram: null }), "an unknown machine keeps the recommended quant").toEqual(["chat-2.5 Q4_K_M"]);
  expect(picks([coder], { gpu: 88 * GiB, ram: 107 * GiB }), "a model is suggested once, not again at its smaller quant").toEqual(["coder UD-Q8_K_XL"]);
});

test("suggestions leave out what is on disk, downloading, gated, of unknown size or not from the catalog", () => {
  const machine = { gpu: 24 * GiB, ram: 64 * GiB };
  const downloading = catalogModel("chat-8", "chat", 8);
  downloading.quants[0]!.jobs = [{ id: "job", state: "downloading", downloaded_bytes: 1, expected_bytes: 10 }];
  const unpullable = catalogModel("chat-7", "chat", 7);
  unpullable.quants[1]!.pullable = false;
  const models = [
    catalogModel("chat-9", "chat", 9, {}, { present: true }), catalogModel("chat-8.5", "chat", 8.5, {}, { present_files: 1 }), downloading, unpullable,
    catalogModel("chat-6", "chat", 6, { gated: true }), catalogModel("chat-5", "chat", null), { ...catalogModel("chat-4", "chat", 4), source: "huggingface" as const },
    catalogModel("chat-3", "chat", 3), catalogModel("chat-2", "chat", 2),
  ];
  expect(picks(models, machine)).toEqual(["chat-3 Q4_K_M", "chat-2 Q4_K_M"]);
  expect(picks([catalogModel("big", "chat", 30)], machine), "nothing fits: no suggestion").toEqual([]);
  expect(picks([{ ...catalogModel("thenoise", "image", 2), quants: catalogModel("thenoise", "image", 2).quants.map((entry) => ({ ...entry, engine: "thenoise" })) }], { gpu: null, ram: 64 * GiB }), "a GPU-only engine is not suggested without a GPU").toEqual([]);
});

test("a result row offers one quant: on disk, downloading, recommended, then the best fit", () => {
  const machine = { gpu: 8 * GiB, ram: 32 * GiB };
  const offered = (quants: ReturnType<typeof quant>[]) => rowQuant(quants, machine)?.label;
  expect(offered([quant("Q2_K", 2), quant("Q4_K_M", 4), quant("Q8_0", 6), quant("F16", 12)]), "the common quant when it fits").toBe("Q4_K_M");
  expect(offered([quant("Q2_K", 2), quant("Q4_K_M", 40), quant("Q8_0", 60)]), "a common quant that does not fit is passed over").toBe("Q2_K");
  expect(offered([quant("IQ3_M", 3), quant("IQ4_XS", 5), quant("BF16", 50)]), "no common quant: the largest that fits").toBe("IQ4_XS");
  expect(offered([quant("Q8_0", 60), quant("F16", 50)]), "nothing fits: the smallest").toBe("F16");
  expect(offered([quant("Q4_K_M", 4), { ...quant("Q5_K_M", 5), recommended: true }])).toBe("Q5_K_M");
  expect(offered([{ ...quant("Q4_K_M", 4), recommended: true }, { ...quant("Q6_K", 6), jobs: [{ id: "j", state: "downloading", downloaded_bytes: 0, expected_bytes: 6 }] }])).toBe("Q6_K");
  expect(offered([{ ...quant("Q4_K_M", 4), recommended: true }, { ...quant("Q8_0", 6), present: true }]), "what is on disk comes first").toBe("Q8_0");
  expect(offered([])).toBeUndefined();
});

test("a filter with no search text searches the service with that filter instead of narrowing the suggestions", () => {
  const none: Search = { query: "", task: null, engine: null, fit: null, size: null };
  expect(isSearch(none)).toBe(false);
  expect(isSearch({ ...none, query: "  " }), "blank text is no search").toBe(false);
  for (const filter of [{ task: "embeddings" }, { engine: "vllm" }, { fit: "gpu" as const }, { size: "small" as const }, { query: "qwen" }]) expect(isSearch({ ...none, ...filter }), JSON.stringify(filter)).toBe(true);
  expect(searchPath({ ...none, task: "embeddings" })).toBe("juiced/library/search?limit=12&task=embeddings");
  expect(searchPath({ ...none, engine: "onnxruntime", query: " tiny " })).toBe("juiced/library/search?limit=12&q=tiny&engine=onnxruntime");
  expect(searchPath(none), "hardware and size are applied to the pages of a plain search").toBe("juiced/library/search?limit=12");
  expect(searchPath({ ...none, task: "chat" }, "abc=="), "the next page keeps the filter").toBe("juiced/library/search?limit=12&task=chat&cursor=abc%3D%3D");
  expect(searchPath({ ...none, task: "chat" })).not.toContain("source=catalog");
  expect(suggestedTasks.map(suggestionPath)).toEqual(["chat", "embeddings", "transcription", "tts", "image"].map((task) => `juiced/library/search?source=catalog&task=${task}&limit=20`));
});

test("pages of one search merge in order and list a repository once", () => {
  const page = (models: unknown[], next: string | null, generation = 1) => parseSearch({ artifact_generation: generation, data: models, page: { next_cursor: next } });
  const merged = mergePages([
    page([{ id: "whisper-tiny", repo: "ggerganov/whisper.cpp", source: "catalog" }, { id: "whisper-base", repo: "ggerganov/whisper.cpp", source: "catalog" }, { repo: "a/one" }], "c1", 7),
    page([{ repo: "A/One" }, { repo: "ggerganov/whisper.cpp" }, { repo: "a/two" }], "c2", 9),
    page([{ repo: "a/three" }], null, 9),
  ])!;
  expect(merged.models.map((item) => item.id)).toEqual(["whisper-tiny", "whisper-base", "a/one", "a/two", "a/three"]);
  expect([merged.generation, merged.next], "the first page's state and the last page's cursor").toEqual([7, null]);
  expect(mergePages([])).toBeNull();
});
