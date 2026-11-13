export type QuantJob = { id: string; state: string; downloaded_bytes: number; expected_bytes: number | null };
export type QuantFile = { file: string; bytes: number | null; member: string | null };
export type Quant = { label: string; kind: "quant" | "bundle" | "checkpoint"; engine: string | null; file: string; parts: number; bytes: number | null; recommended: boolean; pullable: boolean; present: boolean; presentFiles: number; set: string | null; localPath: string | null; readOnly: boolean; sha256: string | null; files: QuantFile[]; jobs: QuantJob[] };
export type LibraryModel = { id: string; repo: string; name: string; nativeId: string | null; publisher: string | null; task: string | null; source: "catalog" | "huggingface" | "local"; downloads: number | null; likes: number | null; license: string | null; gated: boolean; quants: Quant[] };
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

export const tasks = [["chat", "Chat"], ["embeddings", "Embeddings"], ["transcription", "Speech to text"], ["tts", "Text to speech"], ["image", "Images"], ["audio-generation", "Audio"], ["3d", "3D"], ["classify", "Classify"]] as const;
/** Engines Discover can narrow to; the checkpoint engines list original model folders. */
export const searchEngines = [["llama.cpp", "llama.cpp (GGUF)"], ["vllm", "vLLM"], ["onnxruntime", "ONNX Runtime"], ["thenoise", "TheNoise"]] as const;
export const engineNames: Record<string, string> = { "llama.cpp": "llama.cpp", vllm: "vLLM", onnxruntime: "ONNX Runtime", thenoise: "TheNoise", "sd.cpp": "sd.cpp", "whisper.cpp": "whisper.cpp", kokoros: "Kokoro", moonshine: "Moonshine" };
