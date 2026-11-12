export type Provider = "external-http" | "llama-cpp" | "whisper-cpp";
export type LaunchOptions = { context_tokens: number | null; threads: number | null; gpu_layers: number | null; parallel_slots: number | null };
export type LaunchDraft = { context: string; threads: string; gpu: string };
export type ModelOptions = { model: string; revision: number; provider: Provider; saved: LaunchOptions; effective_launch: LaunchOptions | null; physical_slots: number; context_semantics: string; options_authority?: "local-profile" | "native-settings"; editable_fields?: string[] };
export type ModelContext = {
  schema_version: number; model: string; revision: number; provider: Provider;
  declared: { source: string; format_version: number; architecture: string | null; training_context_tokens: number | null } | null;
  configured: LaunchOptions | null;
  applied: { source: string; launch: LaunchOptions | null; pid: number; engine_confirmed: boolean } | null;
  observed: { props: { source: string; per_request_tokens: number | null; slots: number | null; engine_build: string | null }; slots: { id: number; context_tokens: number | null; is_processing: boolean | null }[] } | null;
  observation_error: string | null; qualification: string;
};
const object = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value);
const integer = (value: unknown): value is number => Number.isSafeInteger(value) && (value as number) >= 0;
const optionalInteger = (value: unknown): boolean => value === null || integer(value);
const provider = (value: unknown): value is Provider => value === "llama-cpp" || value === "external-http" || value === "whisper-cpp";
const options = (value: unknown): value is LaunchOptions => object(value) && ["context_tokens", "threads", "gpu_layers", "parallel_slots"].every((key) => optionalInteger(value[key]));
export function parseModelOptions(value: unknown): ModelOptions {
  if (!object(value) || typeof value.model !== "string" || !integer(value.revision) || !provider(value.provider) || !options(value.saved) || !(value.effective_launch === null || options(value.effective_launch)) || !integer(value.physical_slots) || value.physical_slots === 0 || typeof value.context_semantics !== "string") throw new Error("The model launch settings are invalid. Refresh before saving a profile.");
  if (value.options_authority !== undefined && value.options_authority !== "local-profile" && value.options_authority !== "native-settings") throw new Error("The model settings owner is unsupported. Refresh before saving.");
  if (value.options_authority === "native-settings" && (value.provider !== "llama-cpp" || !Array.isArray(value.editable_fields) || value.editable_fields.length !== 1 || value.editable_fields[0] !== "context_tokens" || value.saved.threads !== null || value.saved.gpu_layers !== null || value.saved.parallel_slots !== null)) throw new Error("The imported model settings expose unsupported editable fields.");
  return value as ModelOptions;
}
export function parseModelContext(value: unknown): ModelContext {
  if (!object(value) || value.schema_version !== 1 || typeof value.model !== "string" || !integer(value.revision) || !provider(value.provider) || typeof value.qualification !== "string" || !(value.configured === null || options(value.configured))) throw new Error("The service returned an invalid context observation. Refresh the context check.");
  if (value.declared !== null && (!object(value.declared) || typeof value.declared.source !== "string" || !optionalInteger(value.declared.training_context_tokens) || !(value.declared.architecture === null || typeof value.declared.architecture === "string"))) throw new Error("The model metadata observation is invalid. Training context remains unknown.");
  if (value.applied !== null && (!object(value.applied) || typeof value.applied.source !== "string" || !integer(value.applied.pid) || !(value.applied.launch === null || options(value.applied.launch)))) throw new Error("The applied launch observation is invalid. Refresh the context check.");
  if (value.observed !== null) {
    const observed = value.observed;
    if (!object(observed) || !object(observed.props) || !optionalInteger(observed.props.per_request_tokens) || !optionalInteger(observed.props.slots) || !Array.isArray(observed.slots) || !observed.slots.every((slot) => object(slot) && integer(slot.id) && optionalInteger(slot.context_tokens) && (slot.is_processing === null || typeof slot.is_processing === "boolean"))) throw new Error("The engine slot observation is invalid. Serving context remains unknown.");
  }
  if (!(value.observation_error === null || typeof value.observation_error === "string")) throw new Error("The context observation status is invalid.");
  return value as ModelContext;
}
export function launchDraft(value: LaunchOptions): LaunchDraft {
  return { context: value.context_tokens?.toString() ?? "", threads: value.threads?.toString() ?? "", gpu: value.gpu_layers?.toString() ?? "" };
}
export function sameDraft(left: LaunchDraft, right: LaunchDraft): boolean {
  return left.context === right.context && left.threads === right.threads && left.gpu === right.gpu;
}
function optionalSetting(value: string, name: string, minimum: number, maximum: number): number | null {
  if (!value.trim()) return null;
  if (!/^\d+$/.test(value.trim())) throw new Error(`${name} must be a whole number, or empty to use the engine arguments or default.`);
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) throw new Error(`${name} must be between ${minimum} and ${maximum}.`);
  return parsed;
}
export function launchOptions(draft: LaunchDraft, physicalSlots: number | null): LaunchOptions {
  return { context_tokens: optionalSetting(draft.context, "Total context budget", 1, 2147483647), threads: optionalSetting(draft.threads, "Threads", 1, 65536), gpu_layers: optionalSetting(draft.gpu, "GPU layers", 0, 2147483647), parallel_slots: physicalSlots };
}
export function editedLaunchOptions(view: ModelOptions, draft: LaunchDraft): LaunchOptions {
  if (view.options_authority === "native-settings") {
    if (draft.threads.trim() || draft.gpu.trim()) throw new Error("Backend placement and threads stay with the imported profile.");
    return launchOptions({ context: draft.context, threads: "", gpu: "" }, null);
  }
  return launchOptions(draft, view.saved.parallel_slots);
}

export function servingContext(context: ModelContext): number | null {
  return context.observed?.props.per_request_tokens ?? null;
}
