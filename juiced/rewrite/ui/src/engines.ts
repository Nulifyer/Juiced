import { ApiError, type Client } from "./api";
import { backendNames, type BackendRow } from "./hardware";

export type EngineJob = { id: string; engine: string; backend: string; state: string; downloaded_bytes: number; total_bytes: number | null; error: string | null; package: string | null };
export type Installable = { engine: string; backend: string; archive_bytes: number | null; availability: string; reasons: string[]; recommended: boolean; installed_package: string | null };
export type InstalledEngine = { package: string; engine: string | null; backend: string | null; tag: string | null; models: string[] };
export type Engines = { generation: number; installable: Installable[]; installed: InstalledEngine[]; jobs: EngineJob[] };

const text = (value: unknown) => typeof value === "string" ? value : null;
const number = (value: unknown) => typeof value === "number" && Number.isFinite(value) ? value : null;
const records = (value: unknown, limit = 128) => Array.isArray(value) ? value.slice(0, limit).filter((item): item is Record<string, any> => !!item && typeof item === "object") : [];

export function parseJob(item: Record<string, any>): EngineJob {
  return { id: text(item.id) ?? text(item.job) ?? "", engine: text(item.engine) ?? "", backend: text(item.backend) ?? "", state: text(item.state) ?? "queued", downloaded_bytes: number(item.downloaded_bytes) ?? 0, total_bytes: number(item.total_bytes) ?? number(item.expected_bytes), error: text(item.error) ?? text(item.error_code), package: text(item.package) };
}

function downloadBytes(build: unknown): number | null {
  if (!build || typeof build !== "object") return null;
  const archive = number((build as Record<string, any>).archive_bytes);
  return archive === null ? null : archive + (number((build as Record<string, any>).runtime?.archive_bytes) ?? 0);
}

export function parseEngines(value: unknown): Engines {
  if (!value || typeof value !== "object") throw new Error("The service returned an invalid engine list.");
  const item = value as Record<string, any>;
  return {
    generation: number(item.generation) ?? 0,
    installable: records(item.installable).map((row) => ({ engine: text(row.engine) ?? "", backend: text(row.backend) ?? "", archive_bytes: downloadBytes(row.build), availability: text(row.availability) ?? "unknown", reasons: records(row.reasons).map((reason) => text(reason.message) ?? ""), recommended: row.recommended === true, installed_package: text(row.installed_package) })),
    installed: records(item.installed).map((row) => ({ package: text(row.package) ?? "", engine: text(row.engine), backend: text(row.backend) ?? text(row.release?.backend), tag: text(row.release?.tag) ?? text(row.tag), models: records(row.models).map((model) => text(model.model) ?? "") })),
    jobs: records(item.jobs, 32).map(parseJob),
  };
}

export function downloadSize(value: number | null): string {
  if (value == null) return "";
  const mib = value / 1024 ** 2;
  if (mib >= 1000) return `${(mib / 1024).toFixed(1)} GiB`;
  if (mib >= 100) return `${Math.round(mib)} MiB`;
  if (mib >= 1) return `${mib.toFixed(1)} MiB`;
  return `${Math.max(1, Math.round(value / 1024))} KiB`;
}

export const activeJob = (job: EngineJob) => !["complete", "failed"].includes(job.state);

export async function installEngine(client: Client, engine: string, backend: string, onProgress: (job: EngineJob) => void, signal?: AbortSignal): Promise<string> {
  const current = parseEngines(await client.request("juiced/engines", undefined, signal));
  const started = await client.request<Record<string, any>>("juiced/engines/install", { engine, backend, expected_generation: current.generation }, signal);
  if (started.already_installed) return text(started.package) ?? "";
  const id = text(started.job?.id) ?? text(started.job) ?? text(started.id);
  while (true) {
    await new Promise((resolve) => setTimeout(resolve, 1000));
    const engines = parseEngines(await client.request("juiced/engines", undefined, signal));
    const job = engines.jobs.find((entry) => entry.id === id) ?? engines.jobs.find((entry) => entry.engine === engine && entry.backend === backend);
    if (job) onProgress(job);
    if (!job || job.state === "complete") return job?.package ?? "";
    if (job.state === "failed") throw new ApiError(`The ${engine} ${backend} engine failed to install${job.error ? ` (${job.error})` : ""}.`, 500, job.error ?? "engine_install_failed");
  }
}

const managed: Record<string, { recipe: string; name: string; purpose: string }> = {
  "llama.cpp": { recipe: "llamacpp", name: "llama.cpp", purpose: "Chat and embeddings" },
  "whisper.cpp": { recipe: "whispercpp", name: "whisper.cpp", purpose: "Speech to text" },
  "sd.cpp": { recipe: "sd-cpp", name: "sd.cpp", purpose: "Images" },
  kokoros: { recipe: "kokoro", name: "Kokoro", purpose: "Text to speech" },
  "parakeet.cpp": { recipe: "parakeet", name: "Parakeet", purpose: "Speech to text" },
  "futo-transcribe.cpp": { recipe: "asr4all", name: "FUTO transcribe", purpose: "Speech to text" },
  moonshine: { recipe: "moonshine", name: "Moonshine", purpose: "Speech to text" },
  fastflowlm: { recipe: "flm", name: "FastFlowLM", purpose: "Chat on the NPU" },
};

export type ChipState = "installed" | "installing" | "installable" | "unavailable" | "no-installer";
export type BackendChip = { key: string; engine: string; backend: string; label: string; state: ChipState; recommended: boolean; download_bytes: number | null; models: number; reason: string; job: EngineJob | null };
export type EngineGroup = { key: string; name: string; purpose: string; installable: boolean; chips: BackendChip[] };

const order: Record<ChipState, number> = { installed: 0, installing: 1, installable: 1, unavailable: 2, "no-installer": 2 };
export const shownByDefault = (chip: BackendChip) => order[chip.state] < 2;

export function engineGroups(engines: Engines, backends: BackendRow[], jobs: Record<string, EngineJob> = {}): EngineGroup[] {
  const groups = new Map<string, EngineGroup>();
  const label = (backend: string) => backendNames[backend] ?? backend;
  const claimed = new Set<string>();
  for (const row of engines.installable) {
    const meta = managed[row.engine];
    const group = groups.get(row.engine) ?? { key: row.engine, name: meta?.name ?? row.engine, purpose: meta?.purpose ?? "", installable: true, chips: [] };
    groups.set(row.engine, group);
    const key = `${row.engine}:${row.backend}`;
    const job = jobs[key] ?? engines.jobs.find((entry) => entry.engine === row.engine && entry.backend === row.backend && activeJob(entry)) ?? null;
    const installed = row.installed_package ? engines.installed.find((entry) => entry.package === row.installed_package) : undefined;
    if (row.installed_package) claimed.add(row.installed_package);
    const state: ChipState = row.installed_package ? "installed" : job ? "installing" : row.availability === "available" ? "installable" : "unavailable";
    group.chips.push({ key, engine: row.engine, backend: row.backend, label: label(row.backend), state, recommended: row.recommended, download_bytes: row.archive_bytes, models: installed?.models.length ?? 0, reason: row.reasons.filter(Boolean).join(" "), job });
  }
  const unclaimed = new Map<string, InstalledEngine[]>();
  for (const entry of engines.installed) {
    if (!entry.engine || !groups.has(entry.engine) || claimed.has(entry.package)) continue;
    const key = `${entry.engine}:installed:${entry.backend ?? ""}`;
    unclaimed.set(key, [...(unclaimed.get(key) ?? []), entry]);
  }
  for (const [key, same] of unclaimed) {
    const { engine, backend } = { engine: same[0]!.engine!, backend: same[0]!.backend ?? "" };
    groups.get(engine)!.chips.push({ key, engine, backend, label: backend ? label(backend) : same.length > 1 ? `Imported ×${same.length}` : "Imported", state: "installed", recommended: false, download_bytes: null, models: same.reduce((total, entry) => total + entry.models.length, 0), reason: "", job: null });
  }
  const byRecipe = new Map(Object.entries(managed).map(([engine, meta]) => [meta.recipe, engine]));
  for (const row of backends) {
    if (row.backend === "system") continue;
    const engine = byRecipe.get(row.recipe);
    if (engine && groups.has(engine)) {
      const group = groups.get(engine)!;
      if (group.chips.some((chip) => chip.backend === row.backend)) continue;
      group.chips.push({ key: `${engine}:${row.backend}`, engine, backend: row.backend, label: label(row.backend), state: row.state === "available" ? "no-installer" : "unavailable", recommended: false, download_bytes: null, models: 0, reason: row.state === "available" ? "No installer for this build yet." : row.reasons.map((reason) => reason.message).join(" "), job: null });
      continue;
    }
    const key = `recipe:${row.recipe}`;
    const group = groups.get(key) ?? { key, name: row.display_name || row.recipe, purpose: "Not for this machine", installable: false, chips: [] };
    if (row.state === "available") group.purpose = "No installer yet";
    groups.set(key, group);
    group.chips.push({ key: `${key}:${row.backend}`, engine: row.recipe, backend: row.backend, label: label(row.backend), state: row.state === "available" ? "no-installer" : "unavailable", recommended: false, download_bytes: null, models: 0, reason: row.state === "available" ? "Fits this machine, but Juiced can't install it yet." : row.reasons.map((reason) => reason.message).join(" "), job: null });
  }
  const rank = (chip: BackendChip) => order[chip.state] * 2 - Number(chip.recommended);
  const result = [...groups.values()].map((group) => ({ ...group, chips: group.chips.map((chip, index) => [chip, index] as const).sort(([left, a], [right, b]) => rank(left) - rank(right) || a - b).map(([chip]) => chip) }));
  return [...result.filter((group) => group.installable), ...result.filter((group) => !group.installable).sort((left, right) => left.name.localeCompare(right.name))];
}
