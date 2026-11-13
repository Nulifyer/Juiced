# Choose runtimes and hardware

Select an execution profile for a downloaded model. A profile identifies the
engine, hardware variant, and settings used to run its weights.

## Inspect the host

1. Open **Engines & hardware**.
2. Check the detected devices and their access status.
3. Find the engine and inspect its hardware variants.
4. Install a required package through its available action.
5. Use the top-bar download button to open Library activity if installation queues a transfer.

A detected device, an installed package, and a compatible model are separate
requirements. Complete an inference test after all three are available.

## Text engines

| Engine | Backends | Runs | Needs |
|--------|----------|------|-------|
| llama.cpp | CPU, Vulkan, CUDA, ROCm | GGUF | The matching GPU and driver |
| llama.cpp HRX | HRX | GGUF | Radeon gfx1100 or gfx1151 (Linux) |
| llama.cpp hybrid | XDNA2 + ROCm | GGUF | gfx1150 iGPU and XDNA2 NPU |
| vLLM | CUDA, ROCm | Transformers checkpoint folders (safetensors) | NVIDIA sm_75 or newer; Radeon gfx110X, gfx1150, gfx1151, gfx120X |
| DwarfStar4 | ROCm | Its own DeepSeek V4 GGUFs | Radeon gfx1151 |
| FastFlowLM | NPU | FLM checkpoints | XDNA2 NPU |
| ONNX Runtime | CPU | Exported encoder classifiers | Any x86_64 or arm64 CPU |

**Engines & hardware** lists every backend. A backend this host cannot run shows
the reason, such as a missing GPU or an unsupported GPU target. Ryzen AI LLM and
Metal builds exist only for Windows and macOS, so the container shows them as
unsupported.

Engines install into the data volume, never into the image. vLLM installs as a
Python environment of about 10 GB and needs that much free space before the
download starts. Its CUDA build comes from hash-pinned wheels. Its ROCm build is
the portable environment the native release used.

## Run an original checkpoint on vLLM

1. Install **vLLM** on its CUDA or ROCm backend.
2. In **Models**, add a local folder that holds `config.json` and the
   `.safetensors` files, for example a Hugging Face cache snapshot under an
   import root. Juiced registers the folder in place.
3. Load the model. The first start compiles kernels and can take several
   minutes; later starts reuse the compile cache.

Juiced sizes the KV cache for the registered context and caps
`--gpu-memory-utilization` to its estimate. Requests that ask for more output
tokens than the context leaves are reduced to fit.

## Classify text

Add a folder with `model.onnx`, `tokenizer.json` and `config.json` (an
`optimum-cli export onnx` classifier) with ONNX Runtime installed. Send
`POST /v1/classify` with `model` and `input`; the reply lists label scores.

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
vLLM runs original checkpoint folders, not GGUF. DwarfStar4 runs only its own
GGUFs; choose it with the `engine` field when you add one. An unavailable
profile explains its restriction.

Use **Find a prepared version** to search for another publisher artifact.
Use [published variants](variants.md) when another format or quant is needed.

## Speech, audio, image and 3D engines

Install these from **Engines & hardware**. Juiced picks the build for your
hardware and lists builds your machine can't run, with the reason, under
**Show all**. Each engine downloads a pinned release that is checked against its
published SHA-256.

| Engine | Used for | Linux builds | Notes |
| --- | --- | --- | --- |
| whisper.cpp | Speech to text | CPU, Vulkan, ROCm | NPU and Metal builds need Windows or macOS. |
| Parakeet | Speech to text | CPU | |
| Moonshine | Speech to text | CPU | |
| FUTO transcribe | Speech to text | CPU | English only. |
| Kokoro | Text to speech | CPU | Metal needs macOS. |
| OpenMOSS | Text to speech, sound effects | CUDA, Vulkan | Needs a GPU with 16 GB for MOSS-TTS. |
| ACE-Step | Music | CUDA, Vulkan | |
| ThinkSound | Sound effects | CUDA, Vulkan | |
| sd.cpp | Images | CPU, Vulkan, CUDA, ROCm | |
| TheNoise | Images | ROCm (gfx1150, gfx1151, gfx1152) | |
| TRELLIS | Image to 3D mesh | CUDA, Vulkan, ROCm (gfx1150) | |

- **CUDA builds** of OpenMOSS, ACE-Step, ThinkSound and TRELLIS bundle CUDA 13, so
  they need NVIDIA driver 580 or newer. TRELLIS needs an RTX 30-series GPU or newer.
  The OpenMOSS CUDA build also needs a CPU with AVX-512; on other CPUs Juiced
  offers only its Vulkan build.
- **AMD GPUs** run OpenMOSS, ACE-Step and ThinkSound with their Vulkan builds. Their
  ROCm releases are built for other GPU targets against a ROCm version Juiced
  doesn't pin.
- **TheNoise** bundles Python, PyTorch and ROCm, so its install is about 1.8 GB to
  download and 6 GB on disk. Its first image after loading is slow while PyTorch
  compiles kernels. The gfx103X, gfx110X and gfx120X bundles are not offered,
  because they ship as split archives.

### Add a generation model

1. Open **Models**, then **Discover**, and pick the **Audio** or **3D** task.
2. Download the model. ACE-Step, ThinkSound, TRELLIS and OpenMOSS models come as a
   set of files, and Juiced downloads all of them.
3. Select **Add model**. If its engine is missing, select **Install engine and add**.
4. Try it in **Test services**: **Audio** makes a clip from a prompt and optional
   lyrics, and **3D** turns an object photo into a `.glb` file.

ACE-Step 1.5 Turbo is the smallest music set (about 3.3 GB). TRELLIS.2 Q4 is the
smallest 3D set (about 6.5 GB with BiRefNet). Generation models load only when a
request arrives if their policy is On demand, and loading can take a minute.

### Parakeet models from an earlier Juiced

If your earlier installation had a Parakeet model, it appears under **Models** as
Disabled and waiting for its engine. Install **Parakeet · CPU** in **Engines &
hardware**. Juiced then registers the model, still Disabled; enable it in
**Loading policy**.

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
