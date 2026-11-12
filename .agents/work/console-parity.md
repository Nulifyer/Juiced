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
| G: automatic chat session continuation (`/juiced/cache/sessions`) | helper worktree | running |
| H: sd.cpp images + Kokoros TTS adapters and engines | helper worktree | running |
| I: journeys for add-with-engine-install, compare, embeddings | helper worktree | running |
| D: engine install from pinned builds + model→engine auto-select (`/juiced/engines`, `/juiced/models/add`) | helper | merged 2e9ee2ae |

## Notes

- Interim compact pass (uncommitted in ui/) is superseded by this rebuild; 22/26 desktop journeys passed on it.
- Reference v0.9.2 container `juiced-v092-ref` on 127.0.0.1:18090 (local volume, not homelab).

## Next action

Primary: merge G, H, I as they report; then Cache sessions UI (slot hit / restored / cold, forget), Speak and Images tabs in Test services; re-anchor inventory evidence; run Rust suites, Clippy, Bun, full e2e (`JUICED_E2E_PORT=13317 JUICED_SOURCE_PORT=13318`), and the full gate; commit.

Verified 2026-11-11 on RTX 5080: Settings save-and-reload, engine/weights controls only shown when actionable (a01abf0b), wide-screen column fix (c05b1883), Library chips 10/10 loads settle, button sweep across every page/tab with no page errors or 4xx/5xx, real two-model Compare.
