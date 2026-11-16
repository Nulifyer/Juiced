import { useEffect, useRef, useState } from "react";
import type { Client } from "./api";
import type { Model } from "./domain";
import { parsePulls } from "./localLibraryData";
import { notify } from "./toast";
import { Button, HelpTip } from "./ui";
import { useResource } from "./useResource";

const listedUpdate = (id: string) => (value: unknown) => {
  const data = value && typeof value === "object" ? (value as { data?: unknown }).data : undefined;
  return Array.isArray(data) && data.some((item) => item && typeof item === "object" && item.id === id && item.update_available === true);
};

/**
 * The Update action for a model with newer upstream files. The service downloads them, then
 * moves the model to them with its settings; the old files stay in Storage as unused.
 * `available` comes from the list's last check; without it the model list is read once.
 */
export function ModelUpdate({ client, model, online, available, compact, onDone }: { client: Client; model: Model; online: boolean; available?: boolean; compact?: boolean; onDone?: () => void }) {
  const [started, setStarted] = useState(false);
  const [busy, setBusy] = useState(false);
  const listed = useResource(client, "models", listedUpdate(model.id), 0, true, available === undefined);
  const pulls = useResource(client, "juiced/library/pulls", parsePulls, 2000, true, started || !compact);
  const pull = pulls.data?.find((item) => item.then?.kind === "update" && item.then.model === model.id);
  const state = pull?.then?.state;
  const finished = useRef<string | null>(null);
  useEffect(() => {
    if (!pull || state !== "done" || finished.current === pull.id) return;
    finished.current = pull.id;
    if (started) notify(`Updated ${model.id}. Its old files show as unused in Storage.`);
    onDone?.();
    if (available === undefined) void listed.reload();
  }, [pull, state, started, model.id, onDone, available, listed]);
  const offered = available ?? listed.data ?? false;
  if (state === "pending") {
    const total = pull!.files.reduce((sum, file) => sum + file.bytes, 0);
    const done = pull!.files.reduce((sum, file) => sum + Math.min(file.downloaded, file.bytes || file.downloaded), 0);
    const text = pull!.state === "complete" ? "Switching files…" : `Updating${total ? ` ${Math.round(done / total * 100)}%` : "…"}`;
    return compact ? <span className="update-note" role="status">{text}</span> : <p className="field-help" role="status">{text}</p>;
  }
  if (!offered && state !== "failed") return null;
  async function update() {
    setBusy(true);
    try {
      await client.request(`models/${encodeURIComponent(model.id)}/update`, { expected_revision: model.revision });
      setStarted(true);
      notify(`Updating ${model.id}. Follow it under Downloads.`);
      await pulls.reload();
    } catch (error) { notify(error instanceof Error ? error.message : "The update didn't start.", "error"); }
    finally { setBusy(false); }
  }
  if (compact) return <>{state === "failed" ? <span className="update-note">update failed</span> : <span className="update-note">update available</span>} <button className="link-button" aria-label={`Update ${model.id}`} disabled={!online || busy} onClick={(event) => { event.stopPropagation(); void update(); }}>Update</button></>;
  const button = <Button disabled={!online || busy} onClick={() => void update()}>Update</Button>;
  return <div className="model-update">
    <div className="action-row">{state === "failed" ? <span className="inline-warning">{pull?.then?.message ?? "The update didn't finish."} The model keeps its old files.</span> : <span className="update-note">Update available</span>}{button}<HelpTip label="Update">Downloads the new files and moves this model to them. Its settings stay.</HelpTip></div>
    <p className="field-help">{model.loaded ? "It reloads on the new files. " : ""}The old files stay on disk as unused.</p>
  </div>;
}
