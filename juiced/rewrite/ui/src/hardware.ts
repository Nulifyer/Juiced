export type Gpu = { vendor: string; name: string | null; vram_total_bytes: number | null; vram_free_bytes: number | null; gtt_total_bytes: number | null; integrated: boolean | null; compute_capability: string | null; gfx_target: string | null; family: string | null; driver_version: string | null; render_node_access: string | null };
export type Npu = { vendor: string; name: string | null; family: string | null; access: string | null };
export type BackendRow = { recipe: string; display_name: string; backend: string; tier: string; format: string; state: "available" | "unavailable" | "unknown"; reasons: { code: string; message: string }[]; installed: boolean | null; recommended: boolean };
export type SystemInfo = {
  host: { os: string | null; architecture: string | null; detection: string | null };
  cpu: { name: string | null; physical_cores: number | null; logical_threads: number | null; isa: string[] } | null;
  memory: { total_bytes: number | null; available_bytes: number | null } | null;
  gpus: Gpu[]; npus: Npu[];
  vulkan: { state: string | null; devices: string[] };
  backends: BackendRow[];
};

const text = (value: unknown) => typeof value === "string" ? value : null;
const number = (value: unknown) => typeof value === "number" && Number.isFinite(value) ? value : null;
function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The service returned invalid hardware information.");
  return value as Record<string, unknown>;
}
const list = (value: unknown) => Array.isArray(value) ? value.slice(0, 256).map(record) : [];

export function parseSystemInfo(value: unknown): SystemInfo {
  const item = record(value), host = record(item.host ?? {}), devices = record(item.devices ?? {});
  const cpu = devices.cpu ? record(devices.cpu) : null, memory = devices.memory ? record(devices.memory) : null, vulkan = record(devices.vulkan ?? {});
  return {
    host: { os: text(host.os), architecture: text(host.architecture), detection: text(host.detection) },
    cpu: cpu && { name: text(cpu.name), physical_cores: number(cpu.physical_cores), logical_threads: number(cpu.logical_threads), isa: Array.isArray(cpu.isa) ? cpu.isa.filter((entry): entry is string => typeof entry === "string").slice(0, 32) : [] },
    memory: memory && { total_bytes: number(memory.total_bytes), available_bytes: number(memory.available_bytes) },
    gpus: list(devices.gpus).map((gpu) => ({ vendor: text(gpu.vendor) ?? "unknown", name: text(gpu.name), vram_total_bytes: number(gpu.vram_total_bytes), vram_free_bytes: number(gpu.vram_free_bytes), gtt_total_bytes: number(gpu.gtt_total_bytes), integrated: typeof gpu.integrated === "boolean" ? gpu.integrated : null, compute_capability: text(gpu.compute_capability), gfx_target: text(gpu.gfx_target), family: text(gpu.family), driver_version: text(gpu.driver_version), render_node_access: text(gpu.render_node_access) })),
    npus: list(devices.npus).map((npu) => ({ vendor: text(npu.vendor) ?? "unknown", name: text(npu.name), family: text(npu.family), access: text(npu.access) })),
    vulkan: { state: text(vulkan.state), devices: list(vulkan.devices).map((device) => text(device.name) ?? "Unknown device") },
    backends: list(item.backends).map((row) => ({
      recipe: text(row.recipe) ?? "", display_name: text(row.display_name) ?? text(row.recipe) ?? "", backend: text(row.backend) ?? "", tier: text(row.tier) ?? "", format: text(row.format) ?? "",
      state: row.state === "available" || row.state === "unavailable" ? row.state : "unknown",
      reasons: list(row.reasons).map((reason) => ({ code: text(reason.code) ?? "", message: text(reason.message) ?? "" })),
      installed: typeof row.installed === "boolean" ? row.installed : null, recommended: row.recommended === true,
    })),
  };
}

export const backendNames: Record<string, string> = { cpu: "CPU", vulkan: "Vulkan", cuda: "CUDA", rocm: "ROCm", "rocm-stable": "ROCm", metal: "Metal", npu: "NPU", "hybrid-rocm": "NPU + ROCm", system: "System install", hrx: "HRX" };

/** Memory the GPU can hold weights in: an integrated GPU also maps shared system memory (GTT). */
export function gpuMemory(gpu: Pick<Gpu, "vram_total_bytes" | "gtt_total_bytes" | "integrated">): number | null {
  const shared = gpu.integrated ? gpu.gtt_total_bytes ?? 0 : 0;
  const total = (gpu.vram_total_bytes ?? 0) + shared;
  return total > 0 ? total : null;
}
