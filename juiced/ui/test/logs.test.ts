import { expect, test } from "bun:test";
import {
  filterEntries,
  isLogEntry,
  MAX_LOG_ENTRIES,
  mergeEntries,
  parseEntry,
  snapshotRestarted,
  type LogEntry,
} from "../src/logs/entries";
const entry = (seq: number, line = "hello"): LogEntry => ({
  seq,
  timestamp: "2026-11-02 00:18:04.201",
  severity: "Info",
  tag: "Telemetry",
  line,
});
test("native telemetry parses model/timings and preserves the exact raw line", () => {
  const raw =
    "2026-11-02 00:18:04.201 [Info] (Telemetry) Inference completed: model=Ornith-1.5-35B-A3B-GGUF, tokens=279 (in=23, out=256), ttft=0.230s, tps=28.68";
  const e = parseEntry(entry(212, raw));
  expect(e.line).toBe(raw);
  expect(e.inference).toEqual({
    model: "Ornith-1.5-35B-A3B-GGUF",
    input: 23,
    output: 256,
    ttft: 0.23,
    tps: 28.68,
  });
  expect(e.message).toStartWith("Inference completed:");
});
test("unknown and multiline output stays visible; only explicit backend severity is inferred", () => {
  expect(
    parseEntry({
      ...entry(1, "first\nsecond <script>alert(1)</script>"),
      severity: "Notice",
    }).level,
  ).toBe("unknown");
  expect(parseEntry(entry(2, "Error count = 0")).level).toBe("info");
  const e = parseEntry({
    ...entry(
      3,
      "\x1b[31m2026-11-02 00:18:04.201 [Info] (Process) 22.56.093.314 E model failed\x1b[0m",
    ),
    tag: "Process",
  });
  expect(e.level).toBe("error");
  expect(e.severity).toBe("Info");
  expect(e.line).toContain("\x1b[31m");
  expect(e.message).not.toContain("\x1b");
});
test("snapshot/live overlap is sorted and deduplicated; buffers have count and size bounds", () => {
  expect(
    mergeEntries([entry(3), entry(1)], [entry(2), entry(3)]).map((e) => e.seq),
  ).toEqual([1, 2, 3]);
  const retained = mergeEntries(
    [],
    Array.from({ length: 2500 }, (_, i) => entry(i + 1)),
  );
  expect(retained.length).toBe(MAX_LOG_ENTRIES);
  expect(retained[0].seq).toBe(501);
  const sized = mergeEntries(
    [],
    [entry(1, "x".repeat(1100000)), entry(2, "y".repeat(1100000))],
  );
  expect(sized.map((e) => e.seq)).toEqual([2]);
});
test("filters apply to raw and parsed views without changing entries", () => {
  const list = [
    parseEntry(entry(1, "Qwen loaded")),
    parseEntry({
      ...entry(2, "Ornith failed"),
      severity: "Error",
      tag: "Router",
    }),
  ];
  expect(
    filterEntries(list, "ORNITH", "issues", "Router").map((e) => e.seq),
  ).toEqual([2]);
  expect(filterEntries(list, "Qwen", "issues", "all")).toEqual([]);
  expect(filterEntries(list, "", "all", "Telemetry").length).toBe(1);
  expect(list.length).toBe(2);
});
test("malformed packets cannot enter the log buffer", () => {
  expect(isLogEntry(entry(1))).toBe(true);
  for (const bad of [
    null,
    {},
    { ...entry(1), seq: -1 },
    { ...entry(1), line: { html: "unsafe" } },
    { ...entry(1), seq: 1.3 },
  ])
    expect(isLogEntry(bad)).toBe(false);
});

test("reconnect detects restarted sequence numbers even after the new server passes the old cursor", () => {
  expect(snapshotRestarted(entry(10), [entry(11)])).toBe(false);
  expect(snapshotRestarted(entry(10), [entry(1)])).toBe(true);
  expect(
    snapshotRestarted(entry(10), [
      { ...entry(10), timestamp: "2026-11-02 01:00:00.000" },
      entry(11),
    ]),
  ).toBe(true);
});
