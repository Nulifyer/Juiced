# Juiced

A local model appliance derived from Lemonade, with a T3-derived model library
and temporary multimodal service tests. The model runtime owns weights, backend
processes and serving policy. Agent tools/workspaces remain in a separate stack.

## Development

- Manager: `cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_WEB_APP=OFF -DBUILD_TAURI_APP=OFF`, then `cmake --build build -j4 --target lemond`.
- UI: `cd juiced/ui && bun install --frozen-lockfile && bun run build`.
- Native API tests: `python3 juiced/tests/test_api.py build/lemond`.
- Actual engine cache gate: `python3 juiced/tests/test_engine_cache.py --help`; uses a checksum-verified tiny CPU fixture in release CI, then real-model acceptance on Mage.
- Container: `docker build -f juiced/Dockerfile -t juiced:development .`.

The dev server can proxy a remote runtime with `JUICED_DEV_API=https://your-runtime.example bun run dev`. Its default target is the local manager on port13305.

The UI build produces root `web/`; the image copies it into native resources.
The C++ server serves the UI and APIs on port8080. Models are downloaded to a
persistent HF cache, not embedded into the image. No Docker socket is required.

The Dockerfile keeps manager, UI and native engine builds in separate stages.
Its final `runtime` target starts from Ubuntu24.04 and copies only compiled
binaries, resources and license notices. It installs runtime libraries, Mesa's
Vulkan driver, hardware discovery and archive/download tools. Compilers, headers,
Make, CMake, Node, Bun and other build tools are excluded and checked before
publication. Backends that require a JIT compiler, such as vLLM/Triton, need an
explicitly provisioned runtime variant and are outside this default native image.

## Workspaces

| Screen | Purpose and layout |
| --- | --- |
| Models | Operate live services: ready/busy models grouped by service, followed by a compact unloaded inventory. Try opens the selected model and service. |
| Loading policy | Set residency in three drag-and-drop lanes: Hot, On demand and Disabled. Menus and bulk actions offer the same commands. |
| Library | Search Hugging Face and review files; inspect downloaded weights in a table; follow transfers; upload or register a GGUF in a dedicated import form. |
| Service APIs | Copy client endpoints and inspect enabled-service counts and session-cache requirements. |
| Cache | Inspect consumer-session computation snapshots separately from retention, mode and disk-budget settings. Unsaved settings survive background refreshes. |
| Test services | Chat has a conversation and anchored composer. Transcribe pairs an audio source with an editable/exportable transcript. Speak pairs a script with playback. Images pairs a prompt with a preview. Compare places responses beside each other after sequential requests. |

The sidebar follows the current task: service filters for operations, acquisition
navigation for Library, cache information for storage, and task navigation
for temporary tests. Wide workspaces collapse into a single column on narrow screens.

## Serving behavior

Models shows actual Loaded/Unloaded state and operation status. Loading policy
separates Hot/On demand/Disabled intent. Native C++ reconciliation recovers hot
models with bounded retries. Existing native registries and downloads supply the
catalog; the product does not duplicate model registrations.

Library provides native Hugging Face search/variant review and transfers, plus
chunked single-file GGUF uploads and approved-directory server import. Split
weights and vision companions use managed HF downloads in this release. The
transfer journal retains interrupted work across restart and retries through
the native downloader. New, unconfigured, downloaded and imported models default to Disabled. Explicitly choose On demand or Hot to permit inference. Disabled models reject inference and are excluded from OpenAI and Ollama discovery.

Test services uses eligible loaded deployments for chat, speech and images. Chat
streams and supports images when declared by the model. Messages remain only in page memory and clear on navigation, reload, model change or Clear test. There is no conversation library or persistent chat API. Downstream clients own history. English audio is converted in-browser to16kHz WAV for the CPU recognizers.
There are no shell, browser, MCP or autonomous agent tools in this application.

Cache capability is reported explicitly. Memory reuse is engine-owned. Persistent
Chat Completions reuse is opt-in and applies only to exact qualified engine/model
profiles. The image uses the checksum-verified upstream llama.cpp b12032 Vulkan
release, not a locally patched engine. The acceptance fixture must verify useful reuse after save/restart/
restore before an operator records qualification. A saved file alone does not
qualify it. External clients send full history and a stable `X-Juiced-Session-Id`; snapshots have matching
prefix/profile keys, checksums, expiry and a retained-state disk budget. Temporary
snapshot writes require additional free disk. Retain/release and forget affect
saved computation independently of client-owned messages or weights. Requests without a stable
session ID do not create persistent snapshots. Temporary UI requests do not send session IDs. The older `X-Juiced-Thread-Id` header remains an alias. Snapshot misses, invalid files and incompatible prefixes rebuild inside their leased slot. An unavailable admission path returns a retryable503 instead of racing another request. Other inference protocols are not
qualified for persistent session snapshots in this first release.

Legacy prototype conversation routes return HTTP410. Existing files are preserved, never extended or used as a cache index. Cache retention and forgetting use snapshot IDs or opaque consumer-session IDs.

## Provenance

Upstream Lemonade base: v2026.44.1, commit69948a7d40afbf91f6ad990d8b863c83ce5cc71d.
The root LICENSE and existing upstream history are retained. T3 UI extraction is
pinned with its MIT notice in `juiced/ui/LICENSE.t3code` and `T3-SOURCES.json`.
FUTO and Parakeet run as native subprocesses with their real backend identities;
each runtime bundle includes its source-license notices. Browser dependency
notices are generated from bundled modules at `/THIRD_PARTY_NOTICES.txt`.
Native build-dependency notices are retained under `/opt/juiced/licenses`. Weights retain their
publisher licenses and are not included in the image.

## Release and deployment

The release workflow builds and tests the image before publishing to
`ghcr.io/nulifyer/juiced`. Versioned tags identify releases. Deploy with a retained
/data volume, /dev/dri for Vulkan, and a private network. An optional read-only
/library mount exposes existing GGUFs for explicit import. Keep administration
on the trusted internal route; inference and management keys use native Lemonade
key handling when configured. Do not mount another manager's writable config.

Homelab deployment/evidence is owned by the separate Homelab repository. Existing
HA and other clients are deliberately not migrated during initial user testing.

Discovery groups GGUF quantization repositories under the exact model and publisher. File review shows selectable weight sizes. Model rows support dragging from their name or body, with independent checkbox/menu actions. `GET /v1/audio/voices?model=kokoro-v1` reads available voices from the loaded Kokoro backend. The speech selector uses that list; unsupported backends explain the limitation and accept an explicit voice ID.

Hugging Face logo: official brand asset from https://huggingface.co/brand, stored locally under `juiced/ui/public/huggingface.svg`. The mark belongs to Hugging Face.
