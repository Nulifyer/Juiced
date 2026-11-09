<div align="center">

<img src="juiced/ui/public/juiced-icon.svg" alt="Juiced" width="112" height="112">

# Juiced

### Your models. Your hardware. One place to run them.

A self-hosted model server with a browser console for managing local AI.
Chat, speech, and image generation through the APIs your apps already use.

[![Container](https://img.shields.io/badge/container-ghcr.io%2Fnulifyer%2Fjuiced-blue?logo=docker)](https://github.com/Nulifyer/Juiced/pkgs/container/juiced)
[![License](https://img.shields.io/badge/license-Apache--2.0-green)](LICENSE)

[Get started](docs/guide/README.md) · [User guides](docs/README.md) · [Releases](https://github.com/Nulifyer/Juiced/releases) · [Source](https://github.com/Nulifyer/Juiced)

</div>

## Meet Juiced

Downloading a model is the easy part. Choosing an engine, fitting it into memory,
keeping it ready, and connecting your apps takes more work.

Juiced puts those controls in one console. Browse models and quants, choose a
runtime, set loading policies, inspect effective settings, and test the result.
Your apps connect to the same server for inference.

Run Juiced on the machine with your models and accelerators. Manage it from a
browser on your desktop, laptop, or phone.

## What you can do

| Task | What Juiced gives you |
| --- | --- |
| Build a model library | Bundled catalog, Hugging Face search, quant selection, downloads, and local GGUF import |
| Choose how models run | Per-model runtime profiles with package, hardware, and file compatibility information |
| Keep services ready | Hot, On-demand, and Disabled policies, with worker status and load/unload controls |
| Tune without guessing | Model settings, profile-specific overrides, and an Effective view showing where values come from |
| Connect your apps | OpenAI, Ollama, Anthropic, and AUTOMATIC1111 compatibility subsets, MCP, and optional Wyoming speech |
| Test before connecting | Temporary chat, transcription, speech, image, and side-by-side model tests |
| Reuse computation | Memory caching and qualified persistent prompt snapshots, with storage and retention controls |
| See what is happening | Hardware metrics, token throughput, worker activity, and searchable logs |
| Prepare another quant | Supported local HF-to-GGUF and GGUF requantization jobs that preserve the source weights |

Juiced hosts models and manages their runtime. Your connected clients own saved
conversations, coding sessions, documents, and creative projects.

## Start with Docker

The release image targets Linux x86-64. Start a local instance with persistent
storage:

```bash
docker run -d --name juiced \
  -p 127.0.0.1:8080:8080 \
  -v juiced-data:/data \
  --restart unless-stopped \
  ghcr.io/nulifyer/juiced:v0.9.2
```

Open **http://localhost:8080**. Download a small chat model under **Library**,
choose a compatible runtime, and enable it under **Loading policy**.
Use **Test services** to send your first prompt.

The [installation guide](docs/guide/install/README.md) covers Compose, GPU access,
permissions, and release selection. For remote access, follow the
[access guide](docs/guide/access.md).
Ready-to-run [Compose setups](docs/guide/install/compose.md) cover CPU,
NVIDIA CUDA, AMD ROCm, and AMD or Intel Vulkan.

## Use your hardware

Choose CPU or a compatible GPU profile for each model. llama.cpp profiles can
reuse the same compatible GGUF across CPU, Vulkan, ROCm, CUDA, or native Apple
Metal. Other engines have their own model-format and hardware requirements.

The Linux container includes a Vulkan llama.cpp runtime and CPU speech workers.
Additional backends depend on their packages and the host. Native core builds
are checked on Linux, Windows, and macOS; that does not establish packaged
Juiced desktop support or qualify every accelerator.

The experimental XDNA2 + ROCm profile accelerates eligible prefill operations on
specific AMD hardware. It requires a matched runtime bundle and does not promise
a speed gain. See [runtimes and hardware](docs/guide/runtimes.md).

## Find your next step

- [Download or import a model](docs/guide/models.md)
- [Choose loading policies](docs/guide/loading-policy.md)
- [Tune a model](docs/guide/settings.md)
- [Connect an application](docs/integrations/README.md)
- [Manage cache and storage](docs/guide/cache.md)
- [Diagnose a problem](docs/guide/troubleshooting.md)

## Development and credits

Juiced is an independent project derived from Lemonade. Its console includes
T3 Code-derived presentation components. We retain the upstream licenses and
notices. See [credits](docs/credits.md) and the
[contributor guide](CONTRIBUTING.md).

Product plans and native contracts live under [juiced/](juiced/README.md).
Planned work, including the optional Barista operator assistant, is separate
from the capabilities described above.
