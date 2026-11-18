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
- v0.11.8 (run 44121549419, cancelled) failed the plan check: the picker merge edited juiced/UI-DESIGN.md, a hashed native source (`updated_in_rewrite: false`). Restored it; the picker convention lives in juiced/rewrite/README.md "Console". The release chain now stops on any failed check. v0.11.9 (run 44121705950 green) deployed to stack 83: 4 Hot models loaded, Coder-Next revision 193; production render (0 non-GET): five Discover filters, Fits GPU 78 -> 39 results; the Aliases picker finds 3 of 53 for "coder".
- Open: Discover filters apply to the fetched page only (service returns up to 12 per source; `next_cursor` unused); Storage still says "unused" for Not added files; FastFlowLM non-chat checkpoints stay hidden (`flm_model_unsupported`); picker search has no publisher/repo field.

## Follow-ups (2026-11-17, v0.11.10)

- User asked where else the searchable picker fits and wanted the rule in frontend-design. The primary agent's skill draft was rejected as project-specific and reverted (diff kept in scratchpad `rejected-inputs-context.diff`); Fable and Astra returned independent generic proposals (picker vs select vs radios/segments vs typed entry vs jump search; NN/g 5-15 heuristic). Not applied; waiting on the user.
- 7ccc28ff, 1d8e9dcc: generic `Picker` (Picker.tsx) under ModelSelect, VoiceSelect (Kokoro voices by language with gender, OpenAI alias search), the weights switch (file, quant, size; model's repository first) and the engine switch (compute path, size). One matcher `wordsMatch` (search.ts) for Installed, pickers, NPU results and the endpoint filter; it matches from word starts ("male" no longer finds "Female"; mid-word queries such as "wen" no longer match). Home/End edit the search once text is typed. Group ids no longer break on keys with spaces.
- Gate: 172 UI unit tests, tsc, build; Playwright 363 passed / 21 skipped before the last two fixes, then media, model-picker, models and connect specs 73 passed on all three projects.
- v0.11.10 (run 44157609192 green) deployed to stack 83: 4 Hot models loaded, Coder-Next revision 193; production render (0 non-GET): kokoro-v1 voice picker 54 of 54 in nine language groups, "british male" gives 4 of 54 (bm_daniel, bm_fable, bm_george, bm_lewis), Escape keeps af_alloy.

## Follow-ups (2026-11-17, v0.11.11)

- Skill: Fable and Astra agreed (4 rounds, log in scratchpad `skill-collab/log.md`) on a generic "Choices and fields" rule for frontend-design plus `references/choices-and-fields.md`; uncommitted in `~/.agents` for the user's review.
- e7 batch (commit before af096a5b): an empty setting's hint names the value in effect and its source, including Juiced's applied defaults (Physical batch read "Default 512" while a measured `--ubatch-size 2048` ran); engine defaults read "N (engine default)". The snapshot size limit takes sizes ("1 MiB", "2 GiB") and shows what a byte count comes to.
- Gate: 172 UI unit tests, tsc, build, Playwright 366 passed / 21 skipped / 0 failed (3 projects). v0.11.11 (run 44171188188 green) deployed to stack 83: 4 Hot models loaded, Coder-Next revision 193; production render (0 non-GET): Physical batch "2048 (measured here)", Batch "2048 (engine default)", Temperature "1 (model file)".
- Open: GitHub release pages exist only up to v0.11.4 (later tags published images only), so the release notice reports latest v0.11.4. Threads and Prompt threads still read "Engine default" (the service reports no number for them).

## Follow-ups (2026-11-17, v0.11.12)

- User report: after Resume, UD-Q6_K_XL (3 parts, 68.1 GiB) showed Not added with Add model and no reason. Production (GET): pull complete, `then` add failed `artifact_deadline`. Cause: `hold_binding_with` rehashes every set member under `set_operation`'s flat 30 s; `hold` for a single artifact has no limit. Fix: `verify_budget(bytes)` = 30 s + bytes / 32 MiB/s for set holds (68 GiB: about 37 min).
- Console: File details shows the failed add ("The download finished, but adding it failed. Checking the files took too long. Add the model again."), a large add says it is checking the files, the heading drops "Not added" when any quant is in use, the empty "Used by" row is gone; Downloads words the failed step through `thenFailure`.
- Gate: 910 service tests, clippy, fmt; 172 UI unit tests, tsc; library, add-model and model-quants specs 51 passed (3 projects). v0.11.12 (run 44180838599 green) deployed to stack 83: 3 Hot models loaded (the user set Coder-Next Q4_K_M to On demand, revision 196, saved options unchanged); production render (0 non-GET) shows the notice and both rows. Add model not pressed (GET-only).
- Open: add reads the set twice (inspect, then register) and startup rehashes stored models, about a minute per 68 GiB by estimate, unmeasured on production. A complete pull whose add failed has no retry action of its own; File details' Add model covers it.

## Follow-ups (2026-11-17, v0.11.13 and v0.11.14)

- User: "not sure if we should have such a time limit on the model files to add". v0.11.13: holds, companion set publish and the library's post-download publish use `SetOperation::unbounded` (cancellation and shutdown still stop them); `verify_budget` and the 3600 s `PUBLISH_DEADLINE` removed; the set publish API keeps its request-bound limit. 910 service tests, clippy, fmt.
- The user added `Qwen3-Coder-Next-UD-Q6_K_XL` on v0.11.12 (On demand, 32768 context, revision 198); the add fix worked on production. Its Downloads row still read Not added with the old failure beside Open model; v0.11.14 settles a failed add once a model uses the files (`addedSince`).
- v0.11.13 (run 44185858059 green) not deployed. v0.11.14 (run 44187406224 green) deployed to stack 83: service unavailable for 60 s with the 68 GiB set registered, about the same as before it; 3 Hot models loaded; both Coder-Next models unchanged; production render (0 non-GET): the Q6 row reads Done, Open model, Dismiss.
- Memory: Q6 reservation 70.5 GiB plus 7.7 GiB of Hot models against the 80 GiB budget. Not loaded by the primary agent; offered to the user.
- Open: add reads a set twice and a load rehashes the model's main file; caching digests by file stamp (as `identity::file_digest` does) is proposed, waiting on the user.

## Follow-ups (2026-11-18, v0.11.15)

- User: no "Add model" or "Dismiss" ("I should download a model/quant to my local storage, or I should be removing it. Downloading is a temp-status"); Discover should be one column with filters that do not move and open on a short suggested list. Decisions taken from those words (the question dialog was dismissed): unregistered usable files become Disabled models automatically; Remove deletes a model's files after one confirmation.
- helper-fable-discover2 `6ce1a4e5`, `67f05bd4` (fast-forwarded): one search bar above list and detail (same place in every state), a one-column row list (one quant, size, fit, one action per model), up to six suggestions by default (`suggest` in libraryData.ts; on-disk models left out), search text or any filter searches the service, Show more follows `next_cursor`, new `source=catalog` on `/juiced/library/search`. Causes fixed: an 1800 px two-column rule; the bar inside the column the detail narrows; the bar hidden under 960 px with a detail; selection by repository left an empty detail column or blank page for catalog models sharing a repository (seen on production with Whisper).
- Gate on the merge: 911 service tests, clippy, fmt; 177 UI unit tests, build; Playwright 381 passed / 21 skipped / 0 failed (3 projects). v0.11.15 (run 44208920979 green) deployed to stack 83: 4 Hot models loaded (the user added `Qwen3-Coder-Next-UD-Q4_K_XL` as Hot); production render (0 non-GET): 5 suggestions in one column at 1920 and 1440.
- Open: helper-fable-lifecycle (download or remove; automatic registration; Downloads lists only active and failed) still running. From discover2: no NPU suggestion; rows go from one line to two with the detail open at 1440; two long select labels clip at 390; rows still say "Not added" / "Add model" until lifecycle lands.

## Follow-ups (2026-11-18, v0.11.16)

- helper-fable-lifecycle `8fd8e56b`, `89b8afdf` merged (dd327106) onto the Discover suggestions work: `auto_models.rs` registers unused complete model files as Disabled (startup, after a download, after an engine install, at most every 30 s while the catalog is listed); `model_removal.rs` with `GET /juiced/models/removal` and `POST /juiced/models/remove` deletes only files this model alone uses and refuses loaded, busy, alias-target, read-only and not-owned cases; pulls list only in-progress and failed, with `retry` and `remove` actions; the console has no Add model, Not added or Dismiss; non-model files show in Storage as "Not a model".
- Merge: three textual conflicts (FindModels.tsx, library.spec.ts, add-model.spec.ts); two tests followed the other helper's screen (quant chip in a row, toast wording) and were updated.
- Gate: 943 service tests, 336 rust-tests (32 ignored), clippy, fmt; 178 UI unit tests, tsc, build; Playwright full run 375 passed / 6 failed (the two tests above, 3 projects), then add-model and library specs 54 passed; release CI green.
- v0.11.16 (run 44219455229) deployed to stack 83. Production before/after (GET): 55 -> 71 models; the 16 new ones are all Disabled (KAT-Coder x2, MiniCPM5 x2, Ornith, Qwen3.5-122B x2, Qwen3.5-4B, Qwen3.6-27B x2, Qwen3.6-35B, Skyfall x3, ThinkingCap x2); no existing model's policy or revision changed; 83 files, 1104944306483 bytes, none gone or resized; pulls list empty; 4 Hot models loaded. Render (0 non-GET): Installed filters "Enabled 7, Disabled 64", Downloads "Nothing downloading".
- Open (from the helper): models on read-only or shared caches can only be Disabled; a Juiced 0.9 volume in a non-standard layout is registered before its settings import; the upload panel keeps one Install step; `POST /delete` with `expected_revision` keeps files; Barista downloads become Disabled models; scan failures retry on their own with no button.

## Follow-ups (2026-11-18, v0.11.17)

- User: "I don't think we need to cache every turn for every session right? just the last turn for each?" and asked how eviction works. Code: a save already discards a conversation's earlier turns (`superseded`); eviction is least recently used, per model then global, only at save time. Production (GET) before: 76 states, 12.94 GiB of 16 GiB; 36 from `juiced-internal` (checks, Tune), 38 `execution_current: false`, 21 near-identical one-off states for one model.
- 3dfbe748: saved sessions expire on an hourly check (and once at startup) from the residency loop, not only when another session is saved.
- helper-fable-cache `15ecc8ba`, `74977e80` (merged): one-shot internal requests are not saved (`sessions::OneShot`, set by `post_once` in check, Tune and verify); stale states are deleted when their model's worker is ready (deferred during Tune) and leftover `juiced-internal` states at `start_hot`; a single-request state is replaced by a newer sibling sharing at least 2048 bytes and 9/10 of both requests (`Key::replaces_sibling`); pinned states are exempt and a later turn no longer deletes a pinned earlier turn. `session-lineage.json` records gain optional `single`.
- Gate on the merge: 946 service tests, clippy, fmt; rust-tests session_cache, engines, library_pipeline 31 passed (the helper ran the full set: 342 passed); 178 UI unit tests; Playwright sessions, cache, models, library 106 passed. v0.11.17 (run 44229956345 green) deployed to stack 83: 4 Hot models loaded; 71 models and 83 files unchanged; cache 76 -> 29 states, 12.94 -> 5.78 GiB (47 deleted, 7.16 GiB: every `juiced-internal` state, and the stale states of the two loaded chat models), matching the helper's prediction. Kept: Nemotron 21, Coder-Next Q4_K_M 4, Mellum 4, all for unloaded models.
- Open: a shared prefix inside one long first message is not covered (seen on Coder-Next Q4_K_M: input 73432, cached 41223, path cold); Tune deferral has no automated test; no opt-out for external one-shot requests; the 21 Nemotron states wait for retention or that model's next start.

## Follow-ups (2026-11-18, v0.11.18)

- User: "Yes, I want caching to work properly." helper-fable-cache2 `b6ff665c` (merged): scenario matrix measured on a real llama.cpp build (Qwen3-0.6B, CPU; rerun with `--test real_cache_matrix -- --ignored`). Fixed: text shared up to a point inside a message restores the saved state that holds it (`Key.text` block digests persisted in `session-lineage.json`; P3 1626 -> 17 tokens processed, D3 1622 -> 10); named sessions match each other (H3 1620 -> 7); two alternating long conversations restore instead of going cold (save reservation = 5/4 of the largest observed state, was 2x KV); `session_cache.path` follows the engine's cached tokens (`slot-hit` at 64 or more); Responses usage recorded; a failed or cancelled request keeps its slot; `X-Juiced-Cache: no-store`. Restore rule: an earlier turn always; an alike opening needs 1 KiB shared and re-reading at least twice the restore cost. Tune deferral now has a scripted test.
- Correction recorded: the 43K/73K production log lines were the primary agent's own depth benchmark (settings changed between cycles), not a client.
- Gate on the merge: 950 service tests, clippy, fmt; rust-tests session_cache, engines, library_pipeline, api_end_to_end 58 passed (helper: full set 348 passed, 37 ignored); 178 UI unit tests; Playwright sessions, cache, diagnostics 27 passed. v0.11.18 (run 44244445957 green) deployed to stack 83: 3 Hot models loaded (the user had changed Coder-Next UD-Q4_K_XL from Hot before the deploy); 71 models, 83 files and all policies unchanged; cache 29 states, 5.78 GiB before and after.
- Open, waiting on the user: per-model session budget (4 GiB cannot hold two 43K-token conversations at 2.25 GiB each; 5.2 GiB can; options A 8 GiB, B equal to total, C scaled to context, D two slots). Not fixed: a request that never completed saves nothing; a full budget of kept states only logs; each turn re-saves the whole state; hybrid models unmeasured.

## Next action

None for this audit. Open from earlier ledgers: the later-work list in drop-in-replacement.md.
