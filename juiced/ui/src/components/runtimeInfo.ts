import { useCallback, useEffect, useState } from "react";
import { api } from "../api";

export type RuntimeInfo = {
  display_name: string;
  modality: string;
  default_backend?: string;
  options?: { name: string; type_name: string; help: string }[];
  backends: Record<
    string,
    {
      state: string;
      message?: string;
      devices?: string[];
      version?: string;
      action?: string;
      update_available?: boolean;
    }
  >;
};
export type SystemInfo = {
  recipes: Record<string, RuntimeInfo>;
  devices: Record<string, any>;
};
export const runtimeName = (id: string, info?: RuntimeInfo) =>
  id === "llamacpp"
    ? "llama.cpp"
    : id === "vllm"
      ? "vLLM"
      : info?.display_name || id;
export const acceleratorName = (name: string) =>
  ({
    cpu: "CPU",
    vulkan: "GPU · Vulkan",
    rocm: "GPU · ROCm",
    cuda: "GPU · CUDA",
    metal: "GPU · Metal",
    npu: "NPU",
    system: "System runtime",
    "hybrid-rocm": "Hybrid NPU + ROCm",
  })[name] || name;
export const runtimeHelp = (recipe: string) =>
  ({
    llamacpp: "llama.cpp runs GGUF weights on CPU or supported accelerators. Hardware choices reuse the same compatible file.",
    vllm: "vLLM serves language models with GPU batching. Its GGUF importer is experimental and supports fewer architectures than llama.cpp.",
    flm: "FLM is AMD's NPU runtime. It needs supported hardware and its own compatible model package; arbitrary GGUF files cannot run directly.",
    whispercpp: "Whisper.cpp runs supported Whisper speech-recognition models on CPU or supported GPUs.",
    "sd-cpp": "Stable Diffusion.cpp runs supported image-generation models. Model architecture and weight format must match the installed engine.",
    kokoro: "Kokoro generates speech with its supported voice and language assets.",
    asr4all: "ASR4ALL runs the supported FUTO speech-recognition model packages on CPU.",
  })[recipe] || `${runtimeName(recipe)} runs its supported model packages. Compatibility also depends on the installed engine and hardware.`;
export const acceleratorHelp = (backend: string) =>
  ({
    cpu: "Runs on the processor using system RAM. No GPU or NPU is required; large models are usually slower.",
    vulkan: "Uses the Vulkan GPU driver. Works across supported GPU vendors and reuses compatible GGUF weights.",
    rocm: "Uses AMD's ROCm GPU runtime. Requires a supported AMD GPU, compatible userspace libraries and GPU device access.",
    cuda: "Uses NVIDIA's CUDA GPU runtime. Requires a supported NVIDIA GPU and compatible host driver/container access.",
    metal: "Uses Apple's Metal GPU runtime on supported macOS hardware.",
    npu: "Uses the neural processor through a supported runtime. NPU model formats and operators are specific to that runtime.",
    system: "Uses the runtime's own device selection. Check its model and hardware requirements before loading.",
    "hybrid-rocm": "Uses XDNA2 for eligible prefill matrix work and ROCm for the rest of the same model, including decode. Reuses the GGUF; adds approximate NPU arithmetic and weight copies. Speed gains are not guaranteed.",
  })[backend] || "Uses the hardware selected by this model's saved runtime settings.";
export function useSystemInfo() {
  const [info, setInfo] = useState<SystemInfo>();
  const [error, setError] = useState("");
  const [revision, setRevision] = useState(0);
  const refresh = useCallback(() => setRevision((value) => value + 1), []);
  useEffect(() => {
    const controller = new AbortController();
    setError("");
    void api<SystemInfo>("system-info", undefined, controller.signal)
      .then(setInfo)
      .catch((e) => {
        if (!controller.signal.aborted) setError(e.message);
      });
    return () => controller.abort();
  }, [revision]);
  return { info, error, refresh };
}
