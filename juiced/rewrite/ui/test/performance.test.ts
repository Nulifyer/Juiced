import { describe, expect, test } from "bun:test";
import type { RequestRecord } from "../src/domain";
import { failed, formatMs, modelPerformance, timing } from "../src/performance";

const chat = (id: number, fields: Partial<RequestRecord>): RequestRecord => ({ id, model: "Ornith-1.5-35B-A3B-GGUF", operation: "chat/completions", outcome: "completed", status: 200, elapsed_ms: 1000, queue_wait_ms: 0, ...fields });
const cold = chat(13, { elapsed_ms: 16866, input_tokens: 6477, output_tokens: 110, cached_tokens: 0, prompt_processed_tokens: 6477, prompt_tokens_per_second: 507.50559004439674, decode_tokens_per_second: 27.11526425193215, session_cache: { key: "prefix", path: "cold", slot: 1, saved_previous: false } });
const hit = chat(18, { elapsed_ms: 3047, input_tokens: 6727, output_tokens: 69, cached_tokens: 6642, prompt_processed_tokens: 85, prompt_tokens_per_second: 185.8967769872912, decode_tokens_per_second: 26.688090400411625, session_cache: { key: "prefix", path: "slot-hit", slot: 1, saved_previous: false } });
const speech = chat(19, { model: "kokoro-v1", operation: "audio/speech", elapsed_ms: 698, decode_tokens_per_second: null, prompt_tokens_per_second: null, input_tokens: null, output_tokens: null });

describe("request timing", () => {
  test("splits a cold prompt into prompt and generate time from engine rates", () => {
    const parts = timing(cold);
    expect(Math.round(parts.prompt)).toBe(12762);
    expect(Math.round(parts.generate)).toBe(4057);
    expect(parts.queue).toBe(0);
    expect(Math.round(parts.other)).toBe(47);
  });
  test("a reused prompt only spends time on its new tokens", () => {
    expect(Math.round(timing(hit).prompt)).toBe(457);
  });
  test("queue wait comes first and engine time never exceeds the remainder", () => {
    const parts = timing(chat(1, { elapsed_ms: 1000, queue_wait_ms: 600, input_tokens: 1000, prompt_tokens_per_second: 1000, output_tokens: 10, decode_tokens_per_second: 10 }));
    expect(parts.queue).toBe(600);
    expect(parts.prompt + parts.generate).toBeCloseTo(400);
    expect(parts.other).toBe(0);
  });
  test("work without token rates is all Other", () => {
    expect(timing(speech)).toEqual({ queue: 0, prompt: 0, generate: 0, other: 698, total: 698 });
  });
});

describe("model performance", () => {
  test("compares reused and full prompt time and counts cached prompt tokens", () => {
    const summaries = modelPerformance([speech, hit, cold]);
    const ornith = summaries.find((entry) => entry.model === cold.model), kokoro = summaries.find((entry) => entry.model === "kokoro-v1");
    expect(ornith!.count).toBe(2);
    expect(Math.round(ornith!.reusedPromptMs!)).toBe(457);
    expect(Math.round(ornith!.fullPromptMs!)).toBe(12762);
    expect(ornith!.cachedShare).toBeCloseTo(6642 / (6727 + 6477));
    expect(ornith!.decodeTrend).toEqual([cold.decode_tokens_per_second!, hit.decode_tokens_per_second!]);
    expect(kokoro!.decodeRate).toBeNull();
    expect(kokoro!.medianMs).toBe(698);
  });
  test("failures count separately and stay out of medians", () => {
    const [summary] = modelPerformance([chat(2, { outcome: "upstream_error", status: 502, elapsed_ms: 90000 }), chat(1, { elapsed_ms: 1000 })]);
    expect(summary!.failed).toBe(1);
    expect(summary!.medianMs).toBe(1000);
    expect(failed(chat(3, { outcome: "completed", status: 500 }))).toBe(true);
    expect(failed(chat(4, { outcome: "cancelled", status: null }))).toBe(false);
  });
  test("formats durations compactly", () => {
    expect([formatMs(698), formatMs(3047), formatMs(16866), formatMs(null)]).toEqual(["698 ms", "3.05 s", "16.9 s", "—"]);
  });
});
