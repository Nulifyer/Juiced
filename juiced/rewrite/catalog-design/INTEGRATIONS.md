# Proposed runtime integration list

Generated from [inventory.json](inventory.json). Edit the inventory and run
`bun inventory.ts`. Use `bun inventory.ts --check` to verify it.

This is a planning list for the [catalog design](DESIGN.md). Entries and
candidate compute paths do not establish package availability, adapter support,
model compatibility, or hardware qualification. Upstream lifecycle is unknown
unless the inventory records an inspected maintenance/archive notice.

## Native recipes to preserve

Preserve the existing native recipe's selected product disposition and behavior. Native descriptors do not establish Rust or hardware qualification.

| Legacy recipe | Catalog identity | Native backend labels | Migration boundary |
| --- | --- | --- | --- |
| acestep | acestep-cpp | cuda, rocm, vulkan | Preserve fixed music pipeline and complete checkpoint set. |
| asr4all | futo-transcribe-cpp | cpu | Preserve FUTO GitLab origin, CPU worker and ASR4ALL model restriction. |
| ds4 | dwarfstar4 | rocm | Keep curated model requirements and selected Linux gfx1151 distribution. |
| flm | fastflowlm | npu | Resolve legacy npu to the exact FLM XDNA2 build; preserve its package/model contracts. |
| kokoro | kokoros | cpu, metal | Retain native Kokoros identity. Other Kokoro implementations are new profiles. |
| llamacpp | llama-cpp | cpu, cuda, hybrid-rocm, metal, rocm, system, vulkan | cpu/cuda/rocm/vulkan/metal map explicitly. system is installation provenance. hybrid-rocm requires its exact hybrid fork and placement profile. |
| llamacpp-hrx | llama-hrx | hrx | Keep separate fork/build identity and selected device restrictions. |
| moonshine | moonshine-voice | cpu | Keep existing worker/version contract; current upstream packages are not an automatic replacement. |
| onnxruntime | onnxruntime | cpu | Retain CPU worker. Additional execution providers require separate selected profiles. |
| openmoss | openmoss-cpp | cuda, vulkan | Preserve fixed synthesis and voice-design contracts. |
| parakeet | parakeet-redux | cpu | Keep the pinned mudler/parakeet.cpp CPU implementation, distinct from other Parakeet implementations. |
| ryzenai-llm | ryzenai-oga | npu | Keep exact vendor stack and prepared-model identity. No npu-to-rocm string substitution. |
| sd-cpp | stable-diffusion-cpp | cpu, cuda, metal, rocm, vulkan | Preserve native selected image operations and backend packages. |
| thenoise | thenoise | rocm | Preserve worker plus pinned Python/compiler dependencies and fixed image operations. |
| thinksound | thinksound-cpp | cuda, rocm, vulkan | Preserve selected fixed sound-generation service. |
| trellis | trellis-cpp | cuda, rocm, vulkan | Preserve selected 3D service disposition and its artifact/output contract. |
| vllm | vllm | rocm | Preserve existing ROCm distribution. New CUDA and plugin targets are additive qualification cells. |
| whispercpp | whisper-cpp | cpu, metal, npu, rocm, vulkan | Keep native backend pins. npu maps to the exact selected encoder-offload stack, not full-model NPU support. |

The llamacpp/hybrid-rocm profile additionally resolves to the
llama-xdna-hybrid distribution. The 18 legacy recipes therefore reference
19 catalog identities. All 44 native support rows, including tier, OS and
device restrictions, remain in the inventory snapshot.

Additional upstream compute paths within these projects are retained as
candidate_backends in the inventory. They require additive qualification.

## First new integrations

Recommended first new integrations after shared catalog and worker contracts; additive to existing required scope. Every target still needs its own release decision and evidence.

| Project / implementation | Catalog ID | Kind | Candidate compute paths | Boundary |
| --- | --- | --- | --- | --- |
| [ik_llama.cpp](https://github.com/ikawrakow/ik_llama.cpp) | ik-llama-cpp | engine | cpu, cuda | Independent fork identity and quantization coverage. Current upstream only calls CPU and CUDA fully functional and performant. |
| [MLX-LM](https://github.com/ml-explore/mlx-lm/blob/main/pyproject.toml) | mlx-lm | implementation | metal, cpu, cuda | First target Apple Silicon Metal. Linux CPU/CUDA packages remain independent follow-on cells. |
| [SGLang](https://github.com/sgl-project/sglang) | sglang | engine | cuda, hip, xpu, cpu, metal, tpu, cann, musa | Start one exact CUDA or ROCm text-serving target. Other accelerators and diffusion require separate operation suites. |
| [ExLlamaV3](https://github.com/turboderp-org/exllamav3) | exllamav3 | engine | cuda, hip | Use EXL3-specific artifact validation. CPU expert offload is a placement mode, not evidence for standalone CPU execution. TabbyAPI is a serving adapter choice. |
| [CTranslate2 / faster-whisper](https://github.com/SYSTRAN/faster-whisper) | ctranslate2 | engine | cpu, cuda | First adapter faster-whisper. Preserve CTranslate2 identity separately from the Python model implementation. Do not advertise ROCm/Metal. |
| [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) | sherpa-onnx | implementation | cpu, cuda, directml, qnn, rknpu, cann, openvino | Start CPU recognition/synthesis. Selected GPU/NPU builds and each additional audio operation require their own evidence. |
| [MLX-Audio](https://github.com/Blaizzy/mlx-audio) | mlx-audio | implementation | metal | First target selected Apple Silicon speech models. Share MLX bundle dependencies with MLX-LM where identical, but keep actual worker/accounting ownership. |
| [Piper](https://github.com/OHF-Voice/piper1-gpl/blob/main/docs/API_PYTHON.md) | piper | implementation | cpu, cuda | CPU-first TTS worker through ONNX Runtime, with optional CUDA. Track phonemizer, voice configuration and voice license companions. |
| [kokoro-onnx](https://github.com/thewh1teagle/kokoro-onnx) | kokoro-onnx | implementation | cpu, cuda | Alternative to Kokoros, not an alias. First qualify CPU. Additional ONNX providers require this implementation to expose and use them. |
| [Text Embeddings Inference](https://github.com/huggingface/text-embeddings-inference) | tei | implementation | cpu, cuda, metal, hip | CPU/CUDA first. Metal native execution and supported AMD Instinct ROCm packages have separate platform limits. |
| [OpenVINO GenAI](https://github.com/openvinotoolkit/openvino.genai) | openvino-genai | implementation | openvino | First target selected Intel CPU/GPU text models, then a separately qualified NPU profile. Do not infer every task from the underlying framework. |
| [Cactus runtime for Needle / Whistle / Mirage](https://github.com/cactus-compute/cactus) | cactus-small | engine | cpu, wasm | Start native CPU Needle tool proposals and Whistle recognition. Never execute returned tools in the worker adapter. No inherited cloud fallback. Distinct from cactus-engine. |

## Conditional integrations

Add only for a named workload/device gap and selected-provider acceptance.

| Project / implementation | Catalog ID | Kind | Candidate compute paths | Boundary |
| --- | --- | --- | --- | --- |
| [Handy transcribe.cpp](https://github.com/handy-computer/transcribe.cpp) | handy-transcribe-cpp | engine | cpu, cuda, vulkan, metal | Separate origin from FUTO transcribe.cpp. Choose when a concrete model/streaming requirement is not met by retained speech workers. |
| [Cactus Engine general inference SDK](https://github.com/cactus-compute/engine) | cactus-engine | engine | cpu, metal | Separate package and bundle identity from current cactus-compute. Disable automatic cloud handoff in managed local profiles. |
| [mistral.rs](https://github.com/EricLBuehler/mistral.rs) | mistral-rs | engine | cpu, cuda, metal | Select for a concrete model/operation gap. Keep engine libraries in a subprocess worker. |
| [Candle](https://github.com/huggingface/candle) | candle | framework | cpu, cuda, metal, wasm | Often a dependency of an implementation such as TEI. Add a standalone adapter only for a named workload. |
| [TensorRT-LLM](https://github.com/NVIDIA/TensorRT-LLM) | tensorrt-llm | engine | cuda, tensorrt | NVIDIA-specific. Acquire existing prepared models; runtime-owned compiled execution caches must not become model export/quantization tooling. |
| [LMDeploy](https://github.com/InternLM/lmdeploy) | lmdeploy | engine | cuda, cann | TurboMind and PyTorch engines are distinct implementation choices with different device and model coverage. |
| [KTransformers / KT-Kernel](https://github.com/kvcache-ai/ktransformers) | ktransformers | implementation | cpu, cuda, xpu, cann | Heterogeneous MoE expert placement, selected exact models. Can compose with SGLang, so do not automatically create a second serving process or reserve weights twice. |
| [MLC-LLM](https://github.com/mlc-ai/mlc-llm) | mlc-llm | engine | cuda, hip, vulkan, metal, opencl, webgpu, wasm | Use externally prepared target-compatible artifacts. Any required model producer stays outside Juiced's scope. |
| [bitnet.cpp](https://github.com/microsoft/BitNet) | bitnet-cpp | engine | cpu, cuda | Select compatible BitNet models and published artifacts. CPU implementation and GPU kernel paths are separately qualified. |
| [PyTorch / Transformers](https://github.com/huggingface/transformers/blob/main/docs/source/en/hardware.md) | pytorch-transformers | framework | cpu, cuda, hip, mps, xpu, tpu, cann, musa | Framework dependency, not permission to load arbitrary remote code. A selected model worker defines the actual operation and package boundary. |
| [Diffusers](https://huggingface.co/docs/diffusers/installation) | diffusers | implementation | cpu, cuda, hip, mps, xpu | Fixed pipeline IDs and pinned code/models. No generic Python or workflow execution API. |
| [Sentence Transformers](https://sbert.net/docs/sentence_transformer/usage/efficiency.html) | sentence-transformers | implementation | cpu, cuda, hip, mps, openvino | PyTorch/ONNX/OpenVINO implementation paths require separate model/runtime identities and output-dimension validation. |
| [ONNX Runtime GenAI](https://github.com/microsoft/onnxruntime-genai) | onnxruntime-genai | implementation | cpu, cuda, directml, tensorrt, openvino, qnn, webgpu | Generation layer over ONNX Runtime. Can be a dependency of Ryzen AI OGA, not necessarily another runtime install. |
| [WhisperKit / Argmax open-source SDK](https://github.com/argmaxinc/argmax-oss-swift) | whisperkit | implementation | coreml | Separate WhisperKit/SpeakerKit/TTSKit operation contracts. Open-source and Pro SDK capabilities must not be conflated. |
| [FluidAudio](https://github.com/FluidInference/FluidAudio) | fluidaudio | implementation | coreml | Apple Neural Engine-focused Core ML pipelines. Capture actual stage placement instead of promising all-ANE execution. |
| [Vosk](https://github.com/alphacep/vosk-api) | vosk | implementation | cpu | Small-device streaming recognition candidate when its vocabulary/language coverage is needed. |
| [NVIDIA NeMo Speech](https://github.com/NVIDIA-NeMo/Speech) | nemo-speech | implementation | cuda, cpu | Use fixed inference workers only. Training, fine-tuning and export remain excluded. |
| [Core ML](https://developer.apple.com/documentation/coreml) | coreml | framework | coreml | Usually a dependency of a named implementation. CPU/GPU/ANE compute-unit selection is part of the execution plan. |
| [Qualcomm QAIRT / QNN / Genie](https://docs.qualcomm.com/bundle/publicresource/topics/80-63442-10/general_overview.html) | qairt | vendor-stack | cpu, opencl, qnn, hexagon | Distinguish CPU/Adreno/HTP execution and use selected external prepared models. Keep host firmware/driver ownership outside Juiced. |
| [ExecuTorch](https://github.com/pytorch/executorch) | executorch | deployment-runtime | xnnpack, vulkan, coreml, qnn, openvino, cuda, metal, webgpu | Pre-exported .pte and target delegate identity. Some desktop backends are experimental. Never expose torch.export as a Juiced producer. |
| [LiteRT / LiteRT-LM](https://github.com/google-ai-edge/LiteRT-LM) | litert-lm | deployment-runtime | cpu, opencl, metal, webgpu, qnn | Exact GPU/NPU delegates need target-specific confirmation before selection. Candidate backend names are discovery leads. |
| [MNN](https://github.com/alibaba/MNN) | mnn | deployment-runtime | cpu, cuda, vulkan, metal, opencl, coreml, hexagon, qnn | Acquire prepared MNN artifacts. Operator/quantization coverage and NPU paths vary by build. |
| [ncnn](https://github.com/Tencent/ncnn) | ncnn | deployment-runtime | cpu, vulkan | Named vision/upscaling pipelines. General graph execution and model conversion are outside scope. |
| [RKLLM](https://github.com/airockchip/rknn-llm) | rkllm | vendor-stack | rknpu | Rockchip-specific LLM runtime with prepared .rkllm artifacts. Vision companions can require RKNN. |
| [RKNN](https://github.com/airockchip/rknn-toolkit2) | rknn | vendor-stack | rknpu | Selected Rockchip prepared-model pipelines. Separate from RKLLM language execution. |
| [HailoRT](https://github.com/hailo-ai/hailort) | hailort | vendor-stack | hailo | Chip family is mandatory. Hailo-8/8L capabilities do not imply Hailo-10 language support, or vice versa. |
| [Coral Edge TPU runtime](https://github.com/google-coral/libedgetpu) | coral-edgetpu | vendor-stack | edgetpu | Compatible precompiled edge vision artifacts and exact USB/PCIe accelerator identity. |

## Independently owned services

Connect to an explicitly configured independently owned service first. Lifecycle management must be separately negotiated.

| Project / implementation | Catalog ID | Kind | Candidate compute paths | Boundary |
| --- | --- | --- | --- | --- |
| [Ollama](https://docs.ollama.com/gpu) | ollama | external-service | cpu, cuda, hip, vulkan, metal | Attach to an explicitly configured service. Observe provider-owned models and lifecycle limits. Do not silently install a second model/cache manager. |
| [LM Studio](https://lmstudio.ai/docs/app) | lm-studio | external-service | cpu, cuda, hip, vulkan, metal | Attach to its published API. llama.cpp/MLX implementation and actual capabilities must be reported per endpoint/model. |
| [KoboldCpp](https://github.com/LostRuins/koboldcpp) | koboldcpp | engine-distribution | cpu, cuda, hip, vulkan, metal | Attach first. A later managed executable adapter needs one lifecycle owner and selected internal-engine qualification. |
| [TabbyAPI](https://github.com/theroyallab/tabbyAPI) | tabbyapi | external-service | cuda, hip | Serving layer for ExLlamaV3. Can serve as its managed worker adapter after qualification, not a duplicate underlying engine. |
| [LocalAI](https://localai.io/docs/) | localai | external-service | cpu, cuda, hip, vulkan, metal | Multi-engine service. Attach with explicit models and operation contracts; do not import its entire backend matrix as Juiced support. |
| [ComfyUI](https://github.com/comfyanonymous/ComfyUI) | comfyui | external-service | cpu, cuda, hip, mps, xpu | Only explicitly allowed fixed workflows. No arbitrary graph, node install, filesystem or Python execution through public Juiced APIs. |

## Packaging and client-local references

Recognize packaging or client-local execution. No new daemon worker is proposed by this inventory.

| Project / implementation | Catalog ID | Kind | Candidate compute paths | Boundary |
| --- | --- | --- | --- | --- |
| [llamafile / whisperfile](https://github.com/mozilla-ai/llamafile) | llamafile | packaging | cpu, cuda, metal | Distribution of llama.cpp/whisper.cpp. Resolve the embedded build and supported platform before reuse. |
| [WebLLM](https://github.com/mlc-ai/web-llm) | webllm | deployment-runtime | webgpu, wasm | Browser runs on the client's device, outside daemon resource admission. Requires a separately scoped client-inference design. |
| [Transformers.js](https://github.com/huggingface/transformers.js) | transformers-js | implementation | wasm, webgpu | Browser ONNX inference is client-local. A server-side Node worker would require a separate managed adapter. |

## Legacy recognition

Recognize retained installations/artifacts without automatically creating new managed integrations.

| Project / implementation | Catalog ID | Kind | Candidate compute paths | Boundary |
| --- | --- | --- | --- | --- |
| [ExLlamaV2](https://github.com/turboderp-org/exllamav2) | exllamav2 | engine | cuda | Archived upstream. Recognize retained EXL2 installations/artifacts; do not automatically reinterpret them as EXL3. |
| [Text Generation Inference](https://github.com/huggingface/text-generation-inference) | tgi | engine | cuda, hip | Maintenance mode upstream. Retain attach/import recognition when needed; prefer current engines for new managed integration. |

## Model family examples

These examples identify weights and architectures. Runtime candidates still
need a compatible artifact export, importer and selected-operation receipt.

| Model family | Candidate implementations | Boundary |
| --- | --- | --- |
| [futo-asr4all](https://futo.tech/blog/asr4all) | futo-transcribe-cpp, onnxruntime, executorch, pytorch-transformers | Exports do not establish compatible importers. Keep FUTO speech artifacts distinct from earlier ACFT Whisper artifacts. |
| [futo-whisper-acft](https://github.com/futo-org/voice-input-models) | whisper-cpp | Earlier FUTO Whisper fine-tunes; preserve the exact ACFT model and implementation compatibility. |
| [cactus-needle](https://github.com/cactus-compute/cactus) | cactus-small | Tool proposal/extraction model. Juiced's dispatcher owns any authorized tool execution. |
| [cactus-whistle](https://github.com/cactus-compute/cactus) | cactus-small | Speech model, not an independent hardware runtime. |
| [cactus-mirage](https://github.com/cactus-compute/cactus) | cactus-small | Vision/extraction model, distinct from a generic multimodal model importer. |
| [kokoro](https://github.com/thewh1teagle/kokoro-onnx) | kokoros, kokoro-onnx, sherpa-onnx, mlx-audio, fluidaudio | Different implementations and companion/voice formats must remain separate profiles. |
| [parakeet](https://github.com/handy-computer/transcribe.cpp) | parakeet-redux, handy-transcribe-cpp, sherpa-onnx, mlx-audio, fluidaudio, nemo-speech | Model revision/export/decoder type must match the selected implementation. |
| [whisper](https://github.com/ggml-org/whisper.cpp) | whisper-cpp, ctranslate2, handy-transcribe-cpp, whisperkit, mlx-audio | Do not reuse model artifacts or capability declarations across these implementations without explicit compatibility. |
| [moonshine](https://github.com/moonshine-ai/moonshine) | moonshine-voice, handy-transcribe-cpp | Toolkit version, architecture and exported artifacts are independently identified. |
