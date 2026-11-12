import { useCallback, useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import { notify } from "./toast";

export function useResource<T>(client: Client, path: string, parse: (value: unknown) => T, period = 2000, retain = false, enabled = true) {
  const [data, setData] = useState<{ client: Client; path: string; value: T } | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const request = useRef<AbortController | null>(null);
  const failures = useRef(0); const nextPoll = useRef(0);
  const reload = useCallback(async () => {
    request.current?.abort();
    const controller = new AbortController(); request.current = controller;
    setLoading(true);
    try {
      const result = parse(await client.request(path, undefined, controller.signal));
      if (!controller.signal.aborted) { setData({ client, path, value: result }); setError(""); failures.current = 0; nextPoll.current = 0; return result; }
    } catch (error) {
      if (!controller.signal.aborted) { setError(error instanceof Error ? error.message : "The service state is unavailable. Retry the refresh."); failures.current = Math.min(5, failures.current + 1); nextPoll.current = performance.now() + Math.min(30000, period * 2 ** failures.current); }
    } finally { if (!controller.signal.aborted) { setLoading(false); request.current = null; } }
  }, [client, path, parse, period]);
  useEffect(() => {
    if (!retain) setData(null);
    setError(""); failures.current = 0; nextPoll.current = 0;
    if (!enabled) return;
    void reload();
    const timer = period ? setInterval(() => { if (!request.current && performance.now() >= nextPoll.current) void reload(); }, period) : undefined;
    return () => { clearInterval(timer); request.current?.abort(); };
  }, [reload, period, retain, enabled]);
  return { data: data?.client === client && (retain || data.path === path) ? data.value : null, current: data?.client === client && data.path === path, error, loading, reload };
}

export function useOperation(client: Client, refresh: () => Promise<unknown>) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const request = useRef<AbortController | null>(null);
  useEffect(() => () => { request.current?.abort(); }, [client]);
  async function run<T>(path: string, body: unknown, success: string, session?: string): Promise<T | undefined> {
    if (request.current) return;
    const controller = new AbortController(); request.current = controller;
    setBusy(true); setError(""); setNotice("");
    try {
      const result = await client.request<T>(path, body, controller.signal, session);
      if (!controller.signal.aborted) { await refresh(); if (!controller.signal.aborted) { setNotice(success); notify(success); } return result; }
    } catch (error) {
      if (!controller.signal.aborted) { setError(error instanceof Error ? error.message : "The operation failed. Refresh and retry."); await refresh(); }
    } finally { if (!controller.signal.aborted) setBusy(false); if (request.current === controller) request.current = null; }
  }
  return { busy, error, notice, run, fail: (message: string) => { setError(message); setNotice(""); } };
}
