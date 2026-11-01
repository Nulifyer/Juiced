import { useEffect, useRef, useState } from "react";
import {
  Search,
  Upload,
  Pause,
  Play,
  X,
  Download,
  ExternalLink,
  LoaderCircle,
} from "lucide-react";
import { Button } from "./t3/button";
import { api, bytes, type Model } from "../api";
export function Acquisition({
  models,
  service,
  jobs,
  notify,
  refresh,
  tab,
  setTab,
}: {
  models: Model[];
  service: string;
  jobs: any[];
  notify: (s: string) => void;
  refresh: () => Promise<void>;
  tab: string;
  setTab: (s: string) => void;
}) {
  const [query, setQuery] = useState(""),
    [catalogQuery, setCatalogQuery] = useState(""),
    [catalogPage, setCatalogPage] = useState(0),
    [results, setResults] = useState<any[]>([]),
    [variants, setVariants] = useState<any>(),
    [variant, setVariant] = useState(""),
    [name, setName] = useState(""),
    [projector, setProjector] = useState(""),
    [busy, setBusy] = useState(false),
    [file, setFile] = useState<File>(),
    [recipe, setRecipe] = useState("llamacpp"),
    [purpose, setPurpose] = useState("chat"),
    [path, setPath] = useState(""),
    [uploadProgress, setUploadProgress] = useState(""),
    [importJobs, setImportJobs] = useState<any[]>([]);
  const uploading = useRef<AbortController | null>(null);
  useEffect(() => setCatalogPage(0), [service]);
  async function search() {
    if (query.trim().length < 3) {
      notify("Enter at least three characters.");
      return;
    }
    setBusy(true);
    try {
      const j = await api(
        "registry/search?source=huggingface&query=" +
          encodeURIComponent(query) +
          "&format=gguf",
      );
      setResults(j.results || []);
    } catch (e) {
      notify((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  async function inspect(repo: string) {
    setBusy(true);
    try {
      const j = await api(
        "pull/variants?checkpoint=" + encodeURIComponent(repo),
      );
      setVariants(j);
      setVariant(j.variants?.[0]?.name || "");
      setName(j.suggested_name || repo.split("/").pop());
      setProjector(j.mmproj_files?.[0] || "");
    } catch (e) {
      notify((e as Error).message);
    } finally {
      setBusy(false);
    }
  }
  async function pull(model: string, extra: Record<string, unknown> = {}) {
    try {
      await api("pull", {
        model_name: model,
        stream: true,
        subscribe: false,
        ...extra,
      });
      notify("Download started. You can leave this screen.");
      setTab("transfers");
      setVariants(undefined);
      await refresh();
    } catch (e) {
      notify((e as Error).message);
    }
  }
  async function download() {
    if (!variants || !variant) return;
    const checkpoints: Record<string, string> = {
      main: variants.checkpoint + ":" + variant,
    };
    if (projector) checkpoints.mmproj = variants.checkpoint + ":" + projector;
    if (models.some((m) => m.id === name)) {
      notify(
        "This name already exists. Include the publisher quantization in the variant name before downloading.",
      );
      return;
    }
    await pull("user." + name, {
      recipe: variants.recipe || "llamacpp",
      checkpoints,
      labels: [
        ...new Set([
          ...(variants.suggested_labels || []),
          ...(projector ? ["vision"] : []),
        ]),
      ],
    });
  }
  async function control(j: any, action: string) {
    try {
      if (action === "resume") await pull(j.model_name);
      else {
        await api("downloads/control", { id: j.id, action });
        await refresh();
      }
    } catch (e) {
      notify((e as Error).message);
    }
  }
  async function upload(resume?: any) {
    if (!file) {
      notify("Select the GGUF file.");
      return;
    }
    setBusy(true);
    uploading.current = new AbortController();
    try {
      let job =
        resume ||
        (await api("juiced/import/start", {
          filename: file.name,
          size: file.size,
        }));
      let offset = job.received || 0;
      while (offset < file.size) {
        if (uploading.current.signal.aborted)
          throw new DOMException("Upload paused", "AbortError");
        const chunk = file.slice(offset, offset + 8 * 1024 * 1024);
        const r = await fetch(
          "/v1/juiced/import/chunk?id=" +
            encodeURIComponent(job.id) +
            "&offset=" +
            offset,
          { method: "POST", body: chunk, signal: uploading.current.signal },
        );
        const j = await r.json();
        if (!r.ok) {
          if (r.status === 409 && j.offset !== undefined) {
            offset = j.offset;
            continue;
          }
          throw Error(j.error || "Upload failed");
        }
        offset = j.received;
        setUploadProgress(bytes(offset) + " / " + bytes(file.size));
      }
      await api("juiced/import/finish", {
        id: job.id,
        name: name || file.name.replace(/\.gguf$/i, ""),
        recipe,
        purpose,
      });
      notify("GGUF registered. It remains On demand and unloaded.");
      await refresh();
      await loadImports();
    } catch (e) {
      notify(
        (e as Error).name === "AbortError"
          ? "Upload paused. Select the same file and resume from its received offset."
          : (e as Error).message,
      );
      await loadImports();
    } finally {
      setBusy(false);
      uploading.current = null;
    }
  }
  async function loadImports() {
    try {
      const j = await api("juiced/imports");
      setImportJobs(Object.values(j.data || {}));
    } catch (e) {
      notify((e as Error).message);
    }
  }
  async function serverImport() {
    try {
      await api("juiced/import/server", { path, name, recipe, purpose });
      notify("Server GGUF registered without copying the original.");
      await refresh();
    } catch (e) {
      notify((e as Error).message);
    }
  }
  return (
    <>
      <header className="page-heading">
        <div>
          <h1>Library</h1>
          <p>Find weights, review variants and track acquisition.</p>
        </div>
      </header>
      <div className="tabs" role="tablist">
        {["discover", "downloaded", "transfers", "import"].map((t) => (
          <button
            key={t}
            role="tab"
            aria-selected={tab === t}
            onClick={() => {
              setTab(t);
              if (t === "import") void loadImports();
            }}
          >
            {t === "import" ? "Import GGUF" : t[0].toUpperCase() + t.slice(1)}
          </button>
        ))}
      </div>
      {tab === "discover" && (
        <>
          <details className="settings-section">
            <summary>Browse runtime catalog</summary>
            <p className="page-note">
              Ready-to-download models across all supported services.
            </p>
            <input
              aria-label="Filter runtime catalog"
              placeholder="Filter catalog models…"
              value={catalogQuery}
              onChange={(e) => {
                setCatalogQuery(e.target.value);
                setCatalogPage(0);
              }}
            />
            {models
              .filter(
                (m) =>
                  !m.downloaded &&
                  m.available !== false &&
                  (service === "All services" || m.service === service) &&
                  m.id.toLowerCase().includes(catalogQuery.toLowerCase()),
              )
              .slice(catalogPage * 30, (catalogPage + 1) * 30)
              .map((m) => (
                <article className="simple-row" key={m.id}>
                  <div>
                    <strong>{m.id}</strong>
                    <p>
                      {m.service} · {m.recipe} · {bytes((m.size || 0) * 1e9)}
                    </p>
                  </div>
                  <Button
                    size="compact"
                    variant="outline"
                    onClick={() => void pull(m.id)}
                  >
                    <Download size={14} /> Download
                  </Button>
                </article>
              ))}
            <div className="row-actions">
              <Button
                size="compact"
                variant="ghost"
                disabled={catalogPage === 0}
                onClick={() => setCatalogPage(catalogPage - 1)}
              >
                Previous
              </Button>
              <span>Page {catalogPage + 1}</span>
              <Button
                size="compact"
                variant="ghost"
                disabled={
                  models.filter(
                    (m) =>
                      !m.downloaded &&
                      m.available !== false &&
                      (service === "All services" || m.service === service) &&
                      m.id.toLowerCase().includes(catalogQuery.toLowerCase()),
                  ).length <=
                  (catalogPage + 1) * 30
                }
                onClick={() => setCatalogPage(catalogPage + 1)}
              >
                Next
              </Button>
            </div>
          </details>
          <h2>Hugging Face search</h2>
          <form
            className="search-row"
            onSubmit={(e) => {
              e.preventDefault();
              void search();
            }}
          >
            <label className="search-input">
              <Search size={16} />
              <input
                placeholder="Search Hugging Face…"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            <Button disabled={busy} type="submit">
              {busy ? <LoaderCircle size={16} className="spin" /> : "Search"}
            </Button>
          </form>
          <p className="page-note">
            Search checks repository metadata. Inspect files before choosing a
            compatible backend.
          </p>
          {results.map((r) => (
            <article className="search-result" key={r.repository_id}>
              <div>
                <strong>{r.display_name || r.repository_id}</strong>
                <p>
                  {r.repository_id} · {r.downloads?.toLocaleString() || 0}{" "}
                  downloads
                </p>
                <p>{r.description?.slice(0, 220)}</p>
              </div>
              <div className="row-actions">
                <a
                  href={"https://huggingface.co/" + r.repository_id}
                  target="_blank"
                  rel="noreferrer"
                  title="Open model card"
                >
                  <ExternalLink size={15} />
                </a>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy}
                  onClick={() => void inspect(r.repository_id)}
                >
                  Review files
                </Button>
              </div>
            </article>
          ))}
          {variants && (
            <div className="variant-panel">
              <div className="dialog-heading">
                <h2>{variants.suggested_name}</h2>
                <Button
                  size="icon-xs"
                  variant="ghost"
                  aria-label="Close file review"
                  onClick={() => setVariants(undefined)}
                >
                  <X size={16} />
                </Button>
              </div>
              <p className="page-note">
                Choose the downloaded variant and companion projector. Names
                remain publisher names.
              </p>
              <label>
                Model name
                <input value={name} onChange={(e) => setName(e.target.value)} />
              </label>
              <label>
                Quantization
                <select
                  value={variant}
                  onChange={(e) => setVariant(e.target.value)}
                >
                  {variants.variants?.map((v: any) => (
                    <option value={v.name} key={v.name}>
                      {v.name} · {bytes(v.size_bytes || 0)} ·{" "}
                      {v.files?.length || 1} file(s)
                    </option>
                  ))}
                </select>
              </label>
              {variants.mmproj_files?.length > 0 && (
                <label>
                  Vision projector
                  <select
                    value={projector}
                    onChange={(e) => setProjector(e.target.value)}
                  >
                    <option value="">Text only</option>
                    {variants.mmproj_files.map((f: string) => (
                      <option key={f}>{f}</option>
                    ))}
                  </select>
                </label>
              )}
              <p className="page-note">
                Backend: {variants.recipe}. Requires architecture support;
                repository tags are not conformance tests.
              </p>
              <Button
                onClick={() => void download()}
                disabled={!variant || !name}
              >
                <Download size={15} /> Download selected variant
              </Button>
            </div>
          )}
        </>
      )}
      {tab === "downloaded" && (
        <div className="library-list">
          {models
            .filter(
              (m) =>
                m.downloaded &&
                (service === "All services" || m.service === service),
            )
            .map((m) => (
              <article className="simple-row" key={m.id}>
                <div>
                  <strong>{m.id}</strong>
                  <p>
                    {m.recipe} · {m.service} · {bytes((m.size || 0) * 1e9)}
                    {m.unavailable_reason ? " · " + m.unavailable_reason : ""}
                  </p>
                </div>
                <span className="badge">
                  {m.available === false
                    ? "Backend unavailable"
                    : m.loaded
                      ? "Loaded"
                      : "Unloaded"}
                </span>
              </article>
            ))}
        </div>
      )}
      {tab === "transfers" && (
        <>
          {jobs.length === 0 ? (
            <div className="empty-state">
              No transfers. Search for a model or import a GGUF.
            </div>
          ) : (
            jobs.map((j) => (
              <article className="transfer-row" key={j.id}>
                <div className="row-spread">
                  <strong>{j.model_name}</strong>
                  <span className="badge">{j.status}</span>
                </div>
                <progress max={100} value={j.percent || 0} />
                <div className="row-spread">
                  <small>
                    {j.file_index || 0} / {j.total_files || 0} files ·{" "}
                    {bytes(
                      j.overall_bytes_downloaded || j.bytes_downloaded || 0,
                    )}
                    {j.total_download_size
                      ? " / " + bytes(j.total_download_size)
                      : ""}
                  </small>
                  <div className="row-actions">
                    {j.status === "complete" && (
                      <Button
                        size="compact"
                        variant="outline"
                        onClick={async () => {
                          try {
                            await api("juiced/operation", {
                              model: j.model_name,
                              action: "load",
                            });
                            notify("Model queued for loading.");
                            await refresh();
                          } catch (e) {
                            notify((e as Error).message);
                          }
                        }}
                      >
                        <Play size={13} />
                        Load
                      </Button>
                    )}
                    {j.running ? (
                      <>
                        <Button
                          size="compact"
                          variant="ghost"
                          onClick={() => void control(j, "pause")}
                        >
                          <Pause size={13} />
                          Pause
                        </Button>
                        <Button
                          size="compact"
                          variant="ghost"
                          onClick={() => void control(j, "cancel")}
                        >
                          <X size={13} />
                          Cancel
                        </Button>
                      </>
                    ) : [
                        "paused",
                        "error",
                        "interrupted",
                        "cancelled",
                      ].includes(j.status) ? (
                      <Button
                        size="compact"
                        variant="outline"
                        onClick={() => void control(j, "resume")}
                      >
                        <Play size={13} />
                        Resume / retry
                      </Button>
                    ) : null}
                  </div>
                </div>
                {j.error && <p className="inline-error">{j.error}</p>}
              </article>
            ))
          )}
          <p className="page-note">
            Completed models remain On demand and unloaded. Loading is a
            separate operation.
          </p>
        </>
      )}
      {tab === "import" && (
        <>
          <div className="import-grid">
            <label className="upload-zone">
              <Upload size={22} />
              <strong>{file?.name || "Choose a GGUF file"}</strong>
              <small>Chunked upload with restart-safe received offsets</small>
              <input
                type="file"
                accept=".gguf"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  setFile(f);
                  if (f) setName(f.name.replace(/\.gguf$/i, ""));
                }}
              />
            </label>
            <label>
              Proper model name
              <input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="Publisher model name"
              />
            </label>
            <label>
              Backend
              <select
                value={recipe}
                onChange={(e) => setRecipe(e.target.value)}
              >
                <option value="llamacpp">llama.cpp</option>
                <option value="asr4all">FUTO transcribe.cpp</option>
                <option value="parakeet">Parakeet Redux</option>
                <option value="whispercpp">Whisper.cpp</option>
              </select>
            </label>
            {recipe === "llamacpp" && (
              <label>
                Model service
                <select
                  value={purpose}
                  onChange={(e) => setPurpose(e.target.value)}
                >
                  <option value="chat">Chat</option>
                  <option value="embedding">Embeddings</option>
                  <option value="reranking">Rerank</option>
                </select>
              </label>
            )}
            <div className="row-actions">
              <Button
                disabled={busy || !file || !name}
                onClick={() => void upload()}
              >
                <Upload size={15} />
                Upload and register
              </Button>
              {busy && (
                <Button
                  variant="outline"
                  onClick={() => uploading.current?.abort()}
                >
                  Pause
                </Button>
              )}
              <span className="page-note">{uploadProgress}</span>
            </div>
            <p className="page-note">
              Single-file GGUF import in this release. Use the managed Hugging
              Face workflow for split weights and vision companions.
            </p>
          </div>
          <section className="settings-section">
            <h2>Files already on Mage</h2>
            <p className="page-note">
              Import a regular GGUF from the read-only /library mount without
              sending it over the VPN.
            </p>
            <label>
              Server path
              <input
                value={path}
                onChange={(e) => setPath(e.target.value)}
                placeholder="/library/huggingface/hub/…/model.gguf"
              />
            </label>
            <Button
              variant="outline"
              disabled={!path || !name}
              onClick={() => void serverImport()}
            >
              Inspect and register server file
            </Button>
          </section>
          {importJobs.map((j) => (
            <article className="simple-row" key={j.id}>
              <div>
                <strong>{j.filename}</strong>
                <p>
                  {j.status} · {bytes(j.received || 0)} / {bytes(j.size || 0)}
                </p>
              </div>
              {j.status === "uploading" && (
                <Button
                  size="compact"
                  variant="outline"
                  disabled={!file || file.name !== j.filename || busy}
                  onClick={() => void upload(j)}
                >
                  Resume with selected file
                </Button>
              )}
            </article>
          ))}
        </>
      )}
    </>
  );
}
