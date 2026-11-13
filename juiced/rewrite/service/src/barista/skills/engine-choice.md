---
topic: engine-choice
version: 2
title: Choose an engine and backend
summary: Match a model to an engine build this host can run, install it, and explain backends the hardware cannot use.
---
# Choose an engine and backend

1. `juiced_hardware`: devices, drivers, and per engine the backends with a build for this host (`here`) and those without (`not_here`).
2. `juiced_list_engines` with `engine` for one engine: what it runs (`uses`), availability, download and disk size, the recommended backend, `why_not` for builds this host cannot run, and which models use each installed build.
3. Match the model format: GGUF runs on llama.cpp (DeepSeek V4 GGUFs on ds4, Radeon HRX builds on llama.cpp hrx); Transformers checkpoint folders on vllm; FLM checkpoints on fastflowlm (NPU); ONNX classifiers on onnxruntime. Speech, music (acestep.cpp), sound effects (thinksound.cpp, openmoss), images (sd.cpp, thenoise) and 3D (trellis.cpp) each have their own engine.
4. Pick the recommended backend unless the operator asked for another. Typical order: CUDA or ROCm on a supported GPU, then Vulkan, then CPU.
5. `juiced_install_engine` with `preview: true`: check the download, disk use and free space. vLLM is a Python environment of about 10 GB and its first load compiles kernels for several minutes; say so. Apply, then `juiced_wait_job`.
6. A model picks its engine when added: `juiced_add_model` with `backend`, and `engine` when several engines read the same format.

A backend this hardware cannot run is not an error to fix: say it is unavailable here and why, and offer the backend that is.
