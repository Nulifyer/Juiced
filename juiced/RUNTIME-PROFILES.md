# Model runtime profiles and published variants

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

## Published variants

Juiced acquires and serves already published model variants. It does not convert,
quantize, requantize, repack, calibrate or export model weights. Select a compatible
published artifact instead of creating one locally. Runtime arithmetic and KV
cache precision are separate execution settings.

Open **Library** to find the desired model family and inspect repository, revision,
format, quant and required companion files. Keep the original registration when
acquiring another quant. A higher-bit filename does not prove its source or quality,
and a smaller download does not establish host fit. Check consuming-engine support,
whole weight allocation, context/state/workspace cost and current resource use.
See [choosing a published variant](../docs/guide/variants.md).

Existing copies created by earlier Juiced versions remain registered and usable.
**Cache > Retained weights** lists those artifacts and their references. Reclaim
only unreferenced copies through the native reference owner. Previous conversion
job records and bounded output remain inspectable, with cancellation/cleanup status.
An unconfirmed previous writer remains quarantined and blocks unsafe deletion;
requesting cancellation does not prove it stopped. No previous job is resumed.

## Runtime limits

The bundled vLLM importer exposes experimental GGUF profiles only for reviewed
Llama/Qwen2/Qwen3 architectures without vision companions, and requires the
original tokenizer/config repository or directory. It consumes the same local
GGUF. The pinned vLLM package does not support Ornith's qwen35moe GGUF importer;
installing an unrelated newer plugin is not a valid engine/plugin qualification.
That choice explains its restriction rather than pretending conversion solves it.

AWQ/GPTQ/ONNX/MLX/Q4NX and other published formats require a compatible consuming
provider and exact architecture contract. Similar names or an available package
are not proof of support. Juiced does not generate another format to work around
an importer limitation.

**Find a published variant** opens Library search with the source model and
requested runtime. File inspection is still required. The current GGUF acquisition
picker does not claim to import arbitrary runtime packages. The complete unified
Library suitability/acquisition flow remains P1 work.

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
checksums and process startup. The native model registry remains the sole
model/weight identity owner; Juiced has no model-weight producer.

Execution and retained-artifact management APIs use all four standard URL prefixes and the
server's existing authentication. `GET/POST juiced/execution` plans/selects;
`GET juiced/preparation` inspects retained jobs; `?id=` reads bounded output and
`?cache=1` lists retained artifacts. POST accepts only `cancel` and `delete_cache`.
Producer planning/creation/resume returns HTTP410 `feature_retired`. Downstream OpenAI/Ollama/Anthropic/Wyoming connections retain
their existing endpoints and model names.
