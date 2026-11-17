export type QuantJob = { id: string; state: string; downloaded_bytes: number; expected_bytes: number | null };
export type QuantFile = { file: string; bytes: number | null; member: string | null };
export type Quant = { label: string; kind: "quant" | "bundle" | "checkpoint"; engine: string | null; file: string; parts: number; bytes: number | null; recommended: boolean; pullable: boolean; present: boolean; presentFiles: number; set: string | null; localPath: string | null; readOnly: boolean; sha256: string | null; files: QuantFile[]; jobs: QuantJob[]; users: string[] | null; traits: string[] };
export type LibraryModel = { id: string; repo: string; name: string; nativeId: string | null; publisher: string | null; task: string | null; source: "catalog" | "huggingface" | "local" | "flm"; downloads: number | null; likes: number | null; license: string | null; gated: boolean; quants: Quant[] };
export type SearchResult = { generation: number; offline: boolean; hub: string; authenticated: boolean; models: LibraryModel[]; next: string | null };
export type PullFile = { file: string; bytes: number | null; sha256: string | null; state: string; downloaded_bytes: number; job: string | null; error: string | null };
export type Pull = { id: string; state: string; error: string | null; repo: string; label: string | null; files: PullFile[]; set: string | null };

const text = (value: unknown) => typeof value === "string" ? value : null;
const number = (value: unknown) => typeof value === "number" && Number.isFinite(value) ? value : null;
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The service returned an invalid library response.");
  return value as Record<string, unknown>;
}
const list = (value: unknown, limit: number) => Array.isArray(value) ? value.slice(0, limit).map(record) : [];

export function parseSearch(value: unknown): SearchResult {
  const item = record(value), sources = record(item.sources ?? {}), hub = record(sources.huggingface ?? {}), page = record(item.page ?? {});
  return {
    generation: number(item.artifact_generation) ?? 0, offline: item.offline === true, hub: text(hub.status) ?? "unavailable", authenticated: hub.authenticated === true, next: text(page.next_cursor),
    models: list(item.data, 64).map((model) => ({
      id: text(model.id) ?? text(model.repo) ?? "", repo: text(model.repo) ?? "", name: text(model.name) ?? text(model.repo) ?? "", nativeId: text(model.native_id), publisher: text(model.publisher), task: text(model.task),
      source: model.source === "catalog" || model.source === "local" ? model.source : "huggingface",
      downloads: number(model.downloads), likes: number(model.likes), license: text(model.license), gated: model.gated === true,
      quants: list(model.quants, 64).map((quant) => {
        const local = record(quant.local ?? {});
        return {
          label: text(quant.label) ?? "Unknown", kind: quant.kind === "checkpoint" || quant.kind === "bundle" ? quant.kind : "quant", engine: text(quant.engine), file: text(quant.file) ?? "", parts: number(quant.parts) ?? 1, bytes: number(quant.bytes), recommended: quant.recommended === true, pullable: quant.pullable === true,
          present: local.present === true, presentFiles: number(local.present_files) ?? 0, set: text(local.set), localPath: text(local.path), readOnly: local.read_only === true, sha256: text(quant.sha256),
          files: list(quant.files, 64).map((file) => ({ file: text(file.file) ?? "", bytes: number(file.bytes), member: text(file.member) })),
          jobs: list(quant.jobs, 16).map((job) => ({ id: text(job.id) ?? "", state: text(job.state) ?? "", downloaded_bytes: number(job.downloaded_bytes) ?? 0, expected_bytes: number(job.expected_bytes) })),
          users: null, traits: [],
        };
      }),
    })),
  };
}

export function parsePull(value: unknown): Pull {
  const item = record(value);
  return {
    id: text(item.pull) ?? "", state: text(item.state) ?? "unknown", error: text(item.error), repo: text(item.repo) ?? "", label: text(item.label), set: text(item.set),
    files: list(item.files, 64).map((file) => ({ file: text(file.file) ?? "", bytes: number(file.bytes), sha256: text(file.sha256), state: text(file.state) ?? "", downloaded_bytes: number(file.downloaded_bytes) ?? 0, job: text(file.job), error: text(file.error) })),
  };
}

export function progress(quant: Quant): number | null {
  const active = quant.jobs.filter((job) => !["complete", "cancelled", "failed", "quarantined"].includes(job.state));
  if (active.length === 0) return null;
  const done = quant.jobs.reduce((total, job) => total + job.downloaded_bytes, 0);
  return quant.bytes ? Math.min(100, done / quant.bytes * 100) : 0;
}

const common = ["Q4_K_M", "Q5_K_M", "Q6_K", "Q8_0", "F16", "BF16", "MXFP4"];
/** The few quants a result card or a model's quant list shows before the rest. */
export function featured(quants: Quant[]): Quant[] {
  const picked = quants.filter((quant) => quant.recommended || quant.present || quant.jobs.length > 0 || quant.kind === "checkpoint" || common.includes(quant.label.toUpperCase().replace(/^UD-/, "")));
  return (picked.length ? picked : quants.slice(0, 4)).slice(0, 6).sort((left, right) => (left.bytes ?? 0) - (right.bytes ?? 0));
}

/** Repositories publish several files under one quant label, so the file identifies a quant. */
export const quantKey = (quant: Quant) => `${quant.kind}:${quant.engine ?? ""}:${quant.file || quant.label}`;

/** Names for quants whose label another quant shares: the part of the file name that tells them apart. */
export function quantNames(quants: Quant[]): Map<Quant, string> {
  const groups = new Map<string, Quant[]>();
  for (const quant of quants) if (quant.file) groups.set(quant.label, [...(groups.get(quant.label) ?? []), quant]);
  const names = new Map<Quant, string>();
  for (const group of groups.values()) {
    if (group.length < 2) continue;
    const full = group.map((quant) => (quant.file.split("/").pop() ?? quant.file).replace(/\.(gguf|bin|safetensors|onnx|ort)$/i, ""));
    let shared = 0;
    while (shared < full[0]!.length && full.every((name) => name[shared] === full[0]![shared])) shared++;
    // Only a long shared start is cut, at a separator, keeping enough of each name to read as a quant.
    let cut = 0;
    for (let index = 0; shared >= 16 && index < shared; index++) if ("-_.".includes(full[0]![index]!) && full.every((name) => name.length - index - 1 >= 8)) cut = index + 1;
    group.forEach((quant, index) => names.set(quant, full[index]!.slice(cut)));
  }
  return names;
}

export const tasks = [["chat", "Chat"], ["embeddings", "Embeddings"], ["transcription", "Speech to text"], ["tts", "Text to speech"], ["image", "Images"], ["audio-generation", "Audio"], ["3d", "3D"], ["classify", "Classify"]] as const;
/** Engines Discover can narrow to; the checkpoint engines list original model folders. */
export const searchEngines = [["llama.cpp", "llama.cpp (GGUF)"], ["vllm", "vLLM"], ["onnxruntime", "ONNX Runtime"], ["thenoise", "TheNoise"]] as const;
export const engineNames: Record<string, string> = { flm: "FastFlowLM", "llama.cpp": "llama.cpp", vllm: "vLLM", onnxruntime: "ONNX Runtime", thenoise: "TheNoise", "sd.cpp": "sd.cpp", "whisper.cpp": "whisper.cpp", kokoros: "Kokoro", moonshine: "Moonshine" };

/** FastFlowLM's engine id in Discover; its models run on the NPU. */
export const npuEngine = "flm";

export type FlmPolicy = "disabled" | "on-demand" | "hot";
export type FlmCheckpoint = { checkpoint: string; installed: boolean; footprint_gb: number | null; kind: string; labels: string[]; registered: string[] };
export type FlmThen = { policy: FlmPolicy; state: "pending" | "done" | "failed"; model: string | null; message: string | null };
export type FlmJob = { checkpoint: string; state: string; percent: number | null; error: string | null; then: FlmThen | null };
export type Flm = { available: boolean; problems: string[]; models: FlmCheckpoint[]; jobs: FlmJob[]; listError: string | null };

const flmPolicies: FlmPolicy[] = ["disabled", "on-demand", "hot"];
export function parseFlm(value: unknown): Flm {
  const body = value && typeof value === "object" ? value as Record<string, any> : {};
  const items = (entries: unknown) => Array.isArray(entries) ? entries.slice(0, 512).filter((item) => item && typeof item === "object") : [];
  const strings = (entries: unknown, limit: number) => Array.isArray(entries) ? entries.filter((item): item is string => typeof item === "string").slice(0, limit) : [];
  const then = (item: any): FlmThen | null => item && typeof item === "object" ? { policy: flmPolicies.includes(item.policy) ? item.policy : "on-demand", state: item.state === "done" || item.state === "failed" ? item.state : "pending", model: text(item.model), message: text(item.message) } : null;
  return {
    available: body.available === true,
    problems: strings(body.validation?.problems, 16),
    models: items(body.models).map((item: any) => ({ checkpoint: String(item.checkpoint ?? ""), installed: item.installed === true, footprint_gb: number(item.footprint_gb), kind: String(item.kind ?? "chat"), labels: strings(item.labels, 16), registered: strings(item.registered, 16) })).filter((item) => item.checkpoint),
    jobs: items(body.jobs).map((item: any) => ({ checkpoint: String(item.checkpoint ?? ""), state: String(item.state ?? ""), percent: number(item.percent), error: text(item.error), then: then(item.then) })),
    listError: text(body.list_error),
  };
}

/** The newest FastFlowLM download of a checkpoint. */
export const flmJob = (flm: Flm, checkpoint: string) => [...flm.jobs].reverse().find((job) => job.checkpoint === checkpoint);

/**
 * FastFlowLM's chat checkpoints as search results: one model per family, one quant per size tag.
 * Only chat checkpoints can be added, so the others are left out.
 */
export function flmResults(flm: Flm | null, query: string, task: string | null): LibraryModel[] {
  if (!flm?.available || (task !== null && task !== "chat")) return [];
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const families = new Map<string, Quant[]>();
  for (const item of flm.models) {
    if (item.kind !== "chat") continue;
    const cut = item.checkpoint.indexOf(":"), family = cut > 0 ? item.checkpoint.slice(0, cut) : item.checkpoint;
    const haystack = `${item.checkpoint} ${item.labels.join(" ")} fastflowlm flm npu`.toLowerCase();
    if (!terms.every((term) => haystack.includes(term))) continue;
    // FastFlowLM reports the footprint in GiB, the unit the service reserves for it.
    const bytes = item.footprint_gb === null ? null : Math.round(item.footprint_gb * 1024 ** 3);
    const job = flmJob(flm, item.checkpoint);
    const jobs = job?.state === "pulling" ? [{ id: item.checkpoint, state: "downloading", downloaded_bytes: (bytes ?? 0) * (job.percent ?? 0) / 100, expected_bytes: bytes }] : [];
    families.set(family, [...(families.get(family) ?? []), {
      label: cut > 0 ? item.checkpoint.slice(cut + 1) : item.checkpoint, kind: "quant", engine: npuEngine, file: item.checkpoint, parts: 1, bytes, recommended: false, pullable: true,
      present: item.installed, presentFiles: item.installed ? 1 : 0, set: null, localPath: null, readOnly: false, sha256: null, files: [], jobs, users: item.registered, traits: item.labels,
    }]);
  }
  return [...families].map(([family, quants]) => ({ id: `flm:${family}`, repo: `flm:${family}`, name: family, nativeId: null, publisher: "FastFlowLM", task: "chat", source: "flm" as const, downloads: null, likes: null, license: null, gated: false, quants }));
}

export type Machine = { gpu: number | null; ram: number | null };
export type Fit = "gpu" | "memory" | "npu" | "none" | "unknown";
export type FitTone = "good" | "warning" | "bad" | "quiet";
const gpuOnly = new Set(["vllm", "thenoise"]);

/** Where a quant of this size would run on this machine. */
export function quantFit(bytes: number | null, engine: string | null | undefined, machine: Machine): Fit {
  if (engine === npuEngine) return bytes === null || machine.ram === null || bytes <= machine.ram ? "npu" : "none";
  if (bytes === null) return "unknown";
  const needed = bytes * 1.15;
  if (machine.gpu !== null && needed <= machine.gpu) return "gpu";
  if (engine && gpuOnly.has(engine)) return "none";
  return machine.ram !== null && needed <= machine.ram ? "memory" : "none";
}

export function fitLabel(fit: Fit, engine: string | null | undefined, machine: Machine): [string, FitTone] {
  if (fit === "unknown") return ["Size unknown", "quiet"];
  if (fit === "npu") return ["Runs on the NPU", "good"];
  if (fit === "gpu") return ["Fits in GPU memory", "good"];
  if (fit === "memory") return [machine.gpu ? "Too big for the GPU; runs partly on CPU" : "Runs on the CPU", "warning"];
  if (engine && gpuOnly.has(engine)) return [machine.gpu ? "Too big for the GPU" : "Needs a supported GPU", "bad"];
  return ["Likely too big for this machine", "bad"];
}

const GiB = 1024 ** 3;
export const sizeBuckets = [["small", "Under 2 GiB", 0, 2 * GiB], ["medium", "2 to 8 GiB", 2 * GiB, 8 * GiB], ["large", "8 to 24 GiB", 8 * GiB, 24 * GiB], ["huge", "Over 24 GiB", 24 * GiB, Infinity]] as const;
export type SizeBucket = typeof sizeBuckets[number][0];
export type FitFilter = "gpu" | "memory" | "npu";
export const sorts = [["match", "Best match"], ["downloads", "Most downloads"], ["size", "Smallest first"]] as const;
export type Sort = typeof sorts[number][0];
export type ResultFilters = { fit: FitFilter | null; size: SizeBucket | null; sort: Sort };
export type ResultView = { model: LibraryModel; quants: Quant[] };

/** Whether one quant passes the hardware and size filters. "Fits memory" includes what fits the GPU. */
export function quantMatches(quant: Quant, filters: Pick<ResultFilters, "fit" | "size">, machine: Machine): boolean {
  if (filters.fit) {
    const fit = quantFit(quant.bytes, quant.engine, machine);
    if (filters.fit === "memory" ? fit !== "gpu" && fit !== "memory" : fit !== filters.fit) return false;
  }
  if (filters.size) {
    const [, , from, to] = sizeBuckets.find(([id]) => id === filters.size)!;
    if (quant.bytes === null || quant.bytes < from || quant.bytes >= to) return false;
  }
  return true;
}

/** Each model with the quants that pass the filters; a model none of whose quants pass is dropped. */
export function filterResults(models: LibraryModel[], filters: ResultFilters, machine: Machine): ResultView[] {
  const narrowed = filters.fit !== null || filters.size !== null;
  const views = models.map((model) => ({ model, quants: narrowed ? model.quants.filter((quant) => quantMatches(quant, filters, machine)) : model.quants })).filter((view) => !narrowed || view.quants.length > 0);
  if (filters.sort === "match") return views;
  const smallest = (view: ResultView) => view.quants.reduce((least, quant) => quant.bytes !== null && quant.bytes < least ? quant.bytes : least, Infinity);
  const rank = filters.sort === "downloads" ? (view: ResultView) => -(view.model.downloads ?? -1) : smallest;
  return views.map((view, index) => ({ view, index })).sort((left, right) => rank(left.view) - rank(right.view) || left.index - right.index).map((entry) => entry.view);
}
