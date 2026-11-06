import {
  existingVariant,
  groupRepositories,
  modelQuant,
  quantization,
  variantName,
} from "./modelDiscovery";
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
import { Dialog } from "@base-ui/react/dialog";
import { Button } from "./t3/button";
import { CapabilityIcons, ServiceIcon, serviceNames } from "./ModelLibrary";
import "./Acquisition.css";
import { api, bytes, type Model } from "../api";
function transferModel(models: Model[], name: string) {
  return models.find((model) => model.id === name) ||
    (name?.startsWith("user.") ? models.find((model) => model.id === name.slice(5)) : undefined);
}
export function Acquisition({
  models,
  service,
  jobs,
  notify,
  refresh,
  tab,
  setTab,
  onInspect,
  initialRepository,
  onRepositoryOpened,
  onBackends,
}: {
  models: Model[];
  service: string;
  jobs: any[];
  notify: (s: string) => void;
  refresh: () => Promise<void>;
  tab: string;
  setTab: (s: string) => void;
  onInspect: (m: Model) => void;
  initialRepository?: { repo: string };
  onRepositoryOpened?: () => void;
  onBackends: () => void;
}) {
  const [query, setQuery] = useState(""),
    [inventoryQuery, setInventoryQuery] = useState(""),
    [inventoryService, setInventoryService] = useState("All services"),
    [catalogQuery, setCatalogQuery] = useState(""),
    [catalogPage, setCatalogPage] = useState(0),
    [searched, setSearched] = useState(false),
    [results, setResults] = useState<any[]>([]),
    [variants, setVariants] = useState<any>(),
    [variant, setVariant] = useState(""),
    [name, setName] = useState(""),
    [customName, setCustomName] = useState(false),
    [projector, setProjector] = useState(""),
    [busy, setBusy] = useState(false),
    [file, setFile] = useState<File>(),
    [recipe, setRecipe] = useState("llamacpp"),
    [purpose, setPurpose] = useState("chat"),
    [path, setPath] = useState(""),
    [uploadProgress, setUploadProgress] = useState(""),
    [importJobs, setImportJobs] = useState<any[]>([]);
  const uploading = useRef<AbortController | null>(null);
  const selectedVariant = variants?.variants?.find(
    (v: any) => v.name === variant,
  );
  const installedVariant =
    selectedVariant &&
    existingVariant(
      models,
      variants.checkpoint,
      selectedVariant,
      variants.recipe || "llamacpp",
      projector,
    );
  const nameConflict =
    !installedVariant &&
    models.some((m) => m.id === name || m.id === "user." + name);
  useEffect(() => setCatalogPage(0), [service]);
  useEffect(() => {
    if (initialRepository)
      void inspect(initialRepository.repo).finally(() =>
        onRepositoryOpened?.(),
      );
  }, [initialRepository]);
  useEffect(() => {
    if (tab === "import") void loadImports();
  }, [tab]);
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
      setSearched(true);
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
      setName(
        variantName(
          j.suggested_name || repo.split("/").pop(),
          j.variants?.[0]?.name || "GGUF",
        ),
      );
      setCustomName(false);
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
    if (installedVariant) {
      if (installedVariant.downloaded) {
        setVariants(undefined);
        onInspect(installedVariant);
      } else await pull(installedVariant.id);
      return;
    }
    const checkpoints: Record<string, string> = {
      main: variants.checkpoint + ":" + variant,
    };
    if (projector) checkpoints.mmproj = variants.checkpoint + ":" + projector;
    if (nameConflict) {
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
      notify("GGUF registered. It remains Disabled until you enable it.");
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
          <h1>
            {
              {
                discover: "Discover models",
                downloaded: "Downloaded weights",
                transfers: "Downloads",
                import: "Import a model",
              }[tab]
            }
          </h1>
          <p>
            {
              {
                discover:
                  "Find a model, review its files and choose a variant.",
                downloaded:
                  "Your local collection. Inspect files and capabilities without changing what is running.",
                transfers:
                  "Follow active transfers and pick up interrupted downloads.",
                import:
                  "Add a GGUF from your computer or register one already on the server.",
              }[tab]
            }
          </p>
        </div>
        {tab !== "import" && (
          <Button
            variant="outline"
            size="compact"
            onClick={() => setTab("import")}
          >
            <Upload size={14} />
            Import GGUF
          </Button>
        )}
      </header>
      {tab === "discover" && (
        <>
          <div
            className={
              "discovery-layout" + (results.length ? " has-results" : "")
            }
          >
            <div className="discovery-primary">
              <form
                className="discovery-search"
                aria-label="Search Hugging Face"
                onSubmit={(e) => {
                  e.preventDefault();
                  void search();
                }}
              >
                <img
                  className="source-mark"
                  src="/huggingface.svg"
                  alt="Hugging Face"
                  width="30"
                  height="30"
                />
                <label className="search-input">
                  <Search size={16} />
                  <input
                    aria-label="Search Hugging Face models"
                    placeholder="Search Hugging Face models…"
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                  />
                </label>
                <Button disabled={busy} type="submit">
                  {busy ? (
                    <LoaderCircle size={16} className="spin" />
                  ) : (
                    "Search"
                  )}
                </Button>
              </form>
              {results.length === 0 && !busy && (
                <div className="discovery-start">
                  <Search size={25} />
                  <h3>
                    {searched
                      ? "No matching repositories"
                      : "What would you like to run?"}
                  </h3>
                  <p>
                    Use a model name or repository, then review available files
                    before downloading.
                  </p>
                  <div className="search-suggestions">
                    {["Qwen", "Ornith", "Whisper"].map((term) => (
                      <button key={term} onClick={() => setQuery(term)}>
                        {term}
                      </button>
                    ))}
                  </div>
                </div>
              )}
              {results.length > 0 && (
                <div className="discovery-result-summary">
                  <span>{groupRepositories(results).length} models</span>
                  <span>
                    {results.length} repositories · grouped by publisher and
                    model
                  </span>
                </div>
              )}
              <div
                className="discovery-results"
                aria-label="Model search results"
              >
                {groupRepositories(results).map((group) => (
                  <article className="discovery-model" key={group.key}>
                    <header>
                      <div>
                        <h3>{group.name}</h3>
                        <p>
                          Published by <strong>{group.publisher}</strong>
                        </p>
                      </div>
                      <span className="discovery-downloads">
                        <Download size={13} />
                        {group.repositories
                          .reduce((sum, r) => sum + (r.downloads || 0), 0)
                          .toLocaleString()}{" "}
                        downloads
                      </span>
                    </header>
                    <div
                      className="discovery-quants"
                      aria-label={"Quantizations for " + group.name}
                    >
                      {group.repositories.map((r) => (
                        <Button
                          key={r.repository_id}
                          variant="outline"
                          size="compact"
                          disabled={busy}
                          title={r.repository_id}
                          onClick={() => void inspect(r.repository_id)}
                        >
                          {quantization(
                            r.repository_id.split("/").pop() || "",
                          ) || "Browse quants"}
                        </Button>
                      ))}
                    </div>
                    <details className="discovery-sources">
                      <summary>Sources</summary>
                      {group.repositories.map((r) => (
                        <a
                          key={r.repository_id}
                          href={"https://huggingface.co/" + r.repository_id}
                          target="_blank"
                          rel="noreferrer"
                        >
                          {r.repository_id}
                          <ExternalLink size={12} />
                        </a>
                      ))}
                    </details>
                  </article>
                ))}
              </div>
              <details className="catalog-browser">
                <summary>Browse the runtime catalog</summary>
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
                          {m.service} · {m.recipe} ·{" "}
                          {bytes((m.size || 0) * 1e9)}
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
                          (service === "All services" ||
                            m.service === service) &&
                          m.id
                            .toLowerCase()
                            .includes(catalogQuery.toLowerCase()),
                      ).length <=
                      (catalogPage + 1) * 30
                    }
                    onClick={() => setCatalogPage(catalogPage + 1)}
                  >
                    Next
                  </Button>
                </div>
              </details>
            </div>
            <aside className="discovery-guide">
              <section>
                <Download size={20} />
                <h3>Choose the right files</h3>
                <p>
                  Review quantization and size. Vision models may also need a
                  projector.
                </p>
                <p>
                  Downloads add weights to your library. They do not load a
                  model into memory.
                </p>
              </section>
              <section>
                <Upload size={20} />
                <h3>Already have a GGUF?</h3>
                <p>
                  Upload a file from your computer or register an existing
                  server file.
                </p>
                <Button
                  variant="outline"
                  size="compact"
                  onClick={() => setTab("import")}
                >
                  Import a model
                </Button>
              </section>
              {jobs.some((j) => j.running || j.status === "interrupted") && (
                <section>
                  <h3>Transfers in progress</h3>
                  <p>
                    {
                      jobs.filter(
                        (j) => j.running || j.status === "interrupted",
                      ).length
                    }{" "}
                    downloads need attention.
                  </p>
                  <Button
                    variant="ghost"
                    size="compact"
                    onClick={() => setTab("transfers")}
                  >
                    View downloads
                  </Button>
                </section>
              )}
            </aside>
          </div>
          {variants && (
            <Dialog.Root
              open
              onOpenChange={(open) => {
                if (!open) setVariants(undefined);
              }}
            >
              <Dialog.Portal>
                <Dialog.Backdrop className="dialog-backdrop" />
                <Dialog.Popup className="model-drawer variant-panel">
                  <div className="dialog-heading">
                    <Dialog.Title>{variants.suggested_name}</Dialog.Title>
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
                    Review the source, weight size and optional companion
                    projector. Downloads remain Disabled until enabled. Names
                    remain publisher names.
                  </p>
                  <p className="variant-source">
                    <a
                      href={"https://huggingface.co/" + variants.checkpoint}
                      target="_blank"
                      rel="noreferrer"
                    >
                      {variants.checkpoint} <ExternalLink size={12} />
                    </a>
                  </p>
                  <p className="variant-registration">
                    <strong>{installedVariant?.id || name}</strong>
                    <span>
                      {installedVariant?.downloaded
                        ? "Already in your library"
                        : "New variants stay Disabled until enabled"}
                    </span>
                  </p>
                  <fieldset className="quant-options">
                    <legend>Quantization</legend>
                    <div>
                      {variants.variants?.map((v: any) => {
                        const existing = existingVariant(
                          models,
                          variants.checkpoint,
                          v,
                          variants.recipe || "llamacpp",
                          projector,
                        );
                        return (
                          <label
                            key={v.name}
                            className={variant === v.name ? "is-selected" : ""}
                          >
                            <input
                              type="radio"
                              name="quantization"
                              value={v.name}
                              checked={variant === v.name}
                              onChange={() => {
                                setVariant(v.name);
                                if (!customName)
                                  setName(
                                    variantName(
                                      variants.suggested_name ||
                                        variants.checkpoint.split("/").pop(),
                                      v.name,
                                    ),
                                  );
                              }}
                            />
                            <strong>{v.name}</strong>
                            <span>{bytes(v.size_bytes || 0)}</span>
                            <small>
                              {existing?.downloaded
                                ? "Downloaded"
                                : existing
                                  ? "Registered"
                                  : `${v.files?.length || 1} file(s)`}
                            </small>
                          </label>
                        );
                      })}
                    </div>
                  </fieldset>
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
                  <details>
                    <summary>Registration name</summary>
                    <label>
                      Model name
                      <input
                        value={name}
                        onChange={(e) => {
                          setCustomName(true);
                          setName(e.target.value);
                        }}
                      />
                    </label>
                    <p className="page-note">
                      The publisher name and quant identify each variant
                      independently. Changing this name does not rename another
                      model.
                    </p>
                  </details>
                  {nameConflict && (
                    <p className="inline-error" role="alert">
                      This name belongs to a different checkpoint. Choose a
                      distinct registration name above.
                    </p>
                  )}
                  <Button
                    onClick={() => void download()}
                    disabled={!variant || !name || nameConflict}
                  >
                    <Download size={15} />{" "}
                    {installedVariant?.downloaded
                      ? "Open installed variant"
                      : `Download ${variant}`}
                  </Button>
                </Dialog.Popup>
              </Dialog.Portal>
            </Dialog.Root>
          )}
        </>
      )}
      {tab === "downloaded" && (
        <div className="weights-inventory">
          <div className="workspace-toolbar">
            <label className="search-input">
              <Search size={15} />
              <input
                aria-label="Search downloaded weights"
                placeholder="Search your local collection…"
                value={inventoryQuery}
                onChange={(e) => setInventoryQuery(e.target.value)}
              />
            </label>
            <select
              aria-label="Filter downloaded services"
              value={inventoryService}
              onChange={(e) => setInventoryService(e.target.value)}
            >
              <option>All services</option>
              {[
                ...new Set(
                  models.filter((m) => m.downloaded).map((m) => m.service),
                ),
              ].map((v) => (
                <option key={v}>{v}</option>
              ))}
            </select>
          </div>
          <div className="weights-table-heading">
            <span>Model</span>
            <span>Quant / service</span>
            <span>Download size</span>
            <span>Backend</span>
          </div>
          {models
            .filter(
              (m) =>
                m.downloaded &&
                (inventoryService === "All services" ||
                  m.service === inventoryService) &&
                [m.id, m.recipe]
                  .join(" ")
                  .toLowerCase()
                  .includes(inventoryQuery.toLowerCase()),
            )
            .map((m) => (
              <button
                className="weight-row"
                key={m.id}
                onClick={() => onInspect(m)}
              >
                <span className="weight-identity">
                  <span className="weight-icon">
                    <ServiceIcon service={m.service} />
                  </span>
                  <span>
                    <strong>{m.id}</strong>
                    <small>
                      <CapabilityIcons model={m} />
                      {m.available === false
                        ? "Backend unavailable"
                        : "Downloaded"}
                    </small>
                  </span>
                </span>
                <span className="weight-service">
                  {modelQuant(m) && (
                    <strong className="quant-badge">{modelQuant(m)}</strong>
                  )}
                  {serviceNames[m.service] || m.service}
                </span>
                <span>{m.size ? bytes(m.size * 1e9) : "Not reported"}</span>
                <span>
                  {m.recipe}
                  <ExternalLink size={12} />
                </span>
              </button>
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
            [...jobs]
              .sort(
                (a, b) =>
                  Number(Boolean(b.running)) - Number(Boolean(a.running)),
              )
              .map((j) => (
                <article className="transfer-row" key={j.id}>
                  <div className="row-spread">
                    <strong>{transferModel(models, j.model_name)?.id || j.model_name}</strong>
                    <span className="badge">{j.running && j.percent >= 100 ? "Finishing…" : j.status}</span>
                  </div>
                  {!j.complete && (
                    <progress
                      aria-label={"Download progress for " + j.model_name}
                      max={100}
                      value={j.percent || 0}
                    />
                  )}
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
                      {j.complete && (j.type === "backend" || j.id?.startsWith("backend:")) ? (
                        <Button size="compact" variant="outline" onClick={onBackends}>View backend</Button>
                      ) : j.complete && transferModel(models, j.model_name) ? (
                        <Button size="compact" variant="outline" onClick={() => onInspect(transferModel(models, j.model_name)!)}>Review model</Button>
                      ) : null}
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
            Completed models remain Disabled until you enable them. Loading is a
            separate operation.
          </p>
        </>
      )}
      {tab === "import" && (
        <>
          <div className="import-grid">
            <label
              className="upload-zone"
              onDragOver={(e) => e.preventDefault()}
              onDrop={(e) => {
                e.preventDefault();
                const f = e.dataTransfer.files[0];
                if (f) {
                  setFile(f);
                  setName(f.name.replace(/\.gguf$/i, ""));
                }
              }}
            >
              <Upload size={22} />
              <strong>{file?.name || "Choose a GGUF file"}</strong>
              <small>Drag a file here, or browse your computer.</small>
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
          <details className="server-import settings-section">
            <summary>Register a file already on the server</summary>
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
          </details>
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
