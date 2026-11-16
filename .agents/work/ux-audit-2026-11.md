# Gaps and console UX audit (November 2026)

Status: integrated, not released (2026-11-16)
Owner: primary agent (integration, release, this ledger); helpers own their batches below
Request source: user, 2026-11-16. "Ask fable to do some research ... find if there is anything missing from this, or if the ui/ux needs work", then "Address all it's feedback".

## Input

Fable's report (research only, production read-only): scratchpad `research-ux-gaps.md`, evidence in scratchpad `ux-shots/` (session 50476ad2). Sections: 1.1-1.6 missing capabilities, F1-F25 console findings, 2.3 names table, 2.4 copy cuts 1-31. The P0 (1.1, Ollama `/api/show` and `/api/tags` thinner than v0.9.2) was confirmed by the primary agent in code (service/src/protocols.rs `ollama_show`) and on production.

Base: explore/rust-bun-rewrite at af1a3a90 (v0.10.0 plus the unreleased post-release review fixes; full gate green on it: cargo test, clippy, 136 UI unit tests, Playwright 296 passed).

## Decisions (primary agent, 2026-11-16)

- 1.6b request capture: opt-in, operator-only, off by default, memory only (bounded count and size), cleared when turned off, never in logs, metrics or telemetry. A documented exception to the PLAN.md "no prompts" telemetry rule, which still holds by default.
- 1.6a release notice: the service checks GitHub releases at most once a day, fails silently offline, and can be turned off in Settings. The console shows a quiet sidebar note.
- 1.6d load progress: elapsed time against the last successful load of the same model and engine. No fake percentage when there is no history.
- 1.6e light theme: follows the system, with a per-client Auto/Dark/Light override kept in the browser (invariant 1).
- 1.5 Update: downloads the new revision of the model's files and switches the model to them, keeping its settings; old files show as unused in Storage.
- Names follow the report's 2.3 table.

## Batches

| Batch | Scope | Owner | State |
| --- | --- | --- | --- |
| A | Ollama details (1.1); request attribution, dates, persistence, opt-in capture (1.2, 1.6b, 1.6c, F9, F10); Diagnostics copy 22-23 | helper-ux-a | merged 31d270df, ab7d23f8 |
| B | Models and inspector (F1-F4, F11, F18, F19), Slots (1.3), load progress (1.6d), missing-weights copy, copy 7-19, Enabled/Disabled chip | helper-ux-b | merged ed09b964, a28b6ac9 (merge f662b0b3) |
| C | Discover and add (F5), URL download (1.4, F6), Update action (1.5), add-failure copy (F12), units (F22) | helper-ux-c | merged 05152745, f435ffdd, 980b4b94 |
| D | Shell, Dashboard, Connect, Cache, Storage, Barista (F7, F8, F13-F17, F20, F21, F23-F25, copy 1-6, 20-21, 24-31), release notice (1.6a), light theme (1.6e) | helper-ux-d | merged d62d491e, de8fe0d3, ae7e45dc (merge 5414de60) |

## Acceptance

Every report item is either fixed with evidence or listed below as decided otherwise. Full gate passes on the integrated branch (cargo test, clippy -D warnings, fmt, UI unit tests, build, Playwright). Docs under docs/ match changed behavior.

## Results

- All report items fixed. Corrections to the report: F17 was a stuck AMD busy counter (90 production samples at 100% with no requests), now reported as no reading until the card shows less than 100; two F24 contrast misses came from opacity, not color; 1.3 Slots was refused by the service (`slot_resize_requires_registration`), now saving Slots resizes request slots on llama.cpp models (memory-protected still refuse).
- Integration fixes: Slots shows "each request up to N" beside the field (context is a total budget split across slots unless `--kv-unified`); bulk policy changes keep rows visible under the Enabled filter; capture colors on theme tokens; GPU memory named the same in footer and Diagnostics; tablet policy column fits "On demand"; sidebar link 24 px; Connect examples wrap on phones; route-count fix in rust-tests.
- Gate on the integrated branch: clippy -D warnings clean, fmt clean, 1264 Rust tests passed (75 suites), UI type check and 148 unit tests pass, build ok, Playwright 342 passed / 21 skipped / 0 failed (before the last naming and column fixes; diagnostics, dashboard, models, shell and residency specs re-run green after them). Screenshots of Dashboard, Models and inspector in both themes at 1440/900/390 checked by eye.
- Not checked live: Tune states, a real load failure, engine install progress, a Barista conversation (same limits as the audit). Not released or deployed.

## Next action

Release as v0.10.1 (or v0.11.0, given the new features) after the user agrees; deploy only after a green release build. Then re-check production: Ollama `/api/show` capabilities, GPU reading on the 890M, release notice, light theme.
