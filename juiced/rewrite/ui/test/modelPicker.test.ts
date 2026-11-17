import { expect, test } from "bun:test";
import type { Model } from "../src/domain";
import { matchesQuery, matchingModels, modelType, pickerView } from "../src/modelPicker";

const model = (id: string, policy: Model["policy"], loaded = false, operations = ["chat/completions", "completions"], more: Record<string, unknown> = {}) => ({ id, policy, loaded, operations, ...more } as Model);
const models = [
  model("b-off", "disabled"), model("a-hot", "hot", true), model("c-demand", "on-demand"), model("a-off", "disabled"),
  model("whisper-tiny", "on-demand", false, ["audio/transcriptions"]), model("kokoro", "disabled", false, ["audio/speech"]),
  model("sd-turbo", "disabled", false, ["images/generations"], { recipe: "sd-cpp" }), model("embed-small", "hot", true, ["embeddings"]),
  model("qwen-vl-q4", "disabled", false, ["chat/completions"], { labels: ["chat", "vision", "tool-calling"] }),
];
const ids = (view: ReturnType<typeof pickerView>) => view.groups.map((group) => [group.label, ...group.models.map((item) => item.id)]);

test("groups run Loaded, Enabled, Disabled and order by type, then ID", () => {
  const view = pickerView(models, { showDisabled: true });
  expect(ids(view)).toEqual([
    ["Loaded", "a-hot", "embed-small"],
    ["Enabled", "c-demand", "whisper-tiny"],
    ["Disabled", "a-off", "b-off", "qwen-vl-q4", "kokoro", "sd-turbo"],
  ]);
  expect([view.shown, view.total, view.hidden]).toEqual([9, 9, 0]);
});

test("a long list leaves disabled models out until asked, and counts them", () => {
  const view = pickerView(models);
  expect(ids(view)).toEqual([["Loaded", "a-hot", "embed-small"], ["Enabled", "c-demand", "whisper-tiny"]]);
  expect([view.shown, view.total, view.hidden]).toEqual([4, 9, 5]);
});

test("a selected disabled model stays listed", () => {
  const view = pickerView(models, { selected: "b-off" });
  expect(ids(view).at(-1)).toEqual(["Disabled", "b-off"]);
  expect(view.hidden).toBe(4);
});

test("a search lists disabled matches without asking", () => {
  const view = pickerView(models, { query: "off" });
  expect(ids(view)).toEqual([["Disabled", "a-off", "b-off"]]);
  expect([view.shown, view.total, view.hidden]).toEqual([2, 9, 0]);
});

test("a short list and a list with nothing enabled show disabled models", () => {
  expect(pickerView(models.slice(0, 4)).hidden).toBe(0);
  const off = Array.from({ length: 12 }, (_, index) => model(`off-${index}`, "disabled"));
  expect(pickerView(off).shown).toBe(12);
  expect(pickerView(models, { type: "images" }).shown).toBe(1);
});

test("search matches every word against ID, type words, labels, engine and state", () => {
  const find = (query: string) => models.filter((item) => matchesQuery(item, query)).map((item) => item.id);
  expect(find("QWEN q4")).toEqual(["qwen-vl-q4"]);
  expect(find("speech")).toEqual(["whisper-tiny", "kokoro"]);
  expect(find("stt")).toEqual(["whisper-tiny"]);
  expect(find("tts")).toEqual(["kokoro"]);
  expect(find("image")).toEqual(["sd-turbo"]);
  expect(find("sd-cpp")).toEqual(["sd-turbo"]);
  expect(find("vision")).toEqual(["qwen-vl-q4"]);
  expect(find("embed loaded")).toEqual(["embed-small"]);
  expect(find("on demand")).toEqual(["c-demand", "whisper-tiny"]);
  expect(find("nothing-like-this")).toEqual([]);
});

test("the model list search matches the same way as the picker", () => {
  const ids = (query: string) => matchingModels(models, query).map((item) => item.id);
  expect(ids("qwen vision")).toEqual(["qwen-vl-q4"]);
  expect(ids("  AUDIO/SPEECH ")).toEqual(["kokoro"]);
  expect(ids("loaded")).toEqual(models.filter((item) => item.loaded).map((item) => item.id));
});

test("the type filter narrows the list and type counts cover every offered model", () => {
  const view = pickerView(models, { type: "chat", showDisabled: true });
  expect(view.groups.flatMap((group) => group.models).every((item) => modelType(item) === "chat")).toBe(true);
  expect(view.types).toEqual([
    { type: "chat", label: "Chat", count: 5 }, { type: "embeddings", label: "Embeddings", count: 1 },
    { type: "transcription", label: "Speech to text", count: 1 }, { type: "speech", label: "Text to speech", count: 1 }, { type: "images", label: "Images", count: 1 },
  ]);
});

test("disabledModels never drops disabled models except the selected one", () => {
  const view = pickerView(models, { disabledModels: "never", selected: "kokoro", query: "o" });
  expect(view.total).toBe(5);
  expect(ids(view).at(-1)).toEqual(["Disabled", "kokoro"]);
});

import { logText, tailName } from "../src/Diagnostics";

test("log rows keep the distinguishing end of a model name and drop the engine clock", () => {
  expect(tailName("Qwen3.6-35B-A3B-GGUF-UD-Q4_K_M")).toBe("…5B-A3B-GGUF-UD-Q4_K_M");
  expect(tailName("kokoro-v1")).toBe("kokoro-v1");
  expect(logText("0.24.258.110 I srv load")).toBe("I srv load");
  expect(logText("plain line")).toBe("plain line");
});
