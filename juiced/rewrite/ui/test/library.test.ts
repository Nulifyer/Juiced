import { expect, test } from "bun:test";
import { parseSearch } from "../src/libraryData";

test("checkpoint variants keep their engine, member files and hub credential state", () => {
  const result = parseSearch({
    artifact_generation: 4, offline: false,
    sources: { huggingface: { status: "ok", authenticated: true } },
    data: [{ repo: "Qwen/Qwen3-0.6B", name: "Qwen3-0.6B", gated: false, quants: [{
      label: "Checkpoint", kind: "checkpoint", format: "transformers", engine: "vllm", file: "config.json", parts: 1, bytes: 1519182365, pullable: true,
      files: [{ file: "config.json", bytes: 726, member: "config.json" }, { file: "model.safetensors", bytes: 1503300328, member: "model.safetensors" }],
      local: { present: true, present_files: 2, set: "set-abc" },
    }, { label: "Q4_K_M", file: "m-Q4_K_M.gguf", bytes: 10 }] }],
  });
  expect(result.authenticated).toBe(true);
  const [checkpoint, quant] = result.models[0]!.quants;
  expect(checkpoint).toMatchObject({ kind: "checkpoint", engine: "vllm", set: "set-abc", present: true });
  expect(checkpoint!.files.map((file) => file.member)).toEqual(["config.json", "model.safetensors"]);
  expect(quant).toMatchObject({ kind: "quant", engine: null, files: [] });
});
