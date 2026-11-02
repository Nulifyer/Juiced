import { useEffect, useMemo, useRef, useState } from "react";
import {
  isLogEntry,
  mergeEntries,
  parseEntry,
  snapshotRestarted,
  type LogEntry,
} from "./entries";
export function useLogStream() {
  const [entries, setEntries] = useState<LogEntry[]>([]);
  const [status, setStatus] = useState("Connecting");
  const [error, setError] = useState("");
  const buffer = useRef<LogEntry[]>([]);
  const cutoff = useRef(0);
  const lastSeq = useRef(0);
  const lastEntry = useRef<LogEntry | undefined>(undefined);
  useEffect(() => {
    let stopped = false,
      dirty = false,
      attempt = 0;
    let socket: WebSocket | undefined, retry: ReturnType<typeof setTimeout>;
    let watchdog: ReturnType<typeof setTimeout>;
    const flush = setInterval(() => {
      if (dirty) {
        dirty = false;
        setEntries(buffer.current);
      }
    }, 250);
    function connect() {
      if (stopped) return;
      setStatus(attempt ? "Reconnecting" : "Connecting");
      const previousEntry = lastEntry.current;
      let snapshot = false;
      let pending: LogEntry[] = [];
      const ws = new WebSocket(
        `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/logs/stream`,
      );
      socket = ws;
      watchdog = setTimeout(() => {
        setError("The runtime did not send a log snapshot. Retrying.");
        ws.close();
      }, 10000);
      ws.onopen = () =>
        ws.send(JSON.stringify({ type: "logs.subscribe", after_seq: null }));
      ws.onmessage = (event) => {
        if (stopped) return;
        try {
          const message = JSON.parse(event.data);
          if (
            message.type === "logs.snapshot" &&
            Array.isArray(message.entries)
          ) {
            const initial = message.entries.filter(isLogEntry) as LogEntry[];
            const latest = Math.max(0, ...initial.map((e) => e.seq));
            if (snapshotRestarted(previousEntry, initial)) cutoff.current = 0;
            buffer.current = mergeEntries(
              [],
              [...initial, ...pending].filter((e) => e.seq > cutoff.current),
            );
            const newest = [...initial, ...pending]
              .sort((a, b) => a.seq - b.seq)
              .at(-1);
            lastEntry.current = newest;
            lastSeq.current = newest?.seq || latest;
            pending = [];
            snapshot = true;
            dirty = true;
            attempt = 0;
            clearTimeout(watchdog);
            setStatus("Live");
            setError("");
          } else if (
            message.type === "logs.entry" &&
            isLogEntry(message.entry)
          ) {
            const e = message.entry;
            if (!snapshot) {
              pending = mergeEntries(pending, [e]);
              return;
            }
            if (e.seq > lastSeq.current) lastEntry.current = e;
            lastSeq.current = Math.max(lastSeq.current, e.seq);
            if (e.seq > cutoff.current) {
              buffer.current = mergeEntries(buffer.current, [e]);
              dirty = true;
            }
          } else if (message.type === "error") {
            setError(
              message.error?.message ||
                "The runtime rejected the log subscription.",
            );
            ws.close();
          }
        } catch {
          setError("An unreadable log packet was skipped.");
        }
      };
      ws.onerror = () =>
        setError(
          "Cannot connect to runtime logs. Check connectivity and access settings.",
        );
      ws.onclose = () => {
        clearTimeout(watchdog);
        if (stopped) return;
        setStatus("Reconnecting");
        retry = setTimeout(connect, Math.min(15000, 1000 * 2 ** attempt++));
      };
    }
    connect();
    return () => {
      stopped = true;
      clearInterval(flush);
      clearTimeout(retry);
      clearTimeout(watchdog);
      socket?.close();
    };
  }, []);
  return {
    entries,
    parsed: useMemo(() => entries.map(parseEntry), [entries]),
    status,
    error,
    clear: () => {
      cutoff.current = lastSeq.current;
      buffer.current = [];
      setEntries([]);
    },
  };
}
export type RuntimeLogs = ReturnType<typeof useLogStream>;
