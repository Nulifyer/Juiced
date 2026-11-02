import { expect, test } from "bun:test";
import {
  groupRepositories,
  quantization,
  modelQuant,
  variantName,
  existingVariant,
} from "../src/components/modelDiscovery";
test("quants group under the publisher's exact model, preserving tune and version", () => {
  const repos = [
    "JetBrains/Mellum2-12B-A2.5B-Instruct-GGUF-Q8_0",
    "JetBrains/Mellum2-12B-A2.5B-Instruct-GGUF-Q4_K_M",
    "Other/Mellum2-12B-A2.5B-Instruct-GGUF-Q4_K_M",
    "JetBrains/Mellum2-12B-A2.5B-Thinking-GGUF-Q4_K_M",
    "JetBrains/Mellum2.1-12B-A2.5B-Thinking-GGUF",
  ];
  const groups = groupRepositories(
    repos.map((repository_id) => ({ repository_id })),
  );
  expect(groups).toHaveLength(4);
  expect(groups[0]?.repositories).toHaveLength(2);
  expect(groups[0]?.name).toBe("Mellum2-12B-A2.5B-Instruct");
  expect(quantization("Model-GGUF-MXFP4_MOE")).toBe("MXFP4_MOE");
  expect(quantization("Model-2.5B-Instruct")).toBeUndefined();
  expect(quantization("Model-F32-GGUF")).toBe("F32");
});
test("different quants receive distinct proper names and match stored checkpoint identity", () => {
  const base = "Ornith-1.5-35B-A3B-GGUF",
    repo = "ornith-ai/" + base;
  expect(variantName(base, "Q6_K")).toBe(base + "-Q6_K");
  expect(variantName(base + "-Q6_K", "Q6_K")).toBe(base + "-Q6_K");
  expect(variantName(base + "-Q4_K_M", "Q6_K")).toBe(base + "-Q6_K");
  const q4 = {
    id: base,
    recipe: "llamacpp",
    checkpoints: { main: repo + ":Q4_K_M", mmproj: repo + ":vision.gguf" },
  };
  expect(modelQuant(q4)).toBe("Q4_K_M");
  expect(
    existingVariant([q4], repo, { name: "Q6_K" }, "llamacpp", "vision.gguf"),
  ).toBeUndefined();
  expect(
    existingVariant([q4], repo, { name: "Q4_K_M" }, "llamacpp", "vision.gguf"),
  ).toBe(q4);
  expect(
    existingVariant([q4], repo, { name: "Q4_K_M" }, "llamacpp", ""),
  ).toBeUndefined();
  expect(
    existingVariant([q4], repo, { name: "Q4_K_M" }, "vllm", "vision.gguf"),
  ).toBeUndefined();
  const q6 = {
    recipe: "llamacpp",
    checkpoints: { main: repo + ":Ornith-Q6_K.gguf" },
  };
  expect(
    existingVariant(
      [q6],
      repo,
      { name: "Q6_K", primary_file: "Ornith-Q6_K.gguf" },
      "llamacpp",
      "",
    ),
  ).toBe(q6);
});
test("explicit base metadata groups generic and quant-specific repositories without merging publishers", () => {
  const tags = ["base_model:quantized:Author/Model-v2"];
  const groups = groupRepositories([
    { repository_id: "Quantizer/Model-v2-GGUF", tags },
    { repository_id: "Quantizer/Model-v2-Q6_K", tags },
    { repository_id: "Other/Model-v2-Q6_K", tags },
  ]);
  expect(groups).toHaveLength(2);
  expect(groups[0]?.repositories).toHaveLength(2);
  expect(groups[0]?.name).toBe("Model-v2");
});
