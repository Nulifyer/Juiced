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
| Build a model library | Recommended models, Hugging Face search, quant selection with a fit estimate, downloads, and local file import |
| Choose how models run | An engine build picked for your hardware, installed on demand, and switchable per model |
| Keep services ready | Hot, On demand, and Disabled policies, with a model memory budget, idle unload, and load/unload controls |
| Tune without guessing | Launch settings, generation defaults, a measured tune run, and a view of where each value comes from |
| Connect your apps | OpenAI, Ollama, Anthropic, and AUTOMATIC1111 compatibility subsets, MCP, and optional Wyoming speech |
| Test before connecting | Temporary chat, transcription, speech, image, and side-by-side model tests |
| Reuse computation | A conversation cache in memory and on disk, with budget and retention controls |
| See what is happening | Hardware metrics, token throughput, worker activity, and searchable logs |
| Choose a published variant | Find compatible existing formats and quants while preserving original weights |
| Manage from a chat | Barista, an optional assistant that checks and changes Juiced with one of your own models |

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
  ghcr.io/nulifyer/juiced:v0.11.9
```

Open **http://localhost:8080**. Find a small chat model under **Models →
Discover**, select **Download**, then **Add model**. New models are **On demand**
by default. Use **Test services** to send your first prompt.

The [installation guide](docs/guide/install/README.md) covers Compose, GPU access,
permissions, and release selection. For remote access, follow the
[access guide](docs/guide/access.md).
Ready-to-run [Compose setups](docs/guide/install/compose.md) cover CPU,
NVIDIA CUDA, AMD ROCm, and AMD or Intel Vulkan.

## Use your hardware

Juiced detects your CPU, GPUs and NPUs and offers the engine builds that fit.
The container ships no engines. It downloads each build you choose into the data
volume and checks it against a pinned checksum. The llama.cpp CPU, Vulkan, ROCm
and CUDA builds all run the same GGUF file. Other engines have their own
model-format and hardware requirements.

The experimental XDNA2 + ROCm build accelerates eligible prefill operations on
specific AMD hardware. It does not promise a speed gain. See
[runtimes and hardware](docs/guide/runtimes.md).

## Find your next step

- [Download or import a model](docs/guide/models.md)
- [Choose loading policies](docs/guide/loading-policy.md)
- [Tune a model](docs/guide/settings.md)
- [Connect an application](docs/integrations/README.md)
- [Manage the conversation cache](docs/guide/cache.md)
- [Manage storage](docs/guide/storage.md)
- [Diagnose a problem](docs/guide/troubleshooting.md)

## Development and credits

Juiced is an independent project derived from Lemonade. Its console includes
T3 Code-derived presentation components. We retain the upstream licenses and
notices. See [credits](docs/credits.md) and the
[contributor guide](CONTRIBUTING.md).

Product plans and native contracts live under [juiced/](juiced/README.md).
Planned work is separate from the capabilities described above.
