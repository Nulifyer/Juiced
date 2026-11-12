import { useState } from "react";
import type { Client } from "./api";
import { downloadRequest, parseArtifactInventory } from "./management";
import { Button, Dialog, HelpTip } from "./ui";
import { useResource } from "./useResource";

export function UrlDownload({ client, onClose, onStarted }: { client: Client; onClose: () => void; onStarted: () => void }) {
  const inventory = useResource(client, "juiced/artifacts?section=jobs&limit=1", parseArtifactInventory, 0);
  const [url, setUrl] = useState("");
  const [sha, setSha] = useState("");
  const [expected, setExpected] = useState("");
  const [ceiling, setCeiling] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const data = inventory.data;
  async function start() {
    if (!data) return;
    setBusy(true); setError("");
    try {
      await client.request("juiced/artifacts/pull", { download: downloadRequest(url, sha, expected, ceiling), expected_generation: data.generation });
      onStarted();
    } catch (failure) { setError(failure instanceof Error ? failure.message : "The download didn't start."); }
    finally { setBusy(false); }
  }
  return <Dialog title="Download from a URL" onClose={onClose}>
    <form aria-label="Download weights" onSubmit={(event) => { event.preventDefault(); void start(); }}>
      <div className="section-heading"><div className="page-title"><h2>Direct file link</h2><HelpTip label="Download from a URL">Paste a direct file link and the publisher's SHA256. The file is kept only when its size and checksum match. Allowed sources: {data?.source_policy ?? "loading"}. Links can't contain credentials or query strings.</HelpTip></div></div>
      <div className="form-grid download-grid">
        <div className="form-wide"><label htmlFor="download-url">Download URL</label><input id="download-url" type="url" value={url} disabled={busy} onChange={(event) => setUrl(event.target.value)} placeholder="https://publisher.example/model.gguf" /></div>
        <div className="form-wide"><label htmlFor="download-sha">Publisher SHA256</label><input id="download-sha" value={sha} disabled={busy} onChange={(event) => setSha(event.target.value)} autoComplete="off" spellCheck={false} /></div>
        <div><label htmlFor="download-size">Expected size in bytes</label><input id="download-size" inputMode="numeric" value={expected} disabled={busy} onChange={(event) => setExpected(event.target.value)} placeholder="Optional" /></div>
        <div><label htmlFor="download-ceiling">Storage ceiling in bytes</label><input id="download-ceiling" inputMode="numeric" value={ceiling} disabled={busy} onChange={(event) => setCeiling(event.target.value)} title="Reserved before the download starts" /></div>
      </div>
      {(error || inventory.error) && <p role="alert" className="inline-error">{error || inventory.error}</p>}
      <div className="dialog-actions"><Button onClick={onClose}>Cancel</Button><Button variant="primary" type="submit" disabled={busy || !data || data.source_policy === "offline"}>Start download</Button></div>
    </form>
  </Dialog>;
}
