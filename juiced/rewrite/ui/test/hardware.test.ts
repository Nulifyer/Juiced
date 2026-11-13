import { expect, test } from "bun:test";
import { gpuMemory } from "../src/hardware";

const GiB = 1024 ** 3;

test("an integrated GPU counts the shared memory it maps; a dedicated one only its VRAM", () => {
  expect(gpuMemory({ vram_total_bytes: 16 * GiB, gtt_total_bytes: 72 * GiB, integrated: true })).toBe(88 * GiB);
  expect(gpuMemory({ vram_total_bytes: 16 * GiB, gtt_total_bytes: 64 * GiB, integrated: false })).toBe(16 * GiB);
  expect(gpuMemory({ vram_total_bytes: null, gtt_total_bytes: null, integrated: null })).toBeNull();
});
