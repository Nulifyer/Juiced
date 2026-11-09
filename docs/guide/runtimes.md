# Choose runtimes and hardware

Select an execution profile for a downloaded model. A profile identifies the
engine, hardware variant, and settings used to run its weights.

## Inspect the host

1. Open **Backends & hardware**.
2. Check the detected devices and their access status.
3. Find the engine and inspect its hardware variants.
4. Install a required package through its available action.
5. Use the top-bar download button to open Library activity if installation queues a transfer.

A detected device, an installed package, and a compatible model are separate
requirements. Complete an inference test after all three are available.

## Select a profile

1. Open the model and select **Runtime**.
2. Choose the runtime provider.
3. Choose a hardware variant under **Run on**.
4. Select an accelerator device if the profile offers that control.
5. Review compatibility information and any experimental notice.
6. Select **Use this profile**, or **Apply & reload** for a loaded model.
7. Wait for the operation to settle, then test a request.

Busy models refuse a profile switch. If the new profile fails to load, Juiced
restores the previous profile and attempts to reload it. Inspect the operation
error if recovery cannot complete.

## Reuse a GGUF

Compatible llama.cpp CPU, Vulkan, ROCm, CUDA, and Metal profiles can use the same
GGUF without conversion. The host and runtime must support the selected variant.
Metal needs native macOS execution, outside the Linux container.

Other engines need their own architecture, tokenizer, and artifact support.
A successful llama.cpp load does not qualify the same file for vLLM or FastFlowLM.
The bundled vLLM GGUF importer has restricted architecture support and needs the
original tokenizer/config source. An unavailable profile explains its restriction.

Use **Find a prepared version** to search for another publisher artifact.
Use [published variants](variants.md) when another format or quant is needed.

## Try experimental XDNA2 + ROCm

This profile requires Linux, a gfx1150 Radeon GPU, an XDNA2 NPU, and a matched
llama.cpp/GGML/XDNA runtime bundle. Both accelerators must be accessible.
Map `/dev/dri`, `/dev/kfd`, and `/dev/accel/accel0` into the container, with the
required host groups and sufficient memlock. Host drivers and firmware remain
host-managed.

1. Release any conflicting NPU worker.
2. Choose the experimental hybrid profile.
3. Review the NPU copy budget and prefill settings.
4. Apply the profile and run a representative prompt.
5. Refresh **Current worker** and inspect actual NPU activity.
6. Compare latency with the previous profile before keeping the change.

Eligible prefill matrices can run on the NPU. Attention, recurrent state, routed
experts, and decode remain on ROCm. This partial acceleration does not promise
a speed gain. A selected profile alone does not prove that the NPU did work.
Do not mix the plugin with libraries from another llama.cpp build.

For tuning within a profile, see [model settings](settings.md).
