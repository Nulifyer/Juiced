import { expect, test } from "bun:test";
import {
  groupRepositories,
  quantization,
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
