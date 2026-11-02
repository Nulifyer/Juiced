import { useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDown,
  Download,
  Pause,
  Play,
  Search,
  Trash2,
  Terminal,
} from "lucide-react";
import { Button } from "./t3/button";
import {
  filterEntries,
  MAX_LOG_ENTRIES,
  parseEntry,
  type LogEntry,
} from "../logs/entries";
import type { RuntimeLogs as LogState } from "../logs/useLogStream";
import "./RuntimeLogs.css";

export function RuntimeLogs({ logs }: { logs: LogState }) {
  const [mode, setMode] = useState("parsed");
  const [search, setSearch] = useState("");
  const [level, setLevel] = useState("all");
  const [component, setComponent] = useState("all");
  const [frozen, setFrozen] = useState<LogEntry[] | null>(null);
  const [follow, setFollow] = useState(true);
  const viewport = useRef<HTMLDivElement>(null);
  const parsed = useMemo(
    () => (frozen ? frozen.map(parseEntry) : logs.parsed),
    [frozen, logs.parsed],
  );
  const filtered = useMemo(
    () => filterEntries(parsed, search, level, component),
    [parsed, search, level, component],
  );
  const components = [
    ...new Set([
      ...parsed.map((e) => e.tag),
      ...(component === "all" ? [] : [component]),
    ]),
  ].sort();
  useEffect(() => {
    if (follow && !frozen && viewport.current)
      viewport.current.scrollTop = viewport.current.scrollHeight;
  }, [filtered, follow, frozen, mode]);
  function download() {
    const url = URL.createObjectURL(
      new Blob([filtered.map((e) => e.line).join("\n") + "\n"], {
        type: "text/plain;charset=utf-8",
      }),
    );
    const a = document.createElement("a");
    a.href = url;
    a.download = `juiced-logs-${new Date().toISOString().replace(/[:.]/g, "-")}.log`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return (
    <section className="logs-workspace">
      <header className="page-heading">
        <div>
          <h1>Runtime logs</h1>
          <p>
            Inspect manager and backend output. Parsing happens in this browser.
          </p>
        </div>
        <span
          className={"log-connection " + (logs.status === "Live" ? "live" : "")}
        >
          {logs.status}
        </span>
      </header>
      {logs.error && (
        <p className="log-error" role="status">
          {logs.error}
        </p>
      )}
      <div className="log-controls">
        <div className="log-view-switch" role="group" aria-label="Log view">
          <Button
            size="compact"
            variant={mode === "parsed" ? "secondary" : "ghost"}
            aria-pressed={mode === "parsed"}
            onClick={() => setMode("parsed")}
          >
            Parsed
          </Button>
          <Button
            size="compact"
            variant={mode === "raw" ? "secondary" : "ghost"}
            aria-pressed={mode === "raw"}
            onClick={() => setMode("raw")}
          >
            Raw
          </Button>
        </div>
        <label className="search-input">
          <Search size={14} />
          <input
            aria-label="Search logs"
            placeholder="Search log output…"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
        </label>
        <select
          aria-label="Log severity"
          value={level}
          onChange={(e) => setLevel(e.target.value)}
        >
          <option value="all">All levels</option>
          <option value="issues">Warnings and errors</option>
          {[
            "trace",
            "debug",
            "info",
            "warning",
            "error",
            "fatal",
            "unknown",
          ].map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </select>
        <select
          aria-label="Log component"
          value={component}
          onChange={(e) => setComponent(e.target.value)}
        >
          <option value="all">All components</option>
          {components.map((c) => (
            <option key={c}>{c}</option>
          ))}
        </select>
      </div>
      <div className="log-actions">
        <span>
          {filtered.length.toLocaleString()} of {parsed.length.toLocaleString()}{" "}
          entries{frozen ? " · View paused; stream continues" : ""}
        </span>
        <div>
          <Button
            size="compact"
            variant="ghost"
            onClick={() => setFrozen(frozen ? null : logs.entries)}
          >
            {frozen ? <Play size={13} /> : <Pause size={13} />}
            {frozen ? "Resume" : "Pause view"}
          </Button>
          <Button
            size="compact"
            variant={follow ? "secondary" : "ghost"}
            aria-pressed={follow}
            onClick={() => setFollow(!follow)}
          >
            <ArrowDown size={13} />
            Follow
          </Button>
          <Button
            size="compact"
            variant="ghost"
            disabled={!filtered.length}
            onClick={download}
          >
            <Download size={13} />
            Export
          </Button>
          <Button
            size="compact"
            variant="ghost"
            onClick={() => {
              logs.clear();
              if (frozen) setFrozen([]);
            }}
          >
            <Trash2 size={13} />
            Clear view
          </Button>
        </div>
      </div>
      <div
        className="log-viewport"
        ref={viewport}
        tabIndex={0}
        aria-label="Log output"
        onScroll={() => {
          const e = viewport.current;
          if (e && e.scrollHeight - e.scrollTop - e.clientHeight > 40)
            setFollow(false);
        }}
      >
        {!filtered.length ? (
          <div className="logs-empty">
            <Terminal size={24} />
            <p>
              {parsed.length
                ? "No logs match these filters."
                : "Waiting for runtime output."}
            </p>
          </div>
        ) : mode === "raw" ? (
          <pre className="raw-log-output">
            {filtered.map((e) => e.line).join("\n")}
          </pre>
        ) : (
          <table className="parsed-log-table">
            <thead>
              <tr>
                <th>Server time</th>
                <th>Level</th>
                <th>Component</th>
                <th>Message</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map((e) => (
                <tr key={e.seq}>
                  <td title={e.timestamp}>
                    {e.timestamp.split(" ")[1] || e.timestamp || "Unknown"}
                  </td>
                  <td>
                    <span
                      className={"log-level " + e.level}
                      title={
                        e.level !== e.severity.toLowerCase()
                          ? `Stream severity: ${e.severity}; parsed backend severity: ${e.level}`
                          : undefined
                      }
                    >
                      {e.level}
                    </span>
                  </td>
                  <td>{e.tag || "Unknown"}</td>
                  <td>
                    <details>
                      <summary>{e.message || "Empty line"}</summary>
                      <pre>{e.line}</pre>
                      <small>
                        Sequence {e.seq} · {e.timestamp} · Stream severity{" "}
                        {e.severity}
                      </small>
                    </details>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
      <p className="log-retention">
        Latest {MAX_LOG_ENTRIES.toLocaleString()} entries, up to 2 Mi characters
        in this tab. The runtime retains up to 5,000 since restart. Reconnect
        refreshes retained history. Clear resets this tab’s buffer only. Export
        uses the current filters.
      </p>
    </section>
  );
}

export function RuntimeActivity({
  logs,
  openLogs,
}: {
  logs: LogState;
  openLogs: () => void;
}) {
  const issues = logs.parsed.filter((e) =>
    ["warning", "error", "fatal"].includes(e.level),
  );
  const events = logs.parsed
    .filter(
      (e) =>
        e.inference ||
        ["warning", "error", "fatal"].includes(e.level) ||
        (["Router", "Juiced"].includes(e.tag) &&
          /load|unload|recover|fail/i.test(e.message)),
    )
    .slice(-4)
    .reverse();
  return (
    <section className="runtime-activity">
      <header>
        <div>
          <h2>Recent activity</h2>
          <small>
            {logs.status} · {issues.length} warnings/errors in retained logs
          </small>
        </div>
        <Button size="compact" variant="ghost" onClick={openLogs}>
          View logs
        </Button>
      </header>
      {events.length ? (
        <ul>
          {events.map((e) => (
            <li key={e.seq}>
              <time title={e.timestamp}>
                {e.timestamp.split(" ")[1]?.slice(0, 8) || ""}
              </time>
              <span className={"log-level " + e.level}>
                {e.inference ? "inference" : e.level}
              </span>
              <span>
                {e.inference
                  ? `${e.inference.model} · ${e.inference.input} in / ${e.inference.output} out · ${e.inference.tps.toFixed(1)} tok/s · ${e.inference.ttft.toFixed(2)}s first token`
                  : e.message}
              </span>
            </li>
          ))}
        </ul>
      ) : (
        <p className="page-note">
          {logs.error ||
            "Waiting for model activity. Hardware and token metrics appear in the status bar."}
        </p>
      )}
    </section>
  );
}
