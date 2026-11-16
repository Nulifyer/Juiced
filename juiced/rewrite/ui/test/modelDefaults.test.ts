import { expect, test } from "bun:test";
import { appliedKey, changedSince, parseModelDefaults } from "../src/modelDefaults";
import { fallback } from "../src/modelSettings";

const view = {
  available: true, enabled: true,
  entries: [
    { flag: "--spec-type", value: "ngram-simple", source: "recipe", reason: "Measured.", state: "applied" },
    { flag: "--metrics", value: null, source: "juiced", reason: "Counters.", state: "applied" },
    { flag: "--temp", value: "0.6", source: "family", reason: "Family.", state: "overridden" },
    { flag: "--bogus", value: 3, source: "recipe", reason: "", state: "applied" },
  ],
  sampler: { temperature: { value: 1, source: "model-file" }, top_k: { value: "20", source: "juiced" }, min_p: null, top_p: { value: 1, source: "elsewhere" } },
};

test("defaults parse entries and sampler values and drop malformed ones", () => {
  const defaults = parseModelDefaults(view)!;
  expect(defaults.entries.map((entry) => entry.flag)).toEqual(["--spec-type", "--metrics", "--temp"]);
  expect(defaults.sampler.top_p).toBeNull();
  expect(parseModelDefaults({ enabled: true })).toBeNull();
  expect(defaults.tunedStale).toBe(false);
  expect(parseModelDefaults({ ...view, tuned_stale: true })!.tunedStale).toBe(true);
  expect(defaults.tunedTrial).toBe(false);
  expect(parseModelDefaults({ ...view, tuned_trial: true })!.tunedTrial).toBe(true);
});

test("the change note lists new non-Juiced arguments once per client", () => {
  const defaults = parseModelDefaults(view)!;
  expect(changedSince(defaults, null)).toEqual([]);
  expect(changedSince(defaults, appliedKey(defaults))).toEqual([]);
  expect(changedSince(defaults, "--metrics")).toEqual(["--spec-type ngram-simple"]);
  expect(changedSince({ ...defaults, enabled: false }, null)).toEqual([]);
});

test("an empty generation field names the model file or Juiced default before llama.cpp's", () => {
  const sampler = parseModelDefaults(view)!.sampler;
  expect(fallback("temperature", {}, { temperature: 0.8 }, sampler)).toBe("1 (model file)");
  expect(fallback("top_k", {}, { top_k: 40 }, sampler)).toBe("20 (Juiced)");
  expect(fallback("min_p", {}, { min_p: 0.05 }, sampler)).toBe("Default 0.05");
  expect(fallback("temperature", { temperature: 0.9 }, {}, sampler)).toBe("0.9 (profile)");
});

import { parseTune } from "../src/modelDefaults";

test("a tune view reads running progress and a finished result", () => {
  const running = parseTune({ state: "running", step: "Measuring physical batch 1024", candidates: [{ label: "current settings", speed: { prefill_tps: 332.4, decode_tps: 22.8, rewrite_tps: 22.7 } }] });
  expect(running.state).toBe("running");
  expect(running.candidates[0]!.speed.prefill_tps).toBe(332.4);
  const done = parseTune({ state: "done", result: { arguments: ["--ubatch-size", "1024"], measurements: { candidates: [{ label: "a", speed: {} }] } } });
  expect(done.warning).toBeUndefined();
  expect(parseTune({ state: "done", result: { arguments: [], warning: "m stayed loaded after Tune." } }).warning).toBe("m stayed loaded after Tune.");
  expect(done.arguments).toEqual(["--ubatch-size", "1024"]);
  expect(done.candidates).toHaveLength(1);
  expect(done.candidates[0]!.wrong).toEqual([]);
  const rejected = parseTune({ state: "done", result: { measurements: { candidates: [{ label: "flash attention on", speed: {}, wrong: ["code rewrite", 3] }] } } });
  expect(rejected.candidates[0]!.wrong).toEqual(["code rewrite"]);
  expect(parseTune({ state: "weird" }).state).toBe("idle");
});
