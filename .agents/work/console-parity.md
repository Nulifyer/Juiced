# Console redesign

Status: active
Owner: primary agent (UI files, integration, this ledger)
Repository: /home/nulifyer/Projects/Juiced-rust-rewrite, branch explore/rust-bun-rewrite
Request source: user, 2026-11-11. Console "looks very verbose"; prefers clean and compact; features missing versus v0.9.2. Then: "this is a rewrite so we don't need to respect anything as far as the UI goes… make it GREAT rather than tons of redundant buttons, weird splits in search, random folding sections… a proper great UI/UX experience to go along with our newer really fast and efficient backend." User likes the page split (testing services, backend & hardware, cache, loading policy, dashboard, diagnostics).

## Principles

- Live data everywhere; no manual refresh buttons (retry only on failure).
- One search per job (Library: catalog + Hugging Face + local in one box, chips for filters).
- No folding sections; sub-views use one pattern: page title + tab strip; detail in one side inspector.
- One primary action per view; destructive actions live in the inspector.
- Short task-level copy; explanations in info tooltips only when needed.

## Pages

Dashboard · Models · Loading policy · Library · Backends & hardware · Cache · Test services · Diagnostics · Connect.

## Acceptance checks

- [ ] Shell: destinations-only sidebar, topbar status menu (pause/resume, connection), downloads indicator, live footer metrics.
- [ ] Dashboard: live CPU/RAM/GPU/VRAM, loaded models with tok/s, attention items, recent requests.
- [ ] Models table with search + service chips, inline load/unload, inspector tabs; Loading policy board with drag and drop plus keyboard path.
- [ ] Library: unified search, quant chips, inline download progress, one-step add (needs helper C, then engine auto-select from helper D).
- [ ] Backends: hardware cards + backend availability/installed/reason + install (helpers A, D).
- [ ] Test services: streaming chat thread + transcribe; Diagnostics: performance charts, requests, live logs; Connect: per-protocol URLs and snippets.
- [ ] Browser journeys rewritten for the new UI at desktop/tablet/phone and 200% text; Bun tests pass; screenshots reviewed; full gate passes; committed.

## Workstreams

| Stream | Owner | State |
| --- | --- | --- |
| UI rebuild | primary | committed 17b21763 (all pages) and 8741e2a2 (one-click add, Engines tab); real search→download→add(auto CUDA, engine install)→chat verified |
| A: hardware detection + backend availability (`GET /system-info`) | helper | merged af388472 |
| B: `/system-stats`, `/juiced/logs`, request token stats | helper | merged 75a17e0e |
| C: library search + HF pull (`/juiced/library/search`, `/juiced/library/pull`) | helper | merged 7de93910; real 364 MB Qwen3 Q4_0 pull verified via UI in 33 s |
| E: new Playwright suite for the redesigned UI | helper | merged 5bd44622; 116/117 pass (1 desktop-only skip) on alternate ports |
| F: model aliases + options `apply: "reload"` | helper | merged 42e452b3; UI 7da8f87a (Aliases tab, Save and reload); real CUDA reload 2.3 s verified |
| G: automatic chat session continuation (`/juiced/cache/sessions`) | helper | merged 78935bd1; UI 8421c71d (Cache › Sessions); real CUDA: restore 59–93 ms, 1–18 new tokens processed; removal forgets sessions 59a3768d |
| H: sd.cpp images + Kokoros TTS, `/images/generations`, `/audio/speech`, `/sdapi/v1/txt2img`, `/juiced/voices` | helper | merged 3364a15b; UI 23e0c3d5 (Speak, Images); real RTX 5080: image 0.6 s warm, speech 4.9 s audio in 3.4 s cold |
| I: journeys for add-with-engine-install, compare, embeddings | helper | merged; its 3 defects fixed 9648bce8 |
| J: idle unload, pressure eviction, Ollama keep_alive (`/juiced/residency`) | helper | merged 025c9186; UI 8b5e9330 (Loading policy lane select, inspector override); real llama-server eviction and idle unload verified |
| K: Wyoming server (10300) and Realtime transcription WebSocket | helper | merged c58b0537; UI 85af6ddf (Connect › Wyoming card); real whisper and Kokoros round trip; official wyoming client parsed info; no real HA instance yet |
| L: model upload and server import folder (`/juiced/library/upload`, `/juiced/library/import-path`) | helper | merged 2e5811b5; UI 4980a4bc (Library › Add file); real upload → add → chat verified; split GGUF and mmproj refused |
| M1: parakeet.cpp, Moonshine, FUTO asr4all | helper | merged 25dc0aee; real transcription for all three; FUTO has no published build |
| M2: FastFlowLM, llama.cpp hybrid-rocm, NPU/memlock detection | helper | merged 52e7b38e; UI b1c91b98 (Library NPU section); fake hardware only, real archives checksum-verified |
| N: online v0.9.2 import (`/juiced/import/native`) | helper | merged 29f282b0; UI 22e1d1e4 (Library › Import); real preview against local 0.9.2 volume, volume unchanged |
| O: FUTO built into the container and imported on first boot | helper | merged; real FUTO transcription in the container; image 484 MB |

## Notes

- Interim compact pass (uncommitted in ui/) is superseded by this rebuild; 22/26 desktop journeys passed on it.
- Reference v0.9.2 container `juiced-v092-ref` on 127.0.0.1:18090 (local volume, not homelab).

## Research findings (plan + threads, 2026-11-12)

User decisions to keep: models Disabled by default "until I add them to on-demand or hot" (thread bfcf@7678); no saved chats or conversation browser; no Juiced-owned quantization; compact UI with (i) tips, no folding; Home Assistant over Ollama and Wyoming; container image with build/runtime stages and runtime-updatable drivers; hardware-first automatic backend choice with manual override.

Open, ranked: (1) Rust-rewrite container image (juiced/Dockerfile still builds the C++ lemond); (2) more engines: hybrid ROCm+NPU, FLM/RyzenAI, FUTO, parakeet, moonshine; (3) Wyoming, Realtime; (4) residency: idle TTL, pressure eviction, protected HA reservations; (5) manual GGUF upload; (6) migration import from v0.9.2 native settings, matched benchmarks, review council. Doc drift: PLAN.md/PLATFORM-PLAN.md say eight destinations; UI-DESIGN.md describes the native console; FEATURE-COVERAGE.json predates the redesign.

Decided 2026-11-12: Add shows a policy choice (Disabled / On demand / Hot), preselected On demand (ab1759f9).

Container: merged 39d38274 (juiced/rewrite/container). Image 473 MB; CPU and RTX 5080 (CDI) smoke tests pass; CUDA llama.cpp install and chat inside the container verified. Not verified: ROCm, Intel/AMD Vulkan hardware, NPU, Docker builds.

## Next action

Superseded as the coordination record by [drop-in-replacement.md](drop-in-replacement.md), which holds pending work since rc.3. Status of the open items listed on 2026-11-12 14:24 UTC: ROCm on the homelab Strix verified (rc.3 onward); real Home Assistant Wyoming verified (rc.3); native_store owner_busy flake fixed (11a7b166); real AMD NPU check still open; split GGUF and mmproj upload/import still refused; transcription_rejected 400 mapping for parakeet/FUTO not rechecked.

Fixed 2026-11-12: `GET /juiced/engines` returned 503 `runtime_index_publishing` when an engine install was publishing its index; the inventory read now waits up to 3 s (52e7b38e). The console now has eight destinations (PLAN.md line 9, 20a5c1a3).
