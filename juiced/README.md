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
| Models | Runtime dashboard with readiness counters, service cards and an expandable unloaded inventory. Try opens a temporary service test. |
| Loading policy | Set residency in three drag-and-drop lanes: Hot, On demand and Disabled. Menus and bulk actions offer the same commands. |
| Library | Search Hugging Face and review files; inspect downloaded weights in a table; follow transfers; upload or register a GGUF in a dedicated import form. |
| Service APIs | Copy client endpoints and inspect enabled-service counts and session-cache requirements. |
| Logs | Live raw output and client-parsed fields, search, severity/component filters, pause/follow, filtered export and tab-local clear. |
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

Cache settings include per-model overrides. Set a utility model to Memory only
to use HA's native Ollama integration while other models keep persistent Chat
Completions snapshots. The default mode remains the fallback for models without
an override. Changing an override preserves existing snapshots and qualification
evidence. `POST /v1/juiced/cache` accepts `model_modes`, an object mapping canonical
model IDs to `memory` or `persistent`; an empty object clears the overrides.

Legacy prototype conversation routes return HTTP410. Existing files are preserved, never extended or used as a cache index. Cache retention and forgetting use snapshot IDs or opaque consumer-session IDs.

## Runtime diagnostics

The UI subscribes to native `/logs/stream` WebSocket messages. Raw preserves each
stream line; Parsed separates timestamp, severity, component and message, with the
original line available by expanding it. Explicit child-process error/warning
prefixes are identified without changing the stream severity or raw output.

A shared tab-local buffer serves Logs and the dashboard's recent activity. It is
limited to 2,000 entries and 2 Mi characters of line text; no logs are persisted by
the UI. The manager retains up to 5,000 entries since restart. Reconnect reloads
that history, deduplicates sequence numbers and detects restarted sequences.
Pause freezes only the log view. Clear resets this tab's buffer, including recent
activity, without removing server logs. Export downloads raw lines matching the
current filters. Server timestamps retain the server's clock format.

Dashboard activity extracts explicit native inference summaries and recent
warnings/errors. It does not calculate resource or throughput counters by
scraping text. The bottom status bar uses native `/v1/system-stats` and `/v1/stats`,
with live token estimates distinguished from the last completed decode rate.

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

## Runtime dashboard and removal

The bottom metrics strip reads native `/system-stats` and `/stats` every two seconds. It shows host CPU/RAM, GPU utilization/allocation, NPU when reported, active requests, live estimated output and the last completed decode rate. Details exposes input/output/cache counts and first-token latency. Shared GPU memory overlaps RAM. Missing or stale readings display a dash, rather than a made-up zero.

Model details and row menus offer removal. `GET /v1/juiced/removal?model=...` previews the current file-ownership plan. `POST /v1/juiced/removal` requires that exact preview and confirmation of the model name. Disable/unload the model and finish active downloads first. Managed downloads use the existing model manager's shared-variant cleanup. An unshared uploaded GGUF is deleted from Juiced storage. External/shared local files and retained unsupported artifacts are unregistered without deleting source files. Caches remain separately managed. Built-in catalog definitions can be downloaded again.

Removal routes use all four native version prefixes. The older delete route also requires a Disabled, unloaded model. Release gates cover disposable uploaded/external/shared-cache fixtures, wrong/stale confirmations, Hot protection, and the upstream shared-repository deletion contract.

## Direct client protocols

All interfaces use the same model registry, loading policy and backend workers.
`GET /v1/juiced/protocols` reports implemented subsets and listener process state;
the Service APIs screen shows their addresses and exclusions. Registration of a
route does not establish conformance for every model or client feature.

| Client protocol | Address | Implemented subset |
| --- | --- | --- |
| OpenAI | HTTP base plus `/v1` | Models, Chat Completions, Responses, embeddings, image generation, transcription and speech. Actual model/backend support applies. |
| Ollama | HTTP origin | Chat/generate, tags/show/ps, embeddings. Discovery contains enabled downloaded chat and embedding models. |
| Anthropic | HTTP origin | Messages and streaming tool use; token counting delegates to the selected supported backend. Native llama.cpp b12032 counts without generation. SDK model discovery uses its expected list envelope. Unknown release timestamps are omitted. |
| AUTOMATIC1111 / sd.cpp clients | HTTP origin | `/sdapi/v1/sd-models`, options, txt2img, sampler/scheduler discovery and progress. Native sd-cpp handles image controls and image bytes. |
| Wyoming | Private TCP port10300 | Optional English transcription and Kokoro speech through the same HTTP/model workers. |

Ollama clients cannot download/delete weights through these compatibility routes.
Use Juiced management, which applies ownership and Hot-model protections.
`keep_alive:0` cannot unload Hot models. Disabled models are excluded from
discovery and reject serving. OpenAI discovery also supports
`?service=chat|transcribe|speak|images|embeddings`; the native catalog remains complete.
API-key authentication covers `/sdapi/*`; Anthropic clients can use `x-api-key`
for Messages and model discovery. Administration continues to require the
configured admin credential.

For SD API generation, specify `model`, use `override_settings.sd_model_checkpoint`,
or select the global default with `POST /sdapi/v1/options`. Multiple enabled
image models require an explicit choice. Requests capture that model independently
of later changes to the default. Juiced does not expose arbitrary scripts,
file-based LoRAs, server-side image saving, hires processing or global interrupt.
Unsupported fields return an error. It does not claim full WebUI or ComfyUI support.
Sampler/scheduler discovery requires the selected model to be loaded; querying
progress does not load it. Disabled choices never enable themselves.

Enable the packaged speech listener with `JUICED_WYOMING_ENABLED=true` and optionally
`JUICED_WYOMING_URI=tcp://0.0.0.0:10300`. The container must join the consumer's
private network; publishing a host port is not required for HA on that network.
Wyoming has no protocol authentication and must not be exposed publicly.
The pinned upstream `wyoming_openai`0.8.0 implementation runs as a supervised
child of lemond. A model-policy change refreshes its speech catalog by restarting
the listener. Keep Kokoro Hot for voice discovery at startup. English Kokoro voices
are read from the live backend; no separate model/voice configuration is required.
Normal speech content is excluded from its default WARNING-level logging.

The runtime contains a Python interpreter and a pinned speech environment, but
no pip, setuptools, compiler or build toolchain. Installation happens in the voice
builder stage. The default listener is disabled, so existing speech adapters can
remain in service until an isolated client test and deliberate network cutover.

Persistent session snapshots remain qualified only for Chat Completions. Other
chat protocols require Memory mode for a qualified model; per-model overrides
let a utility model coexist with persistent coding-model snapshots. Responses
and media features still depend on backend support. Provider-hosted tools and
conversation storage are outside this compatibility contract.

`juiced/tests/test_clients.py` uses pinned OpenAI, Anthropic and Ollama SDKs against
a real enabled model. It checks discovery, JSON/streaming chat, token counting
and Responses. `--tools` adds real tool-result round trips and needs a capable
model. CI uses Stories15M only for protocol/engine behavior, not quality or tool
ability. Production-model and unchanged consumer acceptance are separate checks.
