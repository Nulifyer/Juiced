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
    { engine: "llama.cpp", backend: "hrx", availability: "unavailable", recommended: false, installed_package: null, reasons: [{ message: "No AMD GPU detected." }], build: { archive_bytes: 37305496 } },
    { engine: "vllm", backend: "cuda", availability: "available", recommended: false, installed_package: null, reasons: [], build: { archive_bytes: 16671812, environment: { installed_bytes: 12884901888 } } },
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
  { recipe: "ryzenai-llm", display_name: "Ryzen AI LLM", backend: "npu", tier: "core", state: "available", reasons: [] },
  { recipe: "llamacpp-hrx", display_name: "HRX GPU", backend: "hrx", tier: "experimental", state: "unavailable", reasons: [{ code: "no_amd_gpu", message: "No AMD GPU detected." }] },
] }).backends;

describe("engine groups", () => {
  const groups = engineGroups(engines, backends);
  const llama = groups.find((group) => group.key === "llama.cpp")!;
  test("text engines group under their engine, and Python environments show their installed size", () => {
    expect(groups.filter((group) => group.chips.some((chip) => chip.backend === "hrx")).map((group) => group.key)).toEqual(["llama.cpp"]);
    const vllm = groups.find((group) => group.key === "vllm")!;
    expect(vllm).toMatchObject({ name: "vLLM", purpose: "Chat with original checkpoints" });
    expect(vllm.chips.map((chip) => [chip.label, chip.state, chip.download_bytes])).toEqual([["CUDA", "installable", 12884901888], ["ROCm", "no-installer", null]]);
  });
  test("download sizes include the bundled runtime and pick a readable unit", () => {
    expect(engines.installable[1]!.archive_bytes).toBe(287482076 + 1972096149);
    expect(engines.installable[0]!.archive_bytes).toBe(22886629);
    expect([2727511, 22886629, 132320778, 1069727615, 287482076 + 1972096149, 4000, null].map(downloadSize)).toEqual(["2.6 MiB", "21.8 MiB", "126 MiB", "1.0 GiB", "2.1 GiB", "4 KiB", ""]);
  });
  test("installed builds lead, carry model counts and keep the recommended mark", () => {
    expect(llama.chips.map((chip) => [chip.label, chip.state])).toEqual([["ROCm", "installed"], ["Imported", "installed"], ["CPU", "installable"], ["CUDA", "unavailable"], ["HRX", "unavailable"], ["Metal", "unavailable"]]);
    expect(llama.chips[0]).toMatchObject({ recommended: true, models: 2 });
    expect(llama.chips[1]!.models).toBe(1);
    expect(llama.chips.find((chip) => chip.backend === "metal")!.reason).toBe("Requires macOS.");
  });
  test("active jobs show as installing and builds that can't install here stay hidden by default", () => {
    expect(groups.find((group) => group.key === "whisper.cpp")!.chips[0]!.state).toBe("installing");
    expect(groups.find((group) => group.key === "fastflowlm")!.chips.some(shownByDefault)).toBe(false);
    const ryzenai = groups.find((group) => group.key === "recipe:ryzenai-llm")!;
    expect(ryzenai).toMatchObject({ installable: false, name: "Ryzen AI LLM", purpose: "No installer yet" });
    expect(ryzenai.chips[0]!.state).toBe("no-installer");
    expect(groups.at(-1)!.key).toBe("recipe:ryzenai-llm");
    expect(groups.flatMap((group) => group.chips).some((chip) => chip.backend === "system")).toBe(false);
  });
});
