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
