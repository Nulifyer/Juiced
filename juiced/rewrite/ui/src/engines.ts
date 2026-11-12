import { ApiError, type Client } from "./api";

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

export function parseEngines(value: unknown): Engines {
  if (!value || typeof value !== "object") throw new Error("The service returned an invalid engine list.");
  const item = value as Record<string, any>;
  return {
    generation: number(item.generation) ?? 0,
    installable: records(item.installable).map((row) => ({ engine: text(row.engine) ?? "", backend: text(row.backend) ?? "", archive_bytes: number(row.build?.archive_bytes), availability: text(row.availability) ?? "unknown", reasons: records(row.reasons).map((reason) => text(reason.message) ?? ""), recommended: row.recommended === true, installed_package: text(row.installed_package) })),
    installed: records(item.installed).map((row) => ({ package: text(row.package) ?? "", engine: text(row.engine), backend: text(row.backend), tag: text(row.release?.tag) ?? text(row.tag), models: records(row.models).map((model) => text(model.model) ?? "") })),
    jobs: records(item.jobs, 32).map(parseJob),
  };
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
