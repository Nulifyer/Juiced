import { policyNames, type Model } from "./domain";

export type ModelType = "chat" | "embeddings" | "transcription" | "speech" | "images" | "audio" | "3d";
export type ModelState = "loaded" | "enabled" | "disabled";
export type PickerGroup = { state: ModelState; label: string; models: Model[] };
export type PickerView = {
  groups: PickerGroup[];
  /** Models listed, out of `total` the picker could offer. */
  shown: number;
  total: number;
  /** Disabled models left out until asked for. A search always lists them. */
  hidden: number;
  types: { type: ModelType; label: string; count: number }[];
};

/** A model with several operations takes the first type that matches, so each model has one. */
const types: [ModelType, string, string, string][] = [
  ["chat", "Chat", "chat/completions", "llm text"],
  ["embeddings", "Embeddings", "embeddings", "embed embedding"],
  ["transcription", "Speech to text", "audio/transcriptions", "stt transcription transcribe asr"],
  ["speech", "Text to speech", "audio/speech", "tts voice"],
  ["images", "Images", "images/generations", "image picture"],
  ["audio", "Audio", "audio/generations", "music sound"],
  ["3d", "3D", "3d/generations", "mesh"],
];
const stateLabels: Record<ModelState, string> = { loaded: "Loaded", enabled: "Enabled", disabled: "Disabled" };
const SMALL_LIST = 8;

export function modelType(model: Model): ModelType | null {
  return types.find(([, , operation]) => model.operations?.includes(operation))?.[0] ?? null;
}

export function typeLabel(type: ModelType | null): string {
  return types.find(([id]) => id === type)?.[1] ?? "";
}

export function modelState(model: Model): ModelState {
  return model.loaded ? "loaded" : model.policy === "disabled" ? "disabled" : "enabled";
}

function searchText(model: Model): string {
  const type = types.find(([id]) => id === modelType(model));
  const extra = model as Model & { labels?: unknown; recipe?: unknown };
  const labels = Array.isArray(extra.labels) ? extra.labels.filter((label) => typeof label === "string") : [];
  return [model.id, type?.[1], type?.[3], ...labels, typeof extra.recipe === "string" ? extra.recipe : "", policyNames[model.policy], stateLabels[modelState(model)], model.vision ? "vision" : ""].join(" ").toLowerCase();
}

/** Every word of the query must appear in the model's ID, type, labels, engine or state. */
export function matchesQuery(model: Model, query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const text = searchText(model);
  return words.every((word) => text.includes(word));
}

/**
 * Lists a picker's models as Loaded, Enabled and Disabled, each ordered by type and then ID.
 * Disabled models stay out of a long list until asked for; a search, the selected model and a
 * list with nothing enabled show them without asking.
 */
export function pickerView(models: Model[], options: { query?: string; type?: ModelType | null; showDisabled?: boolean; selected?: string; disabledModels?: "toggle" | "never" } = {}): PickerView {
  const { query = "", type = null, showDisabled = false, selected = "", disabledModels = "toggle" } = options;
  const offered = disabledModels === "never" ? models.filter((model) => modelState(model) !== "disabled" || model.id === selected) : models;
  const counts = new Map<ModelType, number>();
  for (const model of offered) { const own = modelType(model); if (own) counts.set(own, (counts.get(own) ?? 0) + 1); }
  const order = (model: Model) => { const index = types.findIndex(([id]) => id === modelType(model)); return index < 0 ? types.length : index; };
  const matching = offered.filter((model) => (type === null || modelType(model) === type) && matchesQuery(model, query))
    .sort((left, right) => order(left) - order(right) || left.id.localeCompare(right.id, undefined, { numeric: true }));
  const searching = query.trim() !== "";
  const anyEnabled = matching.some((model) => modelState(model) !== "disabled");
  const reveal = showDisabled || searching || !anyEnabled || offered.length <= SMALL_LIST;
  const listed = reveal ? matching : matching.filter((model) => modelState(model) !== "disabled" || model.id === selected);
  const groups = (["loaded", "enabled", "disabled"] as const)
    .map((state) => ({ state, label: stateLabels[state], models: listed.filter((model) => modelState(model) === state) }))
    .filter((group) => group.models.length > 0);
  return {
    groups, shown: listed.length, total: offered.length, hidden: matching.length - listed.length,
    types: types.filter(([id]) => counts.has(id)).map(([id, label]) => ({ type: id, label, count: counts.get(id)! })),
  };
}
