# Model runtime profiles and preparation

Open a downloaded model and select **Runtime**. Choose a runtime provider, then
its hardware variant. The server distinguishes file compatibility from package
and hardware availability. Unavailable choices remain inspectable and explain
their restriction. Experimental choices require review; missing benchmarks alone
do not prevent a structurally compatible trial.

**Use this profile** saves an unloaded model's next execution target.
**Apply & reload** switches a loaded model while preserving its loading policy.
Busy models refuse the switch. If loading fails, Juiced restores the previous
profile and attempts to reload it. Clearing a saved override returns to the
profile’s defaults; inactive profile snapshots do not override current edits. An interrupted trial is recovered before
startup loading; a failed recovery blocks automatic retry until the operator
selects a profile. A trial failure does not trigger the legacy
fallback that unloads unrelated workers. Settings belong to each profile; the
model's public name, downloaded files and consumer endpoint remain stable.

CPU, Vulkan, ROCm, CUDA and Metal llama.cpp profiles reuse the same compatible
GGUF. GPU availability still depends on the actual host, container permissions
and installed runtime. A working GPU profile is not a promise that another
runtime implements the same architecture, tokenizer or optional features.

## Local preparation

**Prepare a variant** creates a separate model with its proper target quant name.
New variants start Disabled. Enable them explicitly through loading policy.
Supported initial routes are:

| Input | Output | Conditions |
| --- | --- | --- |
| Llama/Qwen original floating-point HF snapshot | F16 intermediate → Q4_K_M/Q5_K_M/Q6_K/Q8_0 GGUF | Complete local safetensors, config and tokenizer; reviewed architecture adapter |
| Compatible language/embedding/reranking GGUF | Q4_K_M/Q5_K_M/Q6_K/Q8_0 GGUF | Tensor names/shapes must remain identical |
| Existing GGUF | Another llama.cpp hardware profile | Reuse directly; no preparation needed |

Preparation never rewrites the source. Reducing bit depth adds quantization
error; requantizing an already quantized source adds another approximation.
Increasing the output bit count cannot recover discarded source precision.
Floating-point input passes through F16. Changes are presented before starting.
The worker refuses unhandled tensor omissions rather than quietly dropping them.
Multimodal HF snapshots offer an explicit text-only conversion: the plan lists
vision/audio loss and the derived name contains Text. A publisher GGUF with its
companion projector retains those capabilities. Source MTP tensors must produce
MTP metadata in the output, or preparation fails.

The native manager supervises an offline CPU conversion subprocess. Its pinned
Python environment, converter and CPU quantizer are built separately from the
runtime image. No serving request runs in that Python process. No compiler, Git,
package installer or GPU-enabled PyTorch is needed in the running container.

Jobs expose hashing, converting, quantizing, validating and terminal phases,
bounded converter output, errors and cancellation. Cancelling terminates the
converter process group and removes staging files. Restarted active jobs become
Interrupted. A source model cannot be removed while its preparation is pending.

Cache keys include source/companion content hashes, recipe, target quant and tool
identity. A cached artifact must pass its output checksum and tensor/data-range
checks before reuse. Staged files are published atomically; structural validation
does not claim inference correctness or output quality. The first real request
remains a separate test. **Cache → Prepared copies** shows storage and references;
only unreferenced copies can be reclaimed. Registered copies remain protected.

## Runtime limits

The bundled vLLM importer exposes experimental GGUF profiles only for reviewed
Llama/Qwen2/Qwen3 architectures without vision companions, and requires the
original tokenizer/config repository or directory. It consumes the same local
GGUF. The pinned vLLM package does not support Ornith's qwen35moe GGUF importer;
installing an unrelated newer plugin is not a valid engine/plugin qualification.
That choice explains its restriction rather than pretending conversion solves it.

Generic AWQ/GPTQ/ONNX/MLX/Q4NX round trips are not offered. Their tensor layouts,
quantizers, calibration and model adapters differ. In particular, producing a
Q4NX file is separate from registering a custom model with a compatible FLM
engine. The upstream converter's geometry limits and possible MTP omissions need
an explicit adapter and a matched custom-model loading contract before that
route can become a usable execution profile. Original source weights remain the
best input for future target-specific preparation.

**Find a prepared version** opens model search with the model name and intended
runtime. GGUF search can filter on that format; other runtime candidates require
file inspection on the publisher site; the GGUF picker does not pretend to
import arbitrary runtime packages. HF conversion needs an already complete,
registered local snapshot. A repository name is not proof of backend compatibility or
publisher equivalence. Juiced does not maintain an exhaustive equivalence map.

## XDNA2 + ROCm

The experimental llama.cpp hybrid variant uses a matched llama.cpp/GGML/XDNA
plugin bundle. It requires Linux, a gfx1150 Radeon GPU and an XDNA2 NPU together.
The provider descriptor declares this conjunction; having either device alone
is insufficient. The worker verifies actual NPU startup and selected GPU access.
The default native profile never automatically selects an experimental backend.

The same GGUF stays loaded in the shared GPU graph. Eligible prefill matrices can
run on the NPU with reduced-precision arithmetic. Routed experts, attention,
recurrent state and decode remain on ROCm. This is partial prefill acceleration,
not whole-model NPU inference or request routing. It does not promise a speed gain.

The Runtime panel exposes additional NPU-copy budget (default2048 MiB) and minimum
prefill rows (default1024). These bound copied weights, not all XRT working
memory. Keep headroom for the GPU model, context and other Hot models. A pinned
or busy NPU worker blocks a hybrid trial until the operator releases it.
**Current worker** shows actual NPU matmuls, prefill plans and weight-copy use.
Refresh it after a request; selecting the profile alone is not evidence of NPU
work. The vision projector stays on the selected GPU, and MTP prompt batches
remain eligible for prefill while verification/decode stay on the GPU.

The bundle's userspace libraries are private. Host kernel drivers and firmware
remain host-owned. Device passthrough is `/dev/dri`, `/dev/kfd` and
`/dev/accel/accel0`, with the host render/video groups and sufficient memlock.
GGML headers, plugin and runtime libraries must come from the same build. Never
attach the plugin to an arbitrary installed llama.cpp executable.

## Adding a runtime

Use the existing BackendDescriptor/BackendOps/WrappedServer registration.
`BackendOps::execution_compatibility` owns artifact/architecture/feature import
rules for each variant. The manager combines that result with hardware/package
inventory and persists profiles; the UI does not invent support from filenames.
`BackendSupport::requires_all_devices` declares hybrid device conjunctions.
Variant-specific `effective_device` and `effective_slot_policy` participate in
router admission before loading. Native backend installation owns pinned assets,
checksums and process startup. Preparation recipes own offline transformations;
the native model registry remains the sole model/weight identity owner.

Execution/preparation management APIs use all four standard URL prefixes and the
server's existing authentication. `GET/POST juiced/execution` plans/selects;
`GET/POST juiced/preparation` plans, queues, inspects/cancels jobs and manages
prepared-copy cache. Downstream OpenAI/Ollama/Anthropic/Wyoming connections retain
their existing endpoints and model names.
