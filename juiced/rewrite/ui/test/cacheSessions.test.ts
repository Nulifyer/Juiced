import { describe, expect, test } from "bun:test";
import { parseSessions, size, turnDetail } from "../src/CacheSessions";

describe("conversation cache wording", () => {
  test("sizes drop needless decimals", () => {
    expect(size(669.6 * 1024 ** 2)).toBe("670 MiB");
    expect(size(8 * 1024 ** 3)).toBe("8 GiB");
    expect(size(6.5 * 1024 ** 2)).toBe("6.5 MiB");
    expect(size(64)).toBe("64 B");
  });
  test("turn details name cached and new tokens", () => {
    const base = { id: 1, model: "m", path: "slot-hit", restoreMs: null, elapsedMs: 900 };
    expect(turnDetail({ ...base, cached: 9, processed: 4 })).toBe("9 cached + 4 new tokens · 0.90 s");
    expect(turnDetail({ ...base, path: "restored", cached: 20, processed: 27, restoreMs: 81.4, elapsedMs: 650 })).toBe("20 cached + 27 new tokens · restored in 81 ms · 0.65 s");
    expect(turnDetail({ ...base, path: "cold", cached: 0, processed: 3177, elapsedMs: null })).toBe("3,177 new tokens");
  });
  test("an unloaded model's sessions are not marked as changed", () => {
    const view = parseSessions({ configuration_generation: 1, settings: { mode: "persistent" }, models: [], totals: {}, sessions: [{ session: "a", model: null, execution_current: null }, { session: "b", model: "m", execution_current: false }] });
    expect(view.sessions.map((row) => [row.model, row.execution_current])).toEqual([[null, null], ["m", false]]);
  });
});
