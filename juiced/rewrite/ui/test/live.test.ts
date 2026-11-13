import { describe, expect, test } from "bun:test";
import { parseSystemStats } from "../src/live";

const GiB = 1024 ** 3;

describe("parseSystemStats", () => {
  test("an integrated GPU counts its shared system memory with the dedicated carve-out", () => {
    const stats = parseSystemStats({ gpus: [{ id: "amdgpu:0000:c7:00.0", vendor: "amd", integrated: true, memory_used_bytes: 2 * GiB, memory_total_bytes: 16 * GiB, shared_memory_used_bytes: 34 * GiB, shared_memory_total_bytes: 72 * GiB }] });
    expect(stats.gpus[0]).toMatchObject({ memory_used_bytes: 36 * GiB, memory_total_bytes: 88 * GiB, shared: { dedicated_total: 16 * GiB, shared_total: 72 * GiB } });
  });

  test("a discrete GPU keeps reporting only its own memory", () => {
    const stats = parseSystemStats({ gpus: [{ id: "amdgpu:0000:03:00.0", integrated: false, memory_used_bytes: GiB, memory_total_bytes: 16 * GiB, shared_memory_used_bytes: GiB, shared_memory_total_bytes: 32 * GiB }, { id: "nvidia:GPU-a", memory_used_bytes: GiB, memory_total_bytes: 16 * GiB }] });
    expect(stats.gpus.map((gpu) => [gpu.memory_total_bytes, gpu.shared])).toEqual([[16 * GiB, null], [16 * GiB, null]]);
  });
});
