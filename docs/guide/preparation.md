# Prepare another quant

Create a separate GGUF variant from supported local weights without overwriting
the source. You need enough free disk space for staging and output, and CPU
memory for the conversion.

## Review the input

Juiced offers these initial preparation routes:

| Input | Output | Requirement |
| --- | --- | --- |
| Compatible language, embedding, or reranking GGUF | Q4_K_M, Q5_K_M, Q6_K, or Q8_0 GGUF | Supported tensors and geometry |
| Supported Llama/Qwen floating-point HF snapshot | F16 intermediate, then a supported GGUF quant | Complete local safetensors, config, tokenizer, and a reviewed converter adapter |
| Existing compatible GGUF | Another llama.cpp hardware profile | Reuse directly, with no conversion |

A Hugging Face repository name is insufficient for local HF conversion.
The complete snapshot must already be registered and available to the server.

## Prepare the variant

1. Open the source model and select **Runtime**.
2. Find **Prepare a variant**.
3. Choose an offered target quant.
4. Review precision changes and any capability loss.
5. Acknowledge the plan and start preparation.
6. Watch the preparation job for progress or an error.
7. Enable the new Disabled variant and complete an inference test.

Converting multimodal HF input can require explicit text-only output. The plan
lists lost vision/audio support, and the derived model name includes `Text`.
Choose a publisher's GGUF and projector when you need those capabilities.

## Understand the tradeoffs

Reducing weight precision adds quantization error. Requantizing an already
quantized model adds another approximation. Increasing its output bit count
cannot restore information lost earlier. HF conversion passes through F16.

Structural validation checks the produced artifact. It does not establish answer
quality or hardware performance. Test the new variant with your own workload.
Generic AWQ, GPTQ, ONNX, MLX, and Q4NX round trips are not offered.

## Cancel or reclaim preparation

Cancel a pending job through its job controls. Cancellation stops the conversion
process and removes staging files. A server restart marks active jobs Interrupted.
A pending preparation can prevent source-model removal.

Open **Cache → Prepared copies** to inspect prepared storage and references.
Only unreferenced copies can be reclaimed. Remove an unwanted registration through
its model management flow before retrying reclamation.
