import { expect, test } from "bun:test";
import { ago, parseCheck } from "../src/ModelCheck";

test("a check result keeps valid steps and the current flag", () => {
  const result = parseCheck({ model: "m", checked_at_ms: 1, passed: false, current: true, steps: [
    { id: "chat", label: "Chat", status: "pass", detail: "1 2 3", ms: 900 },
    { id: "tools", label: "Tool calls", status: "fail", detail: "No parsed tool call." },
    { id: "bad", label: "Bad", status: "maybe", detail: "" },
  ] })!;
  expect(result.steps.map((step) => step.id)).toEqual(["chat", "tools"]);
  expect(result.current).toBe(true);
  expect(parseCheck({ model: "m" })).toBeNull();
});

test("check age reads in minutes, hours and days", () => {
  const now = 10 * 24 * 3600 * 1000;
  expect(ago(now - 20_000, now)).toBe("just now");
  expect(ago(now - 5 * 60_000, now)).toBe("5 min ago");
  expect(ago(now - 3 * 3600_000, now)).toBe("3 h ago");
  expect(ago(now - 2 * 24 * 3600_000, now)).toBe("2 d ago");
});
