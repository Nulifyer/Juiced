export type LogEntry = {
  seq: number;
  timestamp: string;
  severity: string;
  tag: string;
  line: string;
};
export type ParsedLog = LogEntry & {
  level: string;
  message: string;
  inference?: {
    model: string;
    input: number;
    output: number;
    ttft: number;
    tps: number;
  };
};
export const MAX_LOG_ENTRIES = 2000;
const MAX_LOG_CHARS = 2 * 1024 * 1024;
export function isLogEntry(value: unknown): value is LogEntry {
  if (!value || typeof value !== "object") return false;
  const e = value as LogEntry;
  return (
    Number.isSafeInteger(e.seq) &&
    e.seq > 0 &&
    [e.timestamp, e.severity, e.tag, e.line].every((v) => typeof v === "string")
  );
}
export function mergeEntries(
  previous: LogEntry[],
  incoming: LogEntry[],
): LogEntry[] {
  const bySeq = new Map(previous.map((e) => [e.seq, e]));
  for (const e of incoming) bySeq.set(e.seq, e);
  const sorted = [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  let chars = 0,
    start = sorted.length;
  while (start > 0 && sorted.length - start < MAX_LOG_ENTRIES) {
    const length = sorted[start - 1].line.length;
    if (chars + length > MAX_LOG_CHARS) break;
    chars += length;
    start--;
  }
  return sorted.slice(start);
}
export function parseEntry(entry: LogEntry): ParsedLog {
  const clean = entry.line.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "");
  const prefix = /^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d\.\d+ \[[^\]]+\] \([^)]*\) /;
  const message = clean.replace(prefix, "");
  let level = entry.severity.toLowerCase();
  if (level === "warn") level = "warning";
  if (!["trace", "debug", "info", "warning", "error", "fatal"].includes(level))
    level = "unknown";
  // Child output is wrapped as Info by Process; only recognize its explicit severity prefix.
  if (entry.tag === "Process") {
    const child = message.match(/^\d\d\.\d\d\.\d{3}\.\d{3} ([EW]) /);
    if (child) level = child[1] === "E" ? "error" : "warning";
  }
  const inference =
    entry.tag === "Telemetry"
      ? message.match(
          /^Inference completed: model=(.+), tokens=\d+ \(in=(\d+), out=(\d+)\), ttft=([\d.]+)s, tps=([\d.]+)/,
        )
      : null;
  return {
    ...entry,
    level,
    message,
    ...(inference
      ? {
          inference: {
            model: inference[1],
            input: Number(inference[2]),
            output: Number(inference[3]),
            ttft: Number(inference[4]),
            tps: Number(inference[5]),
          },
        }
      : {}),
  };
}
export function filterEntries(
  entries: ParsedLog[],
  search: string,
  level: string,
  component: string,
) {
  const query = search.trim().toLowerCase();
  return entries.filter(
    (e) =>
      (!query || `${e.line} ${e.tag}`.toLowerCase().includes(query)) &&
      (level === "all" ||
        (level === "issues"
          ? ["warning", "error", "fatal"].includes(e.level)
          : e.level === level)) &&
      (component === "all" || e.tag === component),
  );
}

export function snapshotRestarted(
  last: LogEntry | undefined,
  snapshot: LogEntry[],
) {
  if (!last || !snapshot.length) return Boolean(last);
  const matching = snapshot.find((e) => e.seq === last.seq);
  return (
    snapshot[snapshot.length - 1].seq < last.seq ||
    Boolean(matching && matching.timestamp !== last.timestamp)
  );
}
