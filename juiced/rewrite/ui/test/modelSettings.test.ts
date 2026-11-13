import { describe, expect, test } from "bun:test";
import { enginePatch, engineDraft, fallback, imageFields, joinArgs, sameTuned, splitArgs, tunedDraft, tunedOptions } from "../src/modelSettings";
import { encodeWav, isSpeechWav, SPEECH_RATE } from "../src/audio16k";
import { parseUpdates, sortModels } from "../src/Models";
import { engineGroups, parseEngines } from "../src/engines";
import type { Model } from "../src/domain";

describe("typed model settings", () => {
  test("drafts round-trip saved values and extra arguments", () => {
    const draft = tunedDraft({ temperature: 0.6, top_k: 20, flash_attention: "on" }, ["--spec-type", "draft-mtp", "--chat-template-kwargs", '{"enable_thinking":false}']);
    expect(draft.temperature).toBe("0.6");
    expect(draft.batch_size).toBe("");
    expect(draft.extra).toBe(`--spec-type draft-mtp --chat-template-kwargs '{"enable_thinking":false}'`);
    expect(splitArgs(draft.extra)).toEqual(["--spec-type", "draft-mtp", "--chat-template-kwargs", '{"enable_thinking":false}']);
    expect(sameTuned(draft, tunedDraft({ temperature: 0.6, top_k: 20, flash_attention: "on" }, splitArgs(draft.extra)))).toBe(true);
    const options = tunedOptions({ ...draft, temperature: " 0.7 ", reasoning_budget: "-1", cache_type_k: "q8_0" });
    expect(options).toMatchObject({ temperature: 0.7, top_k: 20, reasoning_budget: -1, flash_attention: "on", cache_type_k: "q8_0", batch_size: null, top_p: null });
  });
  test("out-of-range, fractional and unknown values fail before saving", () => {
    const base = tunedDraft({}, []);
    for (const [key, value] of [["top_p", "1.5"], ["top_k", "2.5"], ["reasoning_budget", "-2"], ["batch_size", "0"], ["cache_type_v", "q3"], ["temperature", "1e2"], ["min_p", "abc"]] as const)
      expect(() => tunedOptions({ ...base, [key]: value })).toThrow();
    expect(() => splitArgs("--chat-template 'open")).toThrow("quote");
    expect(splitArgs(` --a "b c"  d\\ e `)).toEqual(["--a", "b c", "d e"]);
    expect(joinArgs(["a b", "it's"])).toBe(`'a b' 'it'\\''s'`);
    expect(splitArgs(joinArgs(["a b", "it's"]))).toEqual(["a b", "it's"]);
  });
  test("empty fields describe what applies instead", () => {
    expect(fallback("temperature", { temperature: 0.9 }, { temperature: 0.8 })).toBe("0.9 (profile)");
    expect(fallback("temperature", {}, { temperature: 0.8 })).toBe("Default 0.8");
    expect(fallback("batch_threads", null, { batch_threads: null })).toBe("Engine default");
  });
  test("image defaults send only changed keys and null to clear", () => {
    const saved = engineDraft(imageFields, { steps: 8, width: 1024, height: 1024, sampling_method: "euler", seed: null });
    expect(enginePatch(imageFields, saved, { ...saved, steps: "4", width: "", sampling_method: "dpm++2m" })).toEqual({ steps: 4, width: null, sampling_method: "dpm++2m" });
    expect(() => enginePatch(imageFields, saved, { ...saved, width: "1001" })).toThrow();
    expect(() => enginePatch(imageFields, saved, { ...saved, sampling_method: "has space" })).toThrow();
  });
});

describe("speech audio conversion", () => {
  test("encoded WAV is 16-bit mono at 16 kHz and recognized as ready", async () => {
    const wav = encodeWav(Float32Array.from([0, 1, -1, 0.5]));
    expect(wav.length).toBe(44 + 8);
    const view = new DataView(wav.buffer);
    expect(view.getUint32(24, true)).toBe(SPEECH_RATE);
    expect(view.getInt16(46, true)).toBe(32767);
    expect(view.getInt16(48, true)).toBe(-32768);
    expect(await isSpeechWav(new Blob([wav]))).toBe(true);
    const stereo = encodeWav(new Float32Array(4)); new DataView(stereo.buffer).setUint16(22, 2, true);
    expect(await isSpeechWav(new Blob([stereo]))).toBe(false);
    expect(await isSpeechWav(new Blob(["ID3 mp3 bytes"]))).toBe(false);
  });
});

describe("model list helpers", () => {
  const model = (id: string, extra: Partial<Model>) => ({ id, loaded: false, state: "unloaded", policy: "on-demand", resident_bytes: 1, ...extra }) as Model;
  test("sorting is stable by name and reversible", () => {
    const models = [model("b", { resident_bytes: 5 }), model("a", { loaded: true, resident_bytes: 9 }), model("c", { policy: "disabled", resident_bytes: 1 })];
    expect(sortModels(models, null).map((item) => item.id)).toEqual(["b", "a", "c"]);
    expect(sortModels(models, { key: "name", descending: false }).map((item) => item.id)).toEqual(["a", "b", "c"]);
    expect(sortModels(models, { key: "memory", descending: true }).map((item) => item.id)).toEqual(["a", "b", "c"]);
    expect(sortModels(models, { key: "state", descending: false }).map((item) => item.id)).toEqual(["a", "b", "c"]);
  });
  test("update flags come only from explicit true values", () => {
    expect(parseUpdates({ data: [{ id: "a", update_available: true }, { id: "b", update_available: false }, { id: "c" }, null] })).toEqual(["a"]);
    expect(parseUpdates(null)).toEqual([]);
  });
  test("an installed older build turns the pinned build into an update", () => {
    const engines = parseEngines({ generation: 1, installable: [{ engine: "llama.cpp", backend: "rocm", availability: "available", build: { tag: "b12032", archive_bytes: 10 } }], installed: [{ package: "package-old", engine: "llama.cpp", release: { backend: "rocm", tag: "b11580" }, models: [{ model: "m" }] }], jobs: [] });
    const chips = engineGroups(engines, [])[0]!.chips;
    expect(chips.find((chip) => chip.state === "installable")).toMatchObject({ version: "b12032", replaces: "b11580" });
    expect(chips.find((chip) => chip.state === "installed")).toMatchObject({ version: "b11580", models: 1 });
  });
});
