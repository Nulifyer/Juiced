import {
  existingVariant,
  quantization,
  repositoryUrl,
  type PublishedSource,
  variantName,
} from "./modelDiscovery";
import { DiscoverModels } from "./DiscoverModels";
import { runtimeName } from "./runtimeInfo";
import { HelpTip } from "./HelpTip";
import { useEffect, useRef, useState } from "react";
import {
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
import "./Acquisition.css";
import { api, bytes, type Model } from "../api";
function transferModel(models: Model[], name: string) {
  return models.find((model) => model.id === name) ||
    (name?.startsWith("user.") ? models.find((model) => model.id === name.slice(5)) : undefined);
}
export function Acquisition({
  models,
  jobs,
  notify,
  refresh,
  tab,
  setTab,
  onInspect,
  initialRepository,
  onRepositoryOpened,
  onBackends,
  onTry,
  activityRequest,
}: {
  models: Model[];
  jobs: any[];
  notify: (s: string) => void;
  refresh: () => Promise<void>;
  tab: string;
  setTab: (s: string) => void;
  onInspect: (m: Model) => void;
  initialRepository?: { repo?: string; query?: string; runtime?: string; source?: PublishedSource };
  onRepositoryOpened?: () => void;
  onBackends: () => void;
  onTry: (model: Model) => void;
  activityRequest: number;
}) {
  const [variants, setVariants] = useState<any>(),
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
  const [importNames, setImportNames] = useState<Record<string, string>>({});
  const [reviewedModel, setReviewedModel] = useState<Model>();
  const [reviewError, setReviewError] = useState("");
  const [reviewingRepo, setReviewingRepo] = useState("");
  const [activityOpen, setActivityOpen] = useState(false);
  const reviewController = useRef<AbortController | null>(null);
  const reviewSource = useRef<{ repo: string; source: PublishedSource } | undefined>(undefined);
  const reviewTrigger = useRef<HTMLElement | null>(null);
  const activity = useRef<HTMLDetailsElement>(null);
  const uploading = useRef<AbortController | null>(null);
  const activeJobKeys = jobs.filter((j) => !j.complete && j.status !== "cancelled").map((j) => j.id).join("|");
  useEffect(() => () => reviewController.current?.abort(), []);
  useEffect(() => { if (activeJobKeys) setActivityOpen(true); }, [activeJobKeys]);
  useEffect(() => {
    if (!activityRequest) return;
    setActivityOpen(true);
    activity.current?.scrollIntoView({ block: "start" });
  }, [activityRequest]);
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
      variants.source,
    );
  const nameConflict =
    !installedVariant &&
    models.some((m) => m.id === name || m.id === "user." + name);
  useEffect(() => {
    let current = true;
    if (initialRepository?.repo)
      void inspect(initialRepository.repo, initialRepository.source).finally(() => {
        if (current) onRepositoryOpened?.();
      });
    return () => { current = false; };
  }, [initialRepository]);
  useEffect(() => {
    if (tab === "import") void loadImports();
  }, [tab]);
  function returnReviewFocus() {
    return reviewTrigger.current?.isConnected ? reviewTrigger.current : document.querySelector<HTMLInputElement>('.model-discovery input[aria-label="Search models"]');
  }
  async function inspect(repo: string, source: PublishedSource = "huggingface") {
    reviewController.current?.abort();
    const controller = new AbortController();
    reviewController.current = controller;
    reviewTrigger.current = document.activeElement as HTMLElement;
    reviewSource.current = { repo, source };
    setBusy(true); setReviewError(""); setReviewingRepo(repo); setVariants(undefined); setReviewedModel(undefined);
    try {
      const j = await api("pull/variants?checkpoint=" + encodeURIComponent(repo) + "&source=" + source, undefined, controller.signal);
      if (controller.signal.aborted) return;
      if (j.source && j.source !== source) throw Error("The registry changed during file review. Review this repository again.");
      if (!Array.isArray(j.variants) || !j.variants.length) throw Error("No downloadable variants supported by this file picker were reported. Review the publisher files or choose another repository.");
      setVariants({ ...j, source: j.source || source }); setVariant(""); setName(j.suggested_name || repo.split("/").pop());
      setCustomName(false); setProjector("");
    } catch (e) {
      if (!controller.signal.aborted) setReviewError("Could not review " + repo + ": " + (e as Error).message);
    } finally {
      if (!controller.signal.aborted) { setBusy(false); setReviewingRepo(""); }
    }
  }
  function reviewDownload(id: string) {
    const model = models.find((m) => m.id === id);
    if (!model) return;
    reviewTrigger.current = document.activeElement as HTMLElement;
    setReviewError(""); setVariants(undefined); setReviewedModel(model);
  }
  async function pull(model: string, extra: Record<string, unknown> = {}) {
    setBusy(true); setReviewError("");
    try {
      await api("pull", {
        model_name: model,
        stream: true,
        subscribe: false,
        ...extra,
      });
      notify("Download requested. Progress and recovery stay in this library.");
      setActivityOpen(true);
      setVariants(undefined); setReviewedModel(undefined);
      await refresh();
    } catch (e) {
      setReviewError((e as Error).message);
    } finally { setBusy(false); }
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
      source: variants.source,
      checkpoints,
      labels: [
        ...new Set([
          ...(variants.suggested_labels || []).filter((label: string) => !(variants.mmproj_files?.length && !projector && label === "vision")),
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
  async function recoverImport(job: any) {
    setBusy(true);
    try {
      const body = job.publication?.conflict
        ? { id: job.id, name: importNames[job.id]?.trim() }
        : { id: job.id };
      await api("juiced/import/" + (job.status === "publishing" ? "finish" : "cancel"), body);
      notify(job.status === "publishing" ? "Import registered. Its loading policy is unchanged." : "Upload cleanup retried.");
      await refresh();
    } catch (e) {
      notify((e as Error).message);
    } finally {
      await loadImports();
      setBusy(false);
    }
  }
  return (
    <>
      <header className="page-heading">
        <div className="heading-with-help">
          <h1>
            {tab === "import" ? "Import a model" : "Model library"}
          </h1>
          <HelpTip label="Model library">Search Hugging Face repositories, choose a quant and review companion files before downloading. New models stay Disabled until you enable them. Upload local files through Import GGUF.</HelpTip>
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
        {tab !== "import" && (jobs.length > 0 || activityRequest > 0) && (
        <details ref={activity} className="library-activity" open={activityOpen} onToggle={(event) => setActivityOpen(event.currentTarget.open)}>
          <summary>{jobs.some((j) => j.running) ? "Downloads in progress" : jobs.some((j) => !j.complete && j.status !== "cancelled") ? "Downloads need attention" : "Recent downloads"} · {jobs.length}</summary>
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
                    <span className="badge">{j.running && j.status === "paused" ? "Pausing…" : j.running && j.status === "cancelled" ? "Cancelling…" : j.running && j.retry ? "Retrying" : j.running && j.percent >= 100 ? "Finishing…" : j.status}</span>
                  </div>
                  {j.running && j.retry ? <p className="page-note" role="status">Retry {j.retry.attempt} of {j.retry.max_retries} · waiting up to {Math.ceil(j.retry.delay_ms / 1000)} s{j.retry.http_code ? ` after HTTP ${j.retry.http_code}` : " after a connection error"}. You can pause or cancel.</p> : j.running && !(j.overall_bytes_downloaded || j.bytes_downloaded || 0) && <p className="page-note" role="status">Connecting and waiting for download bytes.</p>}
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
                            disabled={j.status === "paused" || j.status === "cancelled"}
                          >
                            <Pause size={13} />
                            Pause
                          </Button>
                          <Button
                            size="compact"
                            variant="ghost"
                            onClick={() => void control(j, "cancel")}
                            disabled={j.status === "paused" || j.status === "cancelled"}
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
                  {(j.error || j.last_error) && <p className="inline-error" role="alert">{j.error || j.last_error}</p>}
                </article>
              ))
          )}
          <p className="page-note">
            Completed models remain Disabled until you enable them. Loading is a
            separate operation.
          </p>
        </details>
      )}
      {tab !== "import" && (
        <>
          <p className="library-journey">Choose a model → review files → download → enable and run → try a request</p>
          <DiscoverModels initialSearch={initialRepository} models={models} jobs={jobs} busy={busy} onRepository={(repo) => void inspect(repo)} onDownload={reviewDownload} onInspect={onInspect} onTry={onTry} onDownloads={() => { setActivityOpen(true); activity.current?.scrollIntoView({ block: "start" }); }} />
          {reviewingRepo && <p className="page-note" role="status"><LoaderCircle size={14} className="spin" /> Reviewing files in {reviewingRepo}… <Button variant="ghost" size="compact" onClick={() => { reviewController.current?.abort(); setBusy(false); setReviewingRepo(""); }}>Cancel review</Button></p>}
          {reviewError && !variants && !reviewedModel && <div role="alert" className="inline-error">{reviewError}{reviewSource.current && <Button variant="outline" size="compact" onClick={() => void inspect(reviewSource.current!.repo, reviewSource.current!.source)}>Retry file review</Button>}</div>}
          {reviewedModel && <Dialog.Root open onOpenChange={(open) => { if (!open) setReviewedModel(undefined); }}><Dialog.Portal><Dialog.Backdrop className="dialog-backdrop" /><Dialog.Popup className="model-drawer variant-panel" finalFocus={returnReviewFocus}>
            <div className="dialog-heading"><Dialog.Title>Review download</Dialog.Title><Button size="icon-xs" variant="ghost" aria-label="Close download review" onClick={() => setReviewedModel(undefined)}><X size={16} /></Button></div>
            <h2>{reviewedModel.id}</h2><p className="page-note">{runtimeName(reviewedModel.recipe)} · {reviewedModel.size ? bytes(reviewedModel.size * 1e9) + " weights" : "Weight size not reported"}</p>
            <h3>Declared files and companions</h3><dl className="download-file-list">{Object.entries(reviewedModel.checkpoints || { main: reviewedModel.checkpoint || "Not reported" }).map(([key, source]) => <div key={key}><dt>{key}</dt><dd>{source}</dd></div>)}</dl>
            <p className="page-note">Weight size excludes context, workspace and runtime memory. Host fit is not established by catalog metadata. Review the runtime, then test the model at the context you need.</p>
            <p className="page-note">Downloading keeps this model Disabled. After it finishes, choose On demand or Hot, load it, then try a request.</p>
            {reviewError && <p className="inline-error" role="alert">{reviewError}</p>}
            <Button disabled={busy} onClick={() => void pull(reviewedModel.id)}>{busy && <LoaderCircle size={14} className="spin" />}Download model</Button>
          </Dialog.Popup></Dialog.Portal></Dialog.Root>}
          {variants && (
            <Dialog.Root
              open
              onOpenChange={(open) => {
                if (!open) setVariants(undefined);
              }}
            >
              <Dialog.Portal>
                <Dialog.Backdrop className="dialog-backdrop" />
                <Dialog.Popup className="model-drawer variant-panel" finalFocus={returnReviewFocus}>
                  <div className="dialog-heading">
                    <div className="heading-with-help"><Dialog.Title>{variants.suggested_name}</Dialog.Title><HelpTip label="File review">Review the source, weight size and optional vision projector. Publisher names and quants identify variants independently.</HelpTip></div>
                    <Button
                      size="icon-xs"
                      variant="ghost"
                      aria-label="Close file review"
                      onClick={() => setVariants(undefined)}
                    >
                      <X size={16} />
                    </Button>
                  </div>
                  <p className="variant-source">
                    <a
                      href={repositoryUrl(variants.checkpoint, variants.source)}
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
                    <legend>Choose a published variant</legend>
                    <p className="page-note">Quants offer different numerical precision and weight size. Quality, speed and context memory need checking for your model and runtime.</p>
                    <div>
                      {variants.variants?.map((v: any) => {
                        const existing = existingVariant(
                          models,
                          variants.checkpoint,
                          v,
                          variants.recipe || "llamacpp",
                          projector,
                          variants.source,
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
                            <strong>{quantization(v.name) || v.name}</strong>
                            {quantization(v.name) && quantization(v.name) !== v.name && <small>{v.name}</small>}
                            <span>{v.size_bytes ? bytes(v.size_bytes) : "Size not reported"}</span>
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
                        <option value="">Text only · no projector</option>
                        {variants.mmproj_files.map((f: string) => (
                          <option key={f}>{f}</option>
                        ))}
                      </select>
                    </label>
                  )}
                  <div className="heading-with-help page-note">Backend: {runtimeName(variants.recipe)}<HelpTip label="Download compatibility">The runtime must support this model's architecture. Repository tags do not establish compatibility with your runtime or hardware.</HelpTip></div>
                  <div>
                    <label>
                      Registration name
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
                  </div>
                  {reviewError && <p className="inline-error" role="alert">{reviewError}</p>}
                  {!variant && <p className="page-note">Choose a variant above to review its files and enable downloading. No variant has been selected for you.</p>}
                  {selectedVariant && <><h3>Files in this download</h3><ul className="download-file-list">{(selectedVariant.files?.length ? selectedVariant.files : [selectedVariant.primary_file || selectedVariant.name]).map((f: string) => <li key={f}>{f}</li>)}{projector && <li>{projector} · vision projector</li>}</ul><p className="page-note">{selectedVariant.size_bytes ? bytes(selectedVariant.size_bytes) + " weights" : "Weight size unknown"}. Context, runtime and workspace memory are additional; host fit is unverified.</p></>}
                  {nameConflict && (
                    <p className="inline-error" role="alert">
                      This name belongs to a different checkpoint. Choose a
                      distinct registration name above.
                    </p>
                  )}
                  <Button
                    onClick={() => void download()}
                    disabled={busy || !variant || !name || nameConflict}
                  >
                    <Download size={15} />{" "}
                    {!variant ? "Choose a variant first" : installedVariant?.downloaded
                      ? "Open installed variant"
                      : `Download ${variant}`}
                  </Button>
                </Dialog.Popup>
              </Dialog.Portal>
            </Dialog.Root>
          )}
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
                {j.last_error && <p role="status">{j.last_error}</p>}
                {j.status === "publishing" && j.publication?.conflict && (
                  <label>
                    New model name
                    <input
                      aria-label={`New model name for ${j.filename}`}
                      value={importNames[j.id] || ""}
                      onChange={(e) => setImportNames({ ...importNames, [j.id]: e.target.value })}
                      placeholder="Choose an unused model name"
                    />
                  </label>
                )}
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
              {(j.status === "publishing" || (j.status === "cancelled" && j.cleanup_pending)) && (
                <Button
                  size="compact"
                  variant="outline"
                  disabled={busy || (j.publication?.conflict && !importNames[j.id]?.trim())}
                  onClick={() => void recoverImport(j)}
                >
                  {j.status === "cancelled" ? "Retry cleanup" : j.publication?.conflict ? "Register with new name" : "Finish import"}
                </Button>
              )}
            </article>
          ))}
        </>
      )}
    </>
  );
}
