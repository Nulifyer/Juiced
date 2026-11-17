# Gaps and console UX audit (November 2026)

Status: v0.11.4 released and deployed (2026-11-16); 1af14d5e unreleased
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

## Release (2026-11-16)

- v0.11.0 (PLAN.md edit broke its approval binding) and v0.11.1 (five new Rust routes not in api-parity/inventory.json) stopped at release checks; no image. PLAN.md was restored; the capture exception lives in this ledger's Decisions. v0.11.2 published (run 43894343974) and deployed.
- Live on v0.11.2: Hot models back in about 1 min; Ollama tags/show/ps carry details (Coder-Next `tools`, `num_ctx 131072`, arch qwen3next); automatic checks passed on both chat models; request records tag protocol (`openai`, `internal` for checks). Found two bugs: Ollama refused bodies without a JSON content type (`curl -d`), and the 890M busy counter read 100 with models loaded idle after reading 0 at startup, so D's rule still showed 100.
- Tune on Coder-Next live (reworked flow, first real run): baseline 340.8/23.1/51.8 (prefill/decode/rewrite tok/s); ubatch 1024 365.0/22.9/49.5; 2048 390.9/23.1/51.2 (kept); flash attention 361.1/23.1/50.9; speculation off 394.5/23.0/22.7. Saved options and extra_args unchanged (revision 169 to 189), tuned layer 1024 -> 2048 without the trial marker, reservation +1.2 GiB, post-Tune check passed.
- v0.11.3 (c8c2e0e3 fixes; run 43901811182 green; release page https://github.com/Nulifyer/Juiced/releases/tag/v0.11.3) deployed to stack 83: 4 Hot models in about 1 min; untyped Ollama chat and show work; GPU reports null with `amdgpu-sysfs-unverified`; release check current v0.11.3. Production console checked read-only in both themes (0 non-GET requests). Homelab stacks/juiced/compose.yaml pins v0.11.3 (untracked, as before).

## Follow-ups (2026-11-16, after v0.11.3)

- Tune answer checks (85650300): trials run at temperature 0 with a fixed seed; a trial that loses an answer the current settings got right (hidden prompt marker, exact count, exact rewrite) is not kept; a kept change below 80% text agreement warns. Live Coder-Next and Qwen3-4B pass all three checks on current settings. Depth benchmark on Coder-Next (43K/73K tokens): flash attention on equals auto; off exceeds the 80 GiB budget; q8_0 KV is 28-35% slower at decode; settings restored to revision 193 (same saved, extra_args, defaults and tuned layer).
- helper-fable-quants 2e137512 (fast-forwarded): Discover quant list no longer jumps (scroll re-centering, label-keyed quants, refresh status line, sticky detail); the inspector Overview lists a model's quants with Open/Add/Get. Open: exact repository lookup needs a `repo=` parameter on `/juiced/library/search` (search ranking is relied on today).
- Exact repository lookup (ada4ab63): `GET /juiced/library/search?repo=owner/name`; a missing repository is not a hub failure; the inspector quant list uses it.
- v0.11.4 (a15226b0; run 43965732856 green; https://github.com/Nulifyer/Juiced/releases/tag/v0.11.4) deployed to stack 83: 4 Hot models loaded; Coder-Next still revision 193 with the same layers; repo lookup 0.1 s live (Coder-Next 38 quants, 1 on disk; Qwen3.6-35B-A3B 24, 4 on disk; a missing repo returns none with status ok); the Quants section renders on production (read-only, 0 non-GET requests).
- Found live: part 1 of 3 of a UD-Q6_K_XL download showed as a 5.7 MiB file with Add. Fixed after the release in 1af14d5e (shows parts on disk and Finish).

## Follow-ups (2026-11-17, v0.11.5)

- 597a6623: byte-progress checkpoints no longer bump the artifact generation, so a second Discover download no longer fails with `artifact_revision_conflict` while one runs; the console retries a stale generation once.
- Live failed pull (unsloth/Qwen3-Coder-Next-GGUF UD-Q6_K_XL): parts 2 and 3 failed `artifact_network` at 19.6 GB/50.0 GB and 13.5 GB/23.2 GB. e965313b: a dropped connection resumes up to 6 times over about 9 min; a new pull adopts a network-failed job with bytes. e65fd435: `POST /juiced/library/pulls/{id} {"action":"resume"}` restarts a failed pull in place and re-arms its failed `then`; Downloads shows Resume for it (Retry still starts over).
- helper-fable-models 4b3e2f08 merged (6f8bc9f6): one quant row (QuantList.tsx) across Discover, inspector and file details; Installed rows 37 px, "All" counts models, unused files under Not added; one row per split pull; plain error text (modelErrors.ts). Open from Fable: inspector Overview unchanged apart from Quants; `pull.error` is generic.
- Gate on 40177e10: 895 service tests and pull-route rust-tests pass, clippy and fmt clean, 156 UI unit tests, tsc, build, Playwright 360 passed / 21 skipped / 0 failed (desktop, tablet, phone). v0.11.5 (run 44092841204) stopped at console-checks: on CI fonts the selected quant's bolder name wrapped (Qwen3-0.6B-BF16 beside "Downloading 0%"), the list lost 4 px and the clicked row moved. Fixed: selection keeps the font weight; the test also asserts the list height is unchanged (negative check: wider selected name fails 872 vs 891). v0.11.6 (run 44096797766 green) deployed to stack 83: 4 Hot models loaded, Coder-Next revision 193 unchanged; production Downloads (read-only render, 0 non-GET) shows Resume, Retry, Cancel on the failed UD-Q6_K_XL row. Same render showed 5.7 MiB of 68.1 GiB (failed parts lose their job link, so their 33 GB were not counted) and advice to Retry; fixed (pullBytes counts the matched job's bytes; network failure text points to Resume) and released as v0.11.7 (run 44101785542 green), deployed to stack 83: 4 Hot models loaded, Coder-Next revision 193; the row now reads 30.8 of 68.1 GiB, "The connection dropped. Resume continues from the downloaded part." with Resume, Retry, Cancel (read-only render, 0 non-GET). Resume not pressed: production is GET-only for helpers; the operator starts it.

## Follow-ups (2026-11-17, v0.11.8)

- User asked why Discover had a separate NPU section, for hardware/runtime/size/type filters, what "Not added" means, and for searchable model dropdowns.
- helper-fable-discover 9ae58753 (merged): FastFlowLM models are result cards with the shared detail panel (FlmModels.tsx removed); one filter bar Type, Engine, Hardware (fits GPU / fits memory / NPU), Size, Sort, with count and Clear filters; Type and Engine go to the service, Hardware, Size and Sort are client-side per quant. Installed "Not added" has a tip ("Model files on disk that no model uses...") and the files header "Files no model uses".
- helper-fable-picker dab11fcf (merged): one searchable ModelSelect (ModelSelect.tsx, modelPicker.ts) replaces every model select, raw ones in Cache and Try a model included; search by ID, type, label, engine, state; Loaded/Enabled/Disabled groups; Disabled behind one row in lists over 8; type chips on mixed lists; no virtualization (2000 models: open 122 ms).
- Gate on the merge: 167 UI unit tests, build, Playwright 366 passed / 21 skipped / 0 failed (3 projects). No Rust change.
- Open: Discover filters apply to the fetched page only (service returns up to 12 per source; `next_cursor` unused); Storage still says "unused" for Not added files; FastFlowLM non-chat checkpoints stay hidden (`flm_model_unsupported`); picker search has no publisher/repo field.

## Next action

None for this audit. Open from earlier ledgers: the later-work list in drop-in-replacement.md.
