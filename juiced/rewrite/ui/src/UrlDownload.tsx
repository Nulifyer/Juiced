import { useState } from "react";
import type { Client } from "./api";
import { downloadRequest, parseArtifactInventory } from "./management";
import { Button, Dialog, HelpTip } from "./ui";
import { useResource } from "./useResource";

export function UrlDownload({ client, onClose, onStarted }: { client: Client; onClose: () => void; onStarted: () => void }) {
  const inventory = useResource(client, "juiced/artifacts?section=jobs&limit=1", parseArtifactInventory, 0);
  const [url, setUrl] = useState("");
  const [sha, setSha] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const data = inventory.data;
  async function start() {
    if (!data) return;
    setBusy(true); setError("");
    try {
      await client.request("juiced/artifacts/pull", { download: downloadRequest(url, sha), expected_generation: data.generation });
      onStarted();
    } catch (failure) { setError(failure instanceof Error ? failure.message : "The download didn't start."); }
    finally { setBusy(false); }
  }
  return <Dialog title="Download from a URL" onClose={onClose}>
    <form aria-label="Download weights" onSubmit={(event) => { event.preventDefault(); void start(); }}>
      <div className="form-grid download-grid">
        <div className="form-wide"><div className="field-head"><label htmlFor="download-url">Download URL</label><HelpTip label="Download from a URL">A direct link to one model file. Juiced reads its size from the server. Allowed sources: {data?.source_policy ?? "loading"}.</HelpTip></div><input id="download-url" type="url" required value={url} disabled={busy} onChange={(event) => setUrl(event.target.value)} placeholder="https://publisher.example/model.gguf" /></div>
        <div className="form-wide"><label htmlFor="download-sha">SHA256 <span className="muted">optional</span></label><input id="download-sha" value={sha} disabled={busy} onChange={(event) => setSha(event.target.value)} autoComplete="off" spellCheck={false} placeholder="Leave empty to skip the check" /><p className="field-help">Without it the file shows as unverified.</p></div>
      </div>
      {(error || inventory.error) && <p role="alert" className="inline-error">{error || inventory.error}</p>}
      <div className="dialog-actions"><Button onClick={onClose}>Cancel</Button><Button variant="primary" type="submit" disabled={busy || !data || data.source_policy === "offline" || !url.trim()}>Start download</Button></div>
    </form>
  </Dialog>;
}
