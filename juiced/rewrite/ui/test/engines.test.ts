import { describe, expect, test } from "bun:test";
import { downloadSize, engineGroups, parseEngines, shownByDefault } from "../src/engines";
import { parseSystemInfo } from "../src/hardware";

const engines = parseEngines({
  generation: 3,
  installable: [
    { engine: "llama.cpp", backend: "cpu", availability: "available", recommended: false, installed_package: null, reasons: [], build: { archive_bytes: 22886629 } },
    { engine: "llama.cpp", backend: "rocm", availability: "available", recommended: true, installed_package: "package-rocm", reasons: [], build: { archive_bytes: 287482076, runtime: { archive_bytes: 1972096149 } } },
    { engine: "llama.cpp", backend: "cuda", availability: "unavailable", recommended: false, installed_package: null, reasons: [{ message: "No NVIDIA GPU detected." }], build: null },
    { engine: "whisper.cpp", backend: "vulkan", availability: "available", recommended: true, installed_package: null, reasons: [], build: { archive_bytes: 43770408 } },
    { engine: "fastflowlm", backend: "npu", availability: "unavailable", recommended: false, installed_package: null, reasons: [{ message: "No AMD NPU detected." }], build: { archive_bytes: 45752536 } },
  ],
  installed: [
    { package: "package-rocm", engine: "llama.cpp", backend: null, release: { backend: "rocm" }, models: [{ model: "a" }, { model: "b" }] },
    { package: "package-local", engine: "llama.cpp", backend: null, release: null, models: [{ model: "c" }] },
  ],
  jobs: [{ id: "job-1", engine: "whisper.cpp", backend: "vulkan", state: "downloading", downloaded_bytes: 10, total_bytes: 100 }],
});
const backends = parseSystemInfo({ devices: {}, backends: [
  { recipe: "llamacpp", display_name: "Llama.cpp GPU", backend: "metal", state: "unavailable", reasons: [{ code: "os", message: "Requires macOS." }] },
  { recipe: "llamacpp", display_name: "Llama.cpp GPU", backend: "system", state: "unknown", reasons: [] },
  { recipe: "vllm", display_name: "vLLM ROCm", backend: "rocm", tier: "experimental", state: "available", reasons: [] },
] }).backends;

describe("engine groups", () => {
  const groups = engineGroups(engines, backends);
  const llama = groups.find((group) => group.key === "llama.cpp")!;
  test("download sizes include the bundled runtime and pick a readable unit", () => {
    expect(engines.installable[1]!.archive_bytes).toBe(287482076 + 1972096149);
    expect(engines.installable[0]!.archive_bytes).toBe(22886629);
    expect([2727511, 22886629, 132320778, 1069727615, 287482076 + 1972096149, 4000, null].map(downloadSize)).toEqual(["2.6 MiB", "21.8 MiB", "126 MiB", "1.0 GiB", "2.1 GiB", "4 KiB", ""]);
  });
  test("installed builds lead, carry model counts and keep the recommended mark", () => {
    expect(llama.chips.map((chip) => [chip.label, chip.state])).toEqual([["ROCm", "installed"], ["Imported", "installed"], ["CPU", "installable"], ["CUDA", "unavailable"], ["Metal", "unavailable"]]);
    expect(llama.chips[0]).toMatchObject({ recommended: true, models: 2 });
    expect(llama.chips[1]!.models).toBe(1);
    expect(llama.chips.find((chip) => chip.backend === "metal")!.reason).toBe("Requires macOS.");
  });
  test("active jobs show as installing and builds that can't install here stay hidden by default", () => {
    expect(groups.find((group) => group.key === "whisper.cpp")!.chips[0]!.state).toBe("installing");
    expect(groups.find((group) => group.key === "fastflowlm")!.chips.some(shownByDefault)).toBe(false);
    const vllm = groups.find((group) => group.key === "recipe:vllm")!;
    expect(vllm).toMatchObject({ installable: false, name: "vLLM ROCm", purpose: "No installer yet" });
    expect(vllm.chips[0]!.state).toBe("no-installer");
    expect(groups.at(-1)!.key).toBe("recipe:vllm");
    expect(groups.flatMap((group) => group.chips).some((chip) => chip.backend === "system")).toBe(false);
  });
});
