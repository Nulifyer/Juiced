import { expect, test } from "bun:test";
import type { Model } from "../src/domain";
import { modelGroups } from "../src/modelPicker";

const model = (id: string, policy: Model["policy"], loaded = false) => ({ id, policy, loaded } as Model);
const models = [model("b-off", "disabled"), model("a-hot", "hot", true), model("c-demand", "on-demand"), model("a-off", "disabled")];

test("loaded models come first and disabled ones stay behind one option", () => {
  expect(modelGroups(models, false)).toEqual({ groups: [{ label: "Loaded", ids: ["a-hot"] }, { label: "Enabled", ids: ["c-demand"] }], hidden: 2 });
});

test("showing disabled models lists them last", () => {
  expect(modelGroups(models, true).groups.at(-1)).toEqual({ label: "Disabled", ids: ["a-off", "b-off"] });
});

test("a selected disabled model stays listed", () => {
  expect(modelGroups(models, false, "b-off")).toEqual({ groups: [{ label: "Loaded", ids: ["a-hot"] }, { label: "Enabled", ids: ["c-demand"] }, { label: "Disabled", ids: ["b-off"] }], hidden: 1 });
});

import { logText, tailName } from "../src/Diagnostics";

test("log rows keep the distinguishing end of a model name and drop the engine clock", () => {
  expect(tailName("Qwen3.6-35B-A3B-GGUF-UD-Q4_K_M")).toBe("…5B-A3B-GGUF-UD-Q4_K_M");
  expect(tailName("kokoro-v1")).toBe("kokoro-v1");
  expect(logText("0.24.258.110 I srv load")).toBe("I srv load");
  expect(logText("plain line")).toBe("plain line");
});
