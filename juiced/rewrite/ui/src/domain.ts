import type { Residency } from "./Residency";

export type Policy = "disabled" | "on-demand" | "hot";
export const policyNames: Record<Policy, string> = { disabled: "Disabled", "on-demand": "On demand", hot: "Hot" };
export type Model = {
  id: string;
  artifact_set?: string;
  runtime_package?: string;
  policy: Policy;
  revision: number;
  state: "ready" | "loading" | "unloaded" | "missing-weights" | "failed" | "stopping" | "cleanup-pending";
  downloaded: boolean;
  loaded: boolean;
  pid: number | null;
  active_requests: number;
  max_inflight: number;
  resident_bytes: number | null;
  operations: string[];
  error: string | null;
  startup_waiting?: boolean;
  cleanup_retry_available?: boolean;
  uncertain_requests?: number;
  uncertain_state_operations?: number;
  request_limits?: { input_bytes: number; batch: number; output_tokens: number };
  residency?: Residency;
  /** The worker binds a vision projector, so chat messages may include images. */
  vision?: boolean;
};
export type RequestRecord = { id: number; model: string; operation: string; outcome: string; status: number | null; elapsed_ms: number; input_tokens?: number | null; output_tokens?: number | null; decode_tokens_per_second?: number | null };
export type CacheControlMemory = { state: "accounted" | "unavailable" | "legacy"; reason: string | null; metadata_bytes: number; scratch_bytes: number; reserved_bytes: number; outstanding_bytes: number; active_operations: number; bounds?: { bootstrap_bytes: number; metadata_bytes: number; scratch_bytes: number; state_scratch_bytes: number; page_scratch_bytes: number; response_bytes: number } };
export type RuntimeControlMemory = { state: "accounted" | "legacy"; metadata_bytes: number; binding_bytes: number; binding_count: number; scratch_bytes: number; active_operations: number; reserved_bytes: number; outstanding_bytes: number; bounds: { metadata_bytes: number; bootstrap_bytes: number; import_bytes: number; response_work_bytes: number; response_bytes: number; binding_work_bytes: number } };
export type Resources = { resident_budget_bytes: number | null; reserved_bytes: number; measured_memory_bytes: number | null; reservation_source: string; active_requests: number; paused?: boolean; cache_control_memory?: CacheControlMemory; runtime_control_memory?: RuntimeControlMemory };
export type Snapshot = { models: Model[]; requests: RequestRecord[]; resources: Resources; version: string };

export function bytes(value: number | null): string {
  if (value === null) return "Unavailable";
  if (value < 1024) return `${value} B`;
  if (value < 1024 ** 2) return `${(value / 1024).toFixed(1)} KiB`;
  if (value < 1024 ** 3) return `${(value / 1024 ** 2).toFixed(1)} MiB`;
  return `${(value / 1024 ** 3).toFixed(1)} GiB`;
}

export function operationEligible(model: Model, operation: string): boolean {
  return model.policy !== "disabled" && model.downloaded && model.state !== "stopping" && model.state !== "cleanup-pending" && model.operations.includes(operation);
}

export function eligible(model: Model): boolean {
  return operationEligible(model, "chat/completions");
}

export function loadReason(model: Model): string | undefined {
  if (model.state === "stopping") return "Wait for the owned worker to confirm exit. Its reservation is retained.";
  if (model.state === "cleanup-pending") return "The previous worker has not confirmed exit. Retry cleanup with Unload.";
  if (model.policy === "disabled") return "Enable the loading policy first.";
  if (!model.downloaded) return "The configured weights are unavailable.";
  if (model.loaded) return "This worker is already loaded.";
  if (model.startup_waiting) return "This worker is waiting to start.";
  if (model.state === "loading") return "This worker is starting.";
}

export function workerReady(model: Model): boolean {
  return model.loaded && model.state === "ready";
}

export function workerTone(model: Model): "good" | "warning" | "bad" | "quiet" {
  if (model.state === "cleanup-pending" || model.state === "stopping") return "warning";
  return workerReady(model) ? "good" : model.error ? "bad" : "quiet";
}

export function workerState(model: Model): string {
  return model.startup_waiting ? "Waiting to start" : model.state;
}

export function unloadReason(model: Model): string | undefined {
  if (model.state === "stopping") return "An exit confirmation attempt is already in progress.";
  if (model.state === "cleanup-pending" && model.cleanup_retry_available === true) return undefined;
  if (!model.loaded) return "This worker is not loaded.";
  if (model.active_requests > 0) return "Wait for active requests to finish.";
  if (model.policy === "hot") return "Change to On demand before unloading.";
}

export const services = [["chat", "Chat"], ["transcription", "Transcription"], ["embeddings", "Embeddings"]] as const;
export type Service = typeof services[number][0];
const serviceOperations: Record<Service, string[]> = { chat: ["chat/completions", "completions", "responses"], transcription: ["audio/transcriptions"], embeddings: ["embeddings"] };
export function offers(model: Model, service: Service): boolean {
  return model.operations.some((operation) => serviceOperations[service].includes(operation));
}

export function matchingModels(models: Model[], query: string): Model[] {
  const needle = query.trim().toLowerCase();
  return models.filter((model) => `${model.id} ${model.policy} ${model.state} ${model.operations.join(" ")}`.toLowerCase().includes(needle));
}

function parseCacheControl(value: unknown): CacheControlMemory | undefined {
  if (value === undefined) return undefined;
  const invalid = () => new Error("The service returned invalid cache memory reservations. Refresh or check the service version.");
  if (!value || typeof value !== "object") throw invalid();
  const v = value as Record<string, unknown>;
  const nonnegative = (n: unknown): n is number => typeof n === "number" && Number.isSafeInteger(n) && n >= 0;
  if (typeof v.state !== "string" || !["accounted", "unavailable", "legacy"].includes(v.state) || ![v.metadata_bytes, v.scratch_bytes, v.reserved_bytes, v.outstanding_bytes, v.active_operations].every(nonnegative)) throw invalid();
  const resident = v.metadata_bytes as number, scratch = v.scratch_bytes as number;
  if (resident + scratch !== v.reserved_bytes || v.outstanding_bytes !== v.reserved_bytes) throw invalid();
  if (v.state === "accounted") {
    const bounds = v.bounds as Record<string, unknown> | undefined;
    if (!bounds || ![bounds.bootstrap_bytes, bounds.metadata_bytes, bounds.scratch_bytes, bounds.state_scratch_bytes, bounds.page_scratch_bytes, bounds.response_bytes].every((n) => nonnegative(n) && n > 0) || (bounds.page_scratch_bytes as number) > (bounds.state_scratch_bytes as number) || (bounds.state_scratch_bytes as number) > (bounds.scratch_bytes as number) || bounds.metadata_bytes !== resident || (bounds.bootstrap_bytes as number) < resident || scratch < (v.active_operations as number) * (bounds.page_scratch_bytes as number) || scratch > (v.active_operations as number) * (bounds.scratch_bytes as number)) throw invalid();
  } else if (resident !== 0 || scratch !== 0 || v.active_operations !== 0) throw invalid();
  if (v.state === "unavailable" && (typeof v.reason !== "string" || !/^[a-z_]{1,128}$/.test(v.reason))) throw invalid();
  return { ...v, reason: typeof v.reason === "string" ? v.reason : null } as CacheControlMemory;
}

function parseRuntimeControl(value: unknown): RuntimeControlMemory | undefined {
  if (value === undefined) return undefined;
  const invalid = () => new Error("The service returned invalid runtime memory reservations. Refresh or check the service version.");
  if (!value || typeof value !== "object" || Array.isArray(value)) throw invalid();
  const v = value as Record<string, unknown>;
  const nonnegative = (n: unknown): n is number => typeof n === "number" && Number.isSafeInteger(n) && n >= 0;
  if (!["accounted", "legacy"].includes(v.state as string) || ![v.metadata_bytes, v.binding_bytes, v.binding_count, v.scratch_bytes, v.active_operations, v.reserved_bytes, v.outstanding_bytes].every(nonnegative)) throw invalid();
  const bytes = (v.metadata_bytes as number) + (v.binding_bytes as number) + (v.scratch_bytes as number);
  const bounds = v.bounds as Record<string, unknown> | undefined;
  if (!bounds || ![bounds.metadata_bytes, bounds.bootstrap_bytes, bounds.import_bytes, bounds.response_work_bytes, bounds.response_bytes, bounds.binding_work_bytes].every(n => nonnegative(n) && n > 0) || (bounds.bootstrap_bytes as number) < (bounds.metadata_bytes as number) || (bounds.response_work_bytes as number) < (bounds.response_bytes as number) || (bounds.import_bytes as number) < (bounds.response_work_bytes as number) || bytes !== v.reserved_bytes || v.outstanding_bytes !== v.reserved_bytes || (v.active_operations === 0 && v.scratch_bytes !== 0) || (v.binding_count === 0 && v.binding_bytes !== 0)) throw invalid();
  if (v.state === "legacy" && bytes !== 0) throw invalid();
  if (v.state === "accounted" && ((v.metadata_bytes !== 0 && v.metadata_bytes !== bounds.metadata_bytes) || (v.active_operations !== 0 && v.scratch_bytes === 0) || (v.binding_count !== 0 && v.binding_bytes === 0))) throw invalid();
  return value as RuntimeControlMemory;
}

export function parseSnapshot(catalog: unknown, state: unknown, requests: unknown): Snapshot {
  const c = catalog as { schema_version?: unknown; data?: unknown };
  const s = state as { schema_version?: unknown; implementation?: unknown; version?: unknown; resources?: Resources };
  const r = requests as { data?: unknown };
  if (c?.schema_version !== 1 || s?.schema_version !== 1 || s?.implementation !== "rust-experiment" || !Array.isArray(c.data) || !Array.isArray(r?.data) || typeof s.version !== "string" || !s.resources) {
    throw new Error("This console needs the matching Rust experiment. Check the service URL.");
  }
  const positive = (value: unknown) => typeof value === "number" && Number.isSafeInteger(value) && value > 0;
  const limitsValid = (value: unknown) => {
    if (value === undefined) return true;
    if (value === null || typeof value !== "object") return false;
    const limits = value as Record<string, unknown>;
    return positive(limits.input_bytes) && positive(limits.batch) && positive(limits.output_tokens);
  };
  for (const model of c.data) {
    if (!model || typeof model.id !== "string" || !Object.hasOwn(policyNames, model.policy) || !Number.isSafeInteger(model.revision) || !limitsValid(model.request_limits) || (model.artifact_set !== undefined && (typeof model.artifact_set !== "string" || !/^set-[a-f0-9]{64}$/.test(model.artifact_set))) || (model.runtime_package !== undefined && (typeof model.runtime_package !== "string" || !/^package-[a-f0-9]{64}$/.test(model.runtime_package))) || (model.startup_waiting !== undefined && typeof model.startup_waiting !== "boolean") || (model.vision !== undefined && typeof model.vision !== "boolean") || (model.cleanup_retry_available !== undefined && typeof model.cleanup_retry_available !== "boolean") || (model.uncertain_requests !== undefined && (!Number.isSafeInteger(model.uncertain_requests) || model.uncertain_requests < 0)) || (model.uncertain_state_operations !== undefined && (!Number.isSafeInteger(model.uncertain_state_operations) || model.uncertain_state_operations < 0)) || !Array.isArray(model.operations) || !model.operations.every((operation: unknown) => typeof operation === "string")) {
      throw new Error("The service returned an invalid model catalog. Refresh or check the service version.");
    }
  }
  if (!r.data.every((record) => record && typeof record.model === "string" && typeof record.operation === "string" && typeof record.outcome === "string" && Number.isSafeInteger(record.id) && Number.isFinite(record.elapsed_ms))) {
    throw new Error("The service returned invalid request metadata. Refresh or check the service version.");
  }
  const control = parseCacheControl(s.resources.cache_control_memory);
  const runtime = parseRuntimeControl(s.resources.runtime_control_memory);
  return { models: c.data, resources: { ...s.resources, ...(control ? { cache_control_memory: control } : {}), ...(runtime ? { runtime_control_memory: runtime } : {}) }, version: s.version, requests: r.data };
}

export function normalizeBase(value: string): string {
  if (!value.trim()) return "";
  const url = new URL(value.trim());
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash || url.pathname !== "/") {
    throw new Error("Use an HTTP or HTTPS service origin without credentials, path, query or fragment.");
  }
  return url.origin;
}

export type Suitability = {
  schema_version: 1;
  read_only: true;
  resources: {
    fit: "unknown";
    blockers: string[];
    declared_budget: { available_bytes: number | null; additional_worker_bytes: number | null; fits_additional_reservation: boolean | null };
  };
};

export function parseSuitability(value: unknown): Suitability {
  const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
  const bytes = (value: unknown) => value === null || (Number.isSafeInteger(value) && (value as number) >= 0);
  if (object(value) && value.schema_version === 1 && value.read_only === true && object(value.resources)) {
    const resources = value.resources;
    const budget = resources.declared_budget;
    if (resources.fit === "unknown" && Array.isArray(resources.blockers) && resources.blockers.every((code) => typeof code === "string") && object(budget) && bytes(budget.available_bytes) && bytes(budget.additional_worker_bytes) && (budget.fits_additional_reservation === null || typeof budget.fits_additional_reservation === "boolean")) return value as Suitability;
  }
  throw new Error("This service returned an unsupported suitability contract. Check the service version.");
}
