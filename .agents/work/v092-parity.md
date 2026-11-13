# v0.9.2 parity: what the rewrite still lacks

Status: active (audit done 2026-11-12; nothing implemented yet)
Owner: primary agent
Request source: user, 2026-11-12: "at least have the things from v0.9.2, then work on the rest".
Baseline: native tag `v0.9.2` (fd6ffaf7) compared with rewrite tag `v1.0.0-rc.12` (edabee1c).

## Method and evidence

- v0.9.2 sources: `git show v0.9.2:<path>` for `src/cpp/server/*`, `src/cpp/cli/*`, `docs/api/*`, `juiced/*`, and the v0.9.2 console in `juiced/ui/src`. In this file, `V/` means `src/cpp/server/` at v0.9.2. `R/` means `juiced/rewrite/service/src/` at rc.12. `UI/` means `juiced/rewrite/ui/src/`.
- Live comparison on the same requests:
  - `ghcr.io/nulifyer/juiced:v0.9.2` on 127.0.0.1:13440, with a throwaway volume and `JUICED_WYOMING_ENABLED=true`.
  - The rc.12 `lemond` debug build (service code unchanged since fc3ab979) on 127.0.0.1:13442, with an empty schema-2 config.
  - Both started with no models and no keys. The probe script is described under "Acceptance". The container and volume were removed after the audit.
- Production: GET-only reads of https://juiced.swarm.nulteck.com (`/api/v1/juiced/state` native adoption). Five Disabled adopted models are `waiting_for_engine`: four `hybrid-rocm` and `parakeet-cpp-gguf-redux-packed`.
- Inventories `api-parity/inventory.json` (135 routes) and `FEATURE-COVERAGE.json` were used as an index only. Every row below was rechecked against source or a live probe.
- Rows marked **[unverified]** rest on code reading alone.

### Priority rule used

- **P0:** breaks a named client in the user's setup (Home Assistant llama_cpp / Ollama / Wyoming, Pi via pi-t3-agent, SillyTavern, the browser console). Also any client-facing serving contract documented at v0.9.2: `docs/api/{openai,ollama,anthropic,mcp,llamacpp}.md`, plus the A1111 and Wyoming subsets listed by `GET /v1/juiced/protocols`.
- **P1:** a documented v0.9.2 operator feature: `docs/api/lemonade.md` management routes, console capabilities, config, CLI and observability.
- **P2:** rarely used, unused in the user's configuration, or superseded by a rewrite feature (the row names which one).
- Engine-dependent routes take the priority of their engine.

## Acceptance

- [ ] Every P0 row has a rewrite implementation and a rust-tests scenario. The scenario sends the v0.9.2 request shape and checks the v0.9.2 status, body fields and headers. An intentional difference has to be recorded here instead.
- [ ] Differential probe: run the v0.9.2 image and the rewrite side by side, as in this audit, with the same GET/POST list. That list is all `V/server.cpp:1198-1583` registrations, `V/ollama_api.cpp:161-230`, `V/anthropic_api.cpp:404-407`, `V/mcp_server.cpp:237-259`, `V/juiced/service.cpp:621-660` and `V/juiced/compatibility.cpp:91-106`. No P0 or P1 route returns 404 `unknown_endpoint` where v0.9.2 returned 2xx, 4xx validation, 409, 410 or 501. Only excluded-by-plan rows may differ, and those keep v0.9.2's explicit 410/501.
- [ ] Real-engine checks on the homelab hardware class:
  - Ollama `think:false`, OpenAI `chat_template_kwargs`/`enable_thinking:false` and Anthropic `thinking:{type:disabled}` each suppress reasoning on Ornith Q6_K.
  - SillyTavern A1111 mode lists models and samplers, then generates with z-image-turbo.
  - MCP `lemonade_transcribe_audio` and `lemonade_generate_image` return results.
- [ ] Each P1 row is either done or moved by the user to later work.
- [ ] `api-parity/inventory.json` statuses are refreshed for every row this ledger closes.
- [ ] User docs under `docs/guide` match the result.

## Gaps: P0 (15)

| # | Area | v0.9.2 behaviour | rc.12 behaviour | Who notices | Evidence | Effort |
|---|---|---|---|---|---|---|
| P0-1 | Thinking off | `normalize_thinking_controls` maps `enable_thinking:false`, `thinking:false` and `thinking:{type:disabled}` to `chat_template_kwargs.enable_thinking=false` plus `/no_think`, then strips the original fields. Ollama `think` goes through the same path. | **Differs (silently ignored).** OpenAI bodies are forwarded unchanged. Ollama `think` and Anthropic `thinking` become a top-level `enable_thinking`. llama-server reads only `chat_template_kwargs.enable_thinking` or `reasoning_effort:none`, so reasoning stays on. | HA Ollama (the ollama lib sends `think`), Anthropic clients, any OpenAI client turning thinking off on Ornith (a thinking model) | V/thinking_controls.cpp:34-104, V/server.cpp:4211, V/ollama_api.cpp:497-499; R/protocols.rs:328-335 and 624-629. llama.cpp `tools/server/server-common.cpp:1473-1503` (cached source 669204ee2) has no top-level read | S |
| P0-2 | Anthropic leniency | Unknown fields and blocks are ignored with warnings: `metadata`, unsupported `thinking` types, block types, `tool_choice` and `output_config` types | **Differs: 400.** Live: `metadata` → `Unsupported parameter: metadata`; `thinking:{type:enabled,budget_tokens:1024}` → `Unsupported parameter: budget_tokens`; `cache_control` on system or content blocks → 400; `service_tier` → 400. These are standard Anthropic request fields | Any Anthropic SDK / Claude-style client | V/anthropic_api.cpp:62,111,583,687,708,720,724-725; R/protocols.rs `fields(...)` checks (e.g. :329); probes on :13442 | S |
| P0-3 | Ollama leniency | Known options mapped (`temperature`, `top_p`, `seed`, `stop`, `num_predict`, `repeat_penalty`→`frequency_penalty`, `num_ctx`); unknown options and fields ignored | **Differs: 400.** Live: `options.min_p` → 400; `options.repeat_penalty` → 400; `/api/generate` `system` → 400; `context` → 400. Allowed top-level fields are limited to model/prompt/stream/options/format/raw (generate) and model/messages/stream/options/format/think/tools (chat) | Ollama clients other than HA (Open WebUI-style options); HA is safe only while it sends `num_ctx`, `keep_alive`, `think`, `tools` | V/ollama_api.cpp:120-140; R/protocols.rs:501-506, 688-699 | S |
| P0-4 | Ollama explicit rejections | `POST /api/create`, `/api/copy`, `/api/push`, `/api/blobs/*` → 501 `{"error":"not supported by Lemonade"}`. Juiced also answered `POST /api/pull` and `DELETE /api/delete` with 501 "Use Juiced model management…" | **Missing: 404** with an empty body for all six | Ollama clients and HA Ollama's config flow (pull when a model is missing) | V/ollama_api.cpp:221-230; docs/api/ollama.md:13-21; R/compat.rs:188-199; probes | S |
| P0-5 | A1111 subset | GET `sd-models`, `options`, `samplers`, `schedulers`, `progress`, `upscalers`, `sd-vae`, `latent-upscale-modes`; POST `options` (`sd_model_checkpoint`), `txt2img`; `img2img` and `interrupt` → 501 | **Partial:** only `POST /sdapi/v1/txt2img`. Every GET and the other POSTs → 404 | SillyTavern image generation in A1111 mode lists models, samplers and options first. Image model z-image-turbo-nsfw-v2 is On demand in production | V/juiced/compatibility.cpp:91-106, 146-185; R/compat.rs:199; probes (v0.9.2 200/409/501, rc.12 404) | S-M |
| P0-6 | MCP tools | `tools/list`: `lemonade_list_models`, `lemonade_chat`, `lemonade_transcribe_audio`, `lemonade_generate_image`, `lemonade_docs`. `serverInfo.name` was `lemonade-mcp` | **Partial:** `lemonade_list_models`, `lemonade_chat`, `lemonade_complete`, `lemonade_embed`. Transcription, image and docs are missing. `serverInfo.name` is `juiced-mcp` | MCP clients (docs/api/mcp.md) | docs/api/mcp.md:54-156; V/mcp_server.cpp:1248,1293; R/protocols.rs:1150-1160; live `tools/list` on both | M |
| P0-7 | Rerank | `POST rerank`, `reranking`, `reranker` (four prefixes) proxied to llama.cpp | **Missing: 404.** The rewrite also has no rerank operation type for models | RAG clients (docs/api/llamacpp.md:16) | V/server.cpp:1336-1346; R/config.rs:208 (operation enum); probes | M |
| P0-8 | Tokenize | `POST /v1/tokenize` proxied to the loaded llama.cpp worker | **Missing: 404** | SillyTavern when set to llama.cpp text completion; llama.cpp-style clients | V/server.cpp:1372; docs/api/llamacpp.md:370; probe | S |
| P0-9 | Slots | `GET /v1/slots`; `POST /v1/slots/{id}?action=save\|restore\|erase` | **Missing: 404.** Session save/restore exists under operator-only `/juiced/cache/*` | llama.cpp-style clients (docs/api/llamacpp.md:113-356) | V/server.cpp:1353-1369; probes | S-M |
| P0-10 | Image edits / variations / upscale | `POST images/edits` and `images/variations` (multipart, sd-cpp); `images/upscale` (base64, sd-cpp upscaler) | **Missing: 404.** Only `images/generations` | OpenAI image clients (docs/api/openai.md:664-907) | V/server.cpp:1387-1396; R/media.rs:259-271; probes | M |
| P0-11 | Bare `WS /realtime` | `/realtime` accepted bare and under all four prefixes, on the main port and `websocket_port` 9000 | **Partial:** `/v1/realtime` works (101). Bare `/realtime` → 404. No dedicated port | Realtime clients using the documented `WS /realtime` | V/websocket_server.cpp:780-791; docs/api/openai.md:486; R/http.rs:266; WS upgrade probes | S |
| P0-12 | Native discovery: `/health` | `version`, `all_models_loaded`, `model_loaded`, `max_models`, pinned counts, `websocket_port`, `telemetry` | `{"status":"ok"}` only. **in progress: helper-discovery** | Pi (pi-t3-agent) | V/server.cpp:2563; R/http.rs:665-670; live bodies | (owned) |
| P0-13 | Native discovery: `/v1/models` | Entries with `checkpoint`, `recipe`, `labels`, `downloaded`, `suggested`, `recipe_options`, `size`, `update_available`; `?show_all=true` lists 114 built-ins; `?service=` filter; `/v1/models/{id}` with the same fields | Bare `id/object/created/owned_by`; `show_all` ignored. **in progress: helper-discovery** | Pi, SillyTavern, label-filtering clients | V/server.cpp:3044-3060, 3351-3420; R/http.rs:1271-1286; live (`show_all`: 114 vs 0) | (owned) |
| P0-14 | Native discovery: capabilities | `/v1/juiced/capabilities`, `/.well-known/juiced.json` (name, version, catalog, capabilities, protocols, inference, ui) | **Missing: 404.** **in progress: helper-discovery** | Pi | V/juiced/service.cpp:621, 651-660; probes | (owned) |
| P0-15 | Catalog fields and ETag | Catalog `capability_contract`, context, thinking/tools capability; ETag and `revision` | Missing fields; no ETag. **in progress: helper-discovery** | Pi | V/juiced/capabilities.cpp; live `/v1/juiced/catalog` bodies | (owned) |

## Gaps: P1 (34)

### Native management API (docs/api/lemonade.md)

| # | Area | v0.9.2 | rc.12 | Who notices | Evidence | Effort |
|---|---|---|---|---|---|---|
| P1-1 | `POST /v1/pull` | Pull by registry name or `user.X` + checkpoint/recipe/mmproj; SSE `progress/complete/error`, or job mode | **Missing: 404.** `/juiced/library/pull` + polling, different body | `lemonade pull`, scripts | V/server.cpp:1417, 6008-6150; lemonade.md:580 | M |
| P1-2 | `GET /v1/downloads`, `POST /v1/downloads/control` | List jobs; pause/cancel/resume/remove | **Missing: 404.** `/juiced/artifacts/jobs/{job}` and `/juiced/library/pulls` instead | CLI, scripts | V/server.cpp:1436-1442; probes | S |
| P1-3 | `GET /v1/registry/search`, `GET /v1/pull/variants` | HF (or ModelScope) search; GGUF variants with mmproj/draft files | **Missing: 404.** `/juiced/library/search`, `/models/{m}/variants` | CLI, scripts | V/server.cpp:1421-1425; probes | S |
| P1-4 | `POST /v1/models/register` | `{model_name:"user.X", checkpoint, recipe, mmproj, labels}` | **Differs: 422.** Needs the rewrite ModelSpec | CLI, scripts | V/server.cpp:1413; R/config.rs:329,445; probe | M |
| P1-5 | `POST /v1/delete` | `model`/`model_name`; unloads, then deletes weights | **Differs:** needs `{model, expected_revision}`; keeps weights (`weights_retained:true`) | Operators expecting freed disk; CLI | V/server.cpp:6630-6680; R/http.rs:1192; R/supervisor.rs:2790 | S |
| P1-6 | `POST /v1/load` options | `model_name` plus `ctx_size`, `*_backend`, `*_args`, `save_options`, `pinned` | **Differs: 422** on `model_name` or any option (unknown fields denied) | `lemonade load --ctx-size`, scripts | V/server.cpp:6391-6405; R/http.rs:1421-1425; probe | M |
| P1-7 | `POST /v1/unload` with no model | Unloads all (200) | **Differs: 422** | CLI, scripts | probe | S |
| P1-8 | `GET /v1/models/{id}/files`; `DELETE /v1/models/{id}/options` | File metadata; reset options to defaults | **Missing.** `/models/{m}/artifacts` (operator, different shape); no DELETE routes exist at all | Console, CLI | V/server.cpp:1279-1305; lemonade.md:327,499; R/http.rs `method_name!` covers get/post only | S |
| P1-9 | `POST /v1/models/check-updates`, `update_available` | Upstream update check per model | **Missing: 405** | Operators | V/server.cpp:1273; lemonade.md:285; probe | M |
| P1-10 | `POST /v1/install`, `/install/dry-run`, `/uninstall` | Install, update or remove a backend by `{recipe, backend}`; dry run resolves the asset | **Missing: 404.** `/juiced/engines/install {engine, backend, expected_generation}`; no uninstall, no dry run, no update | `lemonade backends install`, operators | V/server.cpp:1487-1495, 8146-8458; R/supervisor/engines.rs:162 | M |
| P1-11 | `GET /v1/system-info` shape | `recipes.{r}.backends.{b}.{state,version,release_url}`, `devices`, `model_storage`, `unavailable_recipes` | **Differs:** hardware observations plus `backends[]` and `recommendations`, no install state; operator-only | CLI, older console code, scripts | V/system_info.cpp:1233-1590; R/hardware.rs:664; live bodies | M |
| P1-12 | `GET /v1/stats` | Last-request tokens, TTFT, tok/s, live counters | **Missing: 404.** `/juiced/requests` (operator) | Clients showing speed | V/server.cpp:1500; lemonade.md:1608; probe | S |
| P1-13 | `GET /v1/audio/voices?model=` | Kokoro voice list (409 until loaded) | **Missing: 404.** `/juiced/voices` instead | Speech clients | V/juiced/service.cpp:627, 684-705; probe | S |
| P1-14 | `/internal/*` | `config`, `config/defaults`, `set` (live validation incl. log level, origins, port), `pin`, `aliases` (GET/POST/DELETE), `models/sync`, `cleanup-cache`, `shutdown`, `telemetry/flush` | **Missing: 404.** Typed `/juiced/*` routes replace parts (`aliases`, `residency`, `admission`, `cache`, `protocols`, `wyoming`) | Older CLI and tray; operators | V/server.cpp:1517-1560; V/runtime_config.cpp:812-915; probes | M (aliases, pin: S; generic config: L) |
| P1-15 | `GET /v1/docs`, `/v1/docs/{page}` | Bundled API reference pages; also used by MCP `lemonade_docs` | **Missing: 404** | Clients and agents reading docs | V/server.cpp:1257-1265; lemonade.md:1410-1483 | S |
| P1-16 | Explicit rejections | `POST /jobs*` → 501; `/juiced/threads`, `/juiced/thread` → 410 "Juiced does not store conversations" | **Differs: 404 `unknown_endpoint`.** The inventory requires "preserve rejection" | Clients probing support | api-parity/inventory.json (excluded rows); probes | S |

### Observability, auth and transport

| # | Area | v0.9.2 | rc.12 | Who notices | Evidence | Effort |
|---|---|---|---|---|---|---|
| P1-17 | Container logs | Service and worker output on stdout, so `docker logs` showed it; live `log_level` | **Differs:** in-memory ring only (`/juiced/logs`). One stdout line after more than 100 requests in this audit (v0.9.2 container: 199 lines) | Every operator; Portainer log view | R/logs.rs:9-10; R/main.rs:276; live | S |
| P1-18 | `GET /live` | Unauthenticated liveness | **Missing: 404** | Load balancers, orchestrators | V/server.cpp:1198; lemonade.md:2438 | S |
| P1-19 | `GET /metrics` | Prometheus `lemonade_server_up`, `lemonade_model_*`, key-gated | **Missing: 404** | Grafana/Prometheus | V/prometheus_metrics.cpp:302-366; lemonade.md:1729 | M |
| P1-20 | `WS /logs/stream` | Live log stream (bare and prefixed) | **Missing: 404.** Paged `/juiced/logs` polling | Log tail clients | V/websocket_server.cpp:788; lemonade.md:2341; probe | S |
| P1-21 | CORS | `allowed_origins` (+`LEMONADE_ALLOWED_ORIGINS`): loopback/same-origin allowed, others 403; `Vary: Origin`; explicit allow-headers incl. `Authorization`, `Mcp-Session-Id`, `traceparent`; Private-Network-Access reply | **Differs:** `allow-origin: *`, `allow-headers: *` for every origin. The wildcard does not cover `Authorization` under the Fetch spec, so a cross-origin browser client sending a key likely fails preflight **[unverified in a browser]**. Any website can call an unauthenticated LAN instance. No PNA header | Cross-origin browser clients with keys; LAN security | V/server.cpp:1159-1189, 1985-1990; R/http.rs:1260-1265; preflight probes | M |
| P1-22 | Admin-key-only mode | Only `LEMONADE_ADMIN_API_KEY` set: inference and read routes stay open | **Differs:** every route requires a key. With both keys set, the inference key also loses `/system-info` and `/system-stats` | Deployments using only the admin key | V/server.cpp:1128-1145; R/auth.rs:135-160; R/http.rs:190,244 | S |
| P1-23 | Error body shapes | `{"error":"..."}` or OpenAI-style `{"error":{"message","type"}}` | `{"error":{"code","message","type":"juiced_error"}}` and `WWW-Authenticate` on 401 | Clients parsing errors **[low risk]** | probes | S |

### Engines, options, models

| # | Area | v0.9.2 | rc.12 | Who notices | Evidence | Effort |
|---|---|---|---|---|---|---|
| P1-24 | Per-model settings editor (console + LaunchOptions) | Edit generation defaults (temperature, top P/K, min P, repeat penalty, reasoning budget); llama.cpp `--parallel`, batch, ubatch, flash attention, KV cache type; free-form `llamacpp_args`; sd-cpp image defaults; asr4all threads/commit/right | **Partial:** LaunchOptions has only `context_tokens`, `threads`, `gpu_layers`, `parallel_slots`; the console edits llama.cpp context, device, threads and layers only. Adopted args and image defaults apply but cannot be edited | Operators tuning models (production uses `--spec-type draft-mtp`, `--chat-template-kwargs`) | v0.9.2 `juiced/ui/src/components/modelSettings.ts:17-128`, `ModelWorkspace.tsx:46-66,555-640`; R/profiles.rs:19-24; UI/ContextInspector.tsx | L |
| P1-25 | Per-backend global args and other recipe args on adoption | `llamacpp.rocm_args/vulkan_args/cpu_args`, `sdcpp.*_args`, `flm_args`, `moonshine_args` merged | Reported unsupported and dropped | Operators who tuned per-backend args | R/native_import.rs:265; R/native_import/options.rs:540 | S |
| P1-26 | Image defaults after import | Per-model `image_defaults` editable | Become sd-server flags at import; no editor (F157 planned); global `sdcpp.steps/cfg/width/height` dropped | Image operators | R/native_import/options.rs:243-276; R/native_import.rs:259-262 | M |
| P1-27 | hybrid-rocm (NPU+ROCm) and FLM on real hardware | Hybrid recipe and FLM ran on XDNA2 | Engines exist, tested on fake hardware only. Four production hybrid models are `waiting_for_engine`. NPU exclusivity between FLM and hybrid has no equivalent **[unverified]** | Strix/NPU operator (production) | prod `/juiced/state` native_adoption; R/supervisor/native_import.rs:156; V/router.cpp:956-1005 | M |
| P1-28 | ROCm coverage beyond gfx1150 | llama.cpp ROCm stable for gfx103X…gfx950; sd-cpp ROCm for 6 families; `rocm_channel` | Stable ROCm (b11580 + TheRock) only for gfx1150, others nightly; sd-cpp ROCm gfx1150 only; no gfx942/950; x86_64 only | Non-gfx1150 AMD hosts (not production) | R/engines/catalog.rs:186, 415-530 | M |
| P1-29 | Suggested model catalog | 260 registry entries (114 visible in the container) incl. 12 sd-cpp, rerankers, upscalers | 38 entries (2 image, 0 rerank, 0 upscalers) | Discover users | V resources/server_models.json; R/library/catalog.json | M |
| P1-30 | Vision projector on download; split GGUF | `pull/variants` offers mmproj; split files supported | Library pull has no projector handling; split GGUF/mmproj refused on upload/import-path (adopted mmproj works) | Vision-model users | v0.9.2 `Acquisition.tsx:94-160`; UI/FindModels.tsx:184 | M-L |
| P1-31 | Cache settings | `mode` memory/persistent, `retention_days`, `disk_budget_gib`, per-model `model_modes`; `cache/retain`, `cache/profile`, `cache/qualify` | `/juiced/cache` takes `{enabled, expected_generation}` only; per-model overrides exist; no retain, expiry or budget control (F106) | Operators with limited disk | V/juiced/cache.cpp:249-330; R/http.rs:851; probes | M |
| P1-32 | Eviction tuning | `max_loaded_models`, per-model `auto_evict`, `evict_idle_timeout`, soft-idle downsize, VRAM-pressure monitor | Memory-budget admission + idle unload + LRU under pressure; per-model eviction keys dropped, no downsize | Operators (production had `max_loaded_models: 4`) | V/eviction_engine.cpp:81-150; R/supervisor/residency.rs:452-480; prod adoption row | M |
| P1-33 | `lemonade` CLI | `pull MODEL`, `load` with recipe flags, `delete` (frees disk), `backends install/uninstall`, `run`, `logs`, `config get/set`, `alias`, `pin`, `cleanup-cache`, `export/import`, positional `list` filter; default port 13305 | Different command set: `pull --file`, `delete --revision` (keeps weights), no `run/logs/config/alias/pin/backends install`; default 127.0.0.1:13306; refuses a key without an explicit target. `docs/guide/cli.md:15-45` is inaccurate | Operators using `docker exec … lemonade` | V/../cli/main.cpp:151, 1531-1784; R/bin/lemonade.rs:16-50, 325-331 | M-L |
| P1-34 | Console capabilities | Bulk select (load/unload/policy), sort; model README and repo link; reload as a separate action; chat test max tokens + thinking toggle; microphone and any-format transcription (resampled to 16 kHz); engine version + update; Discover filters (capability/size/runtime, non-GGUF); log download, request filters, GPU picker; model type at import (chat/embed/rerank); resumable chunked upload | Missing or partial (see agent notes in "Notes") | Operator | v0.9.2 `ModelLibrary.tsx:186,318,485,588-616`, `Playground.tsx:190-198,444,576`, `BackendInventory.tsx:162-385`, `DiscoverModels.tsx:60-73`, `Diagnostics.tsx`, `RuntimeLogs.tsx`, `Acquisition.tsx:170-235,620-640`; UI/api.ts:9-13, UI/TranscriptionTest.tsx:34 | M (split per row) |

## Gaps: P2 (14)

| # | Area | v0.9.2 | rc.12 | Why P2 | Evidence | Effort |
|---|---|---|---|---|---|---|
| P2-1 | Engines with no rewrite equivalent: vllm (ROCm), thenoise (ROCm image), llamacpp-hrx, ds4 (gfx1151), onnxruntime (classify), openmoss (TTS/voice design), acestep (music), thinksound (SFX), trellis (3D) | Installable recipes | No engine; native models with these recipes are not adopted | Not used in the production config (recipes in use: llamacpp, sd-cpp, asr4all, parakeet, whispercpp, hybrid, kokoro, flm). Engine-management round | R/engines/catalog.rs:14-43; R/supervisor/native_import.rs:154-161; v0.9.2 `/v1/system-info` recipes | L each |
| P2-2 | `POST /v1/classify`, `audio/generations`, `3d/generations` | onnxruntime, acestep/thinksound/openmoss, trellis routes | **Missing: 404** | Depends on P2-1 engines (F167/F168 conditional) | V/server.cpp:1348,1400-1403; probes | with engine |
| P2-3 | Routing policies (`collection.router`, `/routing/validate`) | Rule, embedding and LLM-router classifiers with decision trace | Missing (F135-F144, P5) | No router models in the production config | V/routing_policy*.cpp; lemonade.md:100-284 | L |
| P2-4 | Cloud providers (`/v1/cloud/auth`, `LEMONADE_<PROVIDER>_API_KEY`, install cloud provider) | Ephemeral cloud keys, cloud models | Missing (F142, P5) | Production `cloud.providers` is empty | V/cloud_provider_registry.cpp:51; probes | L |
| P2-5 | OTLP telemetry (`traceparent` intake, `/spans/stream`, `/traces/stream`, `/internal/telemetry/flush`, CLI `telemetry`) | OpenInference/GenAI spans | Missing | Telemetry `enabled:false` in v0.9.2 health; `/juiced/requests` covers per-request metrics | V/telemetry.cpp; V/websocket_server.cpp:791 | L |
| P2-6 | Device selection and hybrid tuning UI (`juiced/devices`, `juiced/execution`) | Device picker, hybrid memory MiB, minimum prefill batch, NPU counters | Advanced-mode runtime package switch only | Single-iGPU production; partly superseded by the automatic engine choice | v0.9.2 `ExecutionProfiles.tsx:181-504`; probes (501/400 vs 404) | L |
| P2-7 | Prompt-state snapshots (`JUICED_SNAPSHOT_DIR=/data/prompt-state`) | Native snapshot store | Ignored; the rewrite store is `/data/kv-cache`; old snapshots not migrated | Superseded by the rewrite session cache (prefixes rebuild on first use) | V/juiced/service.cpp:124; R/main.rs:141 | S (doc) |
| P2-8 | `X-Juiced-Cache`, `X-Juiced-Restored-Tokens` response headers | Sent on cached requests | Not sent | No known client reads them; `/juiced/requests` has `cached_tokens` | V/juiced/cache.cpp:456,551,559 | S |
| P2-9 | Config keys ignored on adoption: `offline`, `download_rate_limit`, `no_fetch_executables`, `default_model_source` (ModelScope), `extra_models_dir`, custom `models_dir`, `HF_ENDPOINT` mirror | Honoured | Reported ignored or unsupported; mirror via `JUICED_HUGGINGFACE_URL` only | Not set in production | R/native_import.rs:277-331; R/library/hub.rs:12 | S-M |
| P2-10 | UDP beacon to the LAN | RFC1918 broadcast every 2 s (default `broadcast:false` in the Juiced container) | Loopback only, off in the container | Same default as v0.9.2's container; only LAN tray clients would notice | V/utils/network_beacon.cpp:185-296; R/discovery.rs:138-155 | M |
| P2-11 | `/api/generate` image generation | Ollama generate with an sd-cpp model returns an image | Not supported **[unverified]** | Rare | docs/api/ollama.md:10 | S |
| P2-12 | `/internal/simulate-vram-pressure` | Test hook | Missing | Testing only | V/server.cpp:1544 | S |
| P2-13 | Tray, systemd units, `LemonadeServer.exe` | Desktop lifecycle | None | Container-only deployment (R7 portable distribution) | data/lemond*.service.in | L |
| P2-14 | Resumable chunked import API (`juiced/import/start\|chunk\|finish\|cancel\|server`, `juiced/imports`, `juiced/transfers`) | Pausable remote upload | Single streamed upload + import-path | Superseded by `/juiced/library/upload` and `/juiced/library/import-path`; pause/resume lost | V/juiced/service.cpp:646; probes | M |

## Same in both (spot-checked)

- Port 8080, Wyoming on 10300, and the legacy `lemond CACHE CONFIG --host --port` argv.
- Wyoming describe identity (`wyoming_openai` 0.8.0, per the drop-in ledger); `JUICED_WYOMING_ENABLED`.
- `/api/tags`, `/api/ps`, `/api/version` 0.16.1, `:latest` names, Ollama `keep_alive`.
- `/v1/messages/count_tokens`, `/v1/chat|completions|responses|embeddings|audio/transcriptions|audio/speech|images/generations`.
- `/v1/system-stats`, `/v1/juiced/state`, `/v1/juiced/requests`, `/v1/juiced/protocols`.
- `LEMONADE_API_KEY` alone (operator access in both).
- `Authorization: Bearer` and `x-api-key`.
- Native config, options, aliases and companion adoption.
- `ctx_size` and `llamacpp.backend` mapping.
- Console: policy board, context fit, quant sync, removal, HF search/pull, downloads, cache modes, logs, Connect, speech/image/chat tests.

## Excluded by the approved plan (not counted)

| v0.9.2 behaviour | Reason | Rewrite must still |
|---|---|---|
| Local preparation: HF→GGUF conversion, requantization (`/juiced/preparation`, prepared copies) | PLATFORM-FEATURES F019, F020, F022-F027, F033, F035, F204; PLAN.md:58, 111 | Keep existing prepared files as ordinary weights. Optionally answer `/juiced/preparation` with an explicit 410/501 |
| Caller-defined jobs / legacy graphs (`POST /jobs`, pause/interrupt/resume) | Inventory `excluded`; PLAN.md:58 (no generic graph execution) | Return v0.9.2's 501 (P1-16) |
| Stored conversations (`/juiced/threads`, `/juiced/thread`) | F193; PLAN.md:9 | Return v0.9.2's 410 (P1-16) |
| `collection.omni` models and MCP `lemonade_omni` | v0.9.2 Juiced already returned 501 and hid them; PLAN.md:85 | Nothing |
| Cross-engine KV snapshot conversion | F114 | Nothing |
| Prompt rewrite presets, wakeword/satellite, training, RAG workspace, creative gallery | F145, F155, F195, F196, F174 | Nothing |

Rewrite-only redesigns are not counted as gaps: the console layout, typed `/juiced/*` routes, the Barista drawer and API-key scopes.

## Proposed implementation batches

The batches are file-disjoint so they can run in parallel. `http.rs` route-table lines are shared. Each batch adds only its own `(get|post, ...)` rows plus one `.merge(...)` line, and edits nothing else there. Each batch updates `api-parity/inventory.json` for its own rows only. The primary agent merges the inventory counts. Base: rc.12 plus the helper-discovery merge (P0-12 to P0-15).

**B1 Protocol codecs (P0-1, P0-2, P0-3, P0-4, P0-6, P1-15, P1-16):**
- Files: `R/protocols.rs`, `R/compat.rs`, plus a new `R/docs_api.rs` for `/v1/docs` (bundling `docs/api` pages).
- Thinking normalization for OpenAI bodies happens in `protocols.rs`, called from the existing chat forwarder (one call site).
- Anthropic and Ollama become ignore-with-warning, as in v0.9.2.
- Ollama 501 stubs for create/copy/push/blobs/pull/delete.
- MCP `lemonade_transcribe_audio`, `lemonade_generate_image` and `lemonade_docs` are added.
- jobs 501 and threads/thread 410 are restored.
- Checks:
  - rust-tests send v0.9.2 bodies: `metadata`, `cache_control`, `thinking.budget_tokens`, `service_tier`, `min_p`, `repeat_penalty`, `system`.
  - Upstream receives `chat_template_kwargs.enable_thinking=false` for each off form (fixture capture), plus a real Ornith or Qwen3 check that reasoning is empty.
  - MCP `tools/list` is a superset of v0.9.2's five minus omni.
  - The 501/410 bodies match v0.9.2.

**B2 llama.cpp passthroughs and small native routes (P0-7, P0-8, P0-9, P0-11, P1-12, P1-13, P1-18):**
- Files: a new `R/llama_routes.rs`, a rerank operation in `R/config.rs` (one enum variant), and route rows in `http.rs`.
- Routes: `/rerank|reranking|reranker`, `/tokenize` (optionally `/detokenize`, which v0.9.2 did not have), `GET /slots` and `POST /slots/{id}?action=` mapped onto the session cache owner, `/stats`, `/audio/voices`, bare `/realtime`, `/live` (unauthenticated).
- Checks:
  - Real llama-server (Stories15M) tokenize round trip; slots save/restore/erase against a real worker; a rerank fixture plus a real bge-reranker GGUF if cached.
  - The WS upgrade on bare `/realtime` returns 101.
  - `/live` returns 200 with keys set.

**B3 Image surface (P0-5, P0-10, P1-26):**
- Files: `R/media.rs`, a new `R/a1111.rs` (merged router), `R/native_import/options.rs` (image defaults only).
- A1111 GET subset, POST options, 501 for img2img and interrupt.
- `images/edits` and `variations` (multipart → sd-server img2img), `images/upscale`.
- Image defaults stay editable through `/models/{m}/options`.
- Checks:
  - The SillyTavern A1111 probe sequence (`sd-models` → `samplers` → `options` → `txt2img`) passes against a real sd.cpp tiny model, or the cached z-image on the homelab class.
  - An OpenAI SDK `images.edit` call succeeds.

**B4 Operations and auth (P1-17, P1-19, P1-20, P1-21, P1-22, P1-23 doc only):**
- Files: `R/logs.rs`, `R/auth.rs`, a new `R/metrics.rs`, and the CORS layer lines in `http.rs`.
- Log lines are mirrored to stderr, with a level from `LEMONADE_LOG_LEVEL`/config.
- `/metrics` Prometheus with the v0.9.2 metric names.
- `WS /logs/stream`.
- `allowed_origins` + `LEMONADE_ALLOWED_ORIGINS` with v0.9.2 semantics (403, `Vary`, explicit allow-headers, PNA).
- Admin-key-only keeps inference and read open, as in v0.9.2.
- Checks:
  - `podman logs` shows service and worker lines.
  - The preflight with `authorization` from a foreign origin is 403, and from an allowed origin it echoes the origin and lists `Authorization`.
  - The v0.9.2 key matrix (none / api / admin / both) matches.
  - Prometheus text parses.

**B5 Native management compatibility (P1-1 to P1-11, P1-14 aliases/pin/shutdown, P1-33):**
- Files: a new `R/native_api.rs` (adapters onto library pull, artifact jobs, engines, registration), `R/supervisor/engines.rs` (uninstall), `R/bin/lemonade.rs`, `docs/guide/cli.md`.
- Adds `/pull` with SSE, `/downloads(+control)`, `/registry/search`, `/pull/variants`, native `/models/register`, `/delete` with `model_name` (weights removed only when unreferenced), `/load` with options + `save_options`, unload-all, `/models/{id}/files`, `DELETE /models/{id}/options`, `/install|install/dry-run|uninstall`, `/system-info` `recipes` block, `/internal/aliases`, `/internal/pin`, `/internal/shutdown` (admin).
- CLI `pull MODEL`, `load` flags, `backends install/uninstall`, `logs`, default port 8080 inside the container.
- Must not touch `/v1/models` (helper-discovery).
- Checks: the v0.9.2 `lemonade` CLI binary from the v0.9.2 image runs `list`, `pull`, `load`, `unload` and `delete` against the rewrite; `docs/api/lemonade.md` examples replay.

**B6 Model settings and console (P1-24, P1-25, P1-31, P1-34, P1-9):**
- Files: `R/profiles.rs`, `R/launch_plan.rs`, `R/runtime_arguments.rs`, `R/native_import.rs` (per-backend args only), `R/cache.rs` (settings), and `ui/src/*` (Models inspector, Test services, Cache, Diagnostics, Engines).
- Typed LaunchOptions: sampling defaults (`--temp`, `--top-k`, `--top-p`, `--min-p`, `--repeat-penalty`, `--reasoning-budget`), batch, ubatch, flash-attn, KV cache type, free-form extra args, image and asr4all options.
- Console editor, bulk actions, README link, max tokens + thinking toggle, mic/any-format audio, cache retain/expiry/budget, engine versions.
- Depends on B3 for the image defaults API shape; B3 lands first or both agree the field names up front.
- Checks: the saved options appear in the real llama-server argv; Playwright journeys at 3 widths; screenshots reviewed.

Later (not batched): P1-27 real NPU/hybrid qualification on mage; P1-28 ROCm matrix, P1-29 catalog and P2-1 engines in the engine-management round; P2 rows by user choice.

## Notes

- Counts: P0 15 (4 owned by helper-discovery), P1 34, P2 14, excluded 6 groups.
- P0 includes documented contracts with no confirmed user traffic (rerank, slots, tokenize, image edits, MCP media tools). The user may want to move those to P1. The ones with a confirmed or likely client in the user's setup are:
  - P0-1: HA Ollama `think`, Ornith reasoning.
  - P0-3: Ollama options.
  - P0-5: SillyTavern A1111 mode, if used.
  - P0-12 to P0-15: Pi.
- Console gap detail for P1-34, all v0.9.2 `juiced/ui/src/components`:
  - Rows 1-4 of that audit are folded into P1-24.
  - Row 7 (preparation) is excluded by the plan.
  - Rows 9, 10, 11, 13, 14, 15, 16, 18, 19 and 20 make up P1-34.
- Not verified live: Wyoming streaming flags (rc.12 advertises `supports_*_streaming:false`; equal to the captured 0.8.0 identity per the drop-in ledger). Also browser CORS behaviour and NPU exclusivity.

## Next action

User direction 2026-11-12: close all v0.9.2 gaps (no P0/P1 cut needed), then the planned additions. P0-12 to P0-15 merged from helper-discovery (19ed3cf4), shipping as rc.13. Running from f090e527: B1 helper-b1, B2 helper-b2, B3 helper-b3, B4 helper-b4, B5 helper-b5. B6 starts when B3 publishes its image-options shape (scratchpad note b3-image-options-shape.md). Then P2 rows and the engine-management round.
