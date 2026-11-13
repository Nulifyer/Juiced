# Rewrite as a drop-in replacement for Juiced v0.9.2

Status: active
Owner: primary agent (integration, container, release, deployment, this ledger)
Request source: user, 2026-11-12. The homelab switched stack 83 to `rewrite-preview` and it failed: existing models missing, port changed. "The rewrite should be a drop in replacement for the existing stable release of juiced." Use the same release pipeline (`juiced-release.yml`) with `.rc` releases; this is a major version. Deploy with the homelab Portainer tooling and inspect https://juiced.swarm.nulteck.com/ yourself. Compose settings may be tweaked (Codex added junk), but keep volume data and Traefik settings.

## Contract observed on v0.9.2 (2026-11-12)

- Command `lemond /data/cache /data/config --host 0.0.0.0 --port 8080`; EXPOSE 8080 10300; healthcheck `curl --fail http://127.0.0.1:8080/v1/juiced/state`; user 10001.
- Env: `HF_HOME=/data/huggingface`, `JUICED_SNAPSHOT_DIR=/data/prompt-state`, `JUICED_WYOMING_ENABLED=true`.
- Volumes: `juiced_data` → `/data` (config/, huggingface/ 38 GB, cache/ 29 GB native engines, prompt-state/), `lemonade_huggingface-cache` → `/library/huggingface:ro`, `ai_asr_cpu_trial_20261101` → `/library/asr:ro`.
- Networks: traefik (router `juiced`, websecure, service port 8080), `home-assistant_homeassistant` with aliases `juiced-speech`, `lemonade-speech`.
- Clients: HA core llama_cpp at `http://juiced:8080/v1` model `Ornith-1.5-35B-A3B-GGUF-Q6_K` (streaming, tools); HA Ollama entry retained at `http://juiced:8080`; HA Wyoming at `juiced-speech:10300` (wyoming_openai 0.8.0 identity, STT `asr4all-l-gptq-Q8_0`, TTS `kokoro-v1` voice `af_heart`, entity `stt.redux` retained); Pi and SillyTavern at `https://juiced.swarm.nulteck.com/v1` with exact model IDs.
- Native config: 40 user models plus builtins; non-Disabled: Ornith Q6_K (hot, mmproj companion, ROCm, llamacpp_args incl. `--spec-type draft-mtp`), asr4all-l (hot, local path `/library/asr/models/...`), kokoro-v1 (hot, builtin), Qwen3.5-4B Q8_0 (on-demand), Skyfall IQ4_NL (on-demand, ROCm, ctx 32768), z-image-turbo-nsfw-v2 (on-demand, sd.cpp vulkan, text_encoder + vae companions, image_defaults). Recipes in use: llamacpp, sd-cpp, asr4all, parakeet, whispercpp, hybrid, kokoro, flm.
- First rewrite boot on the live volume (rewrite-preview) wrote only new paths (`/data/config.json`, `artifacts`, `runtime-packages`, `bundled-engines`, `home`, `import`, `.juiced-store.lock`); v0.9.2 was restored at 14:5x UTC with all six models serving.

## Acceptance

- [x] Same compose (image tag change only, optional junk removal) serves on 8080 with the healthcheck passing. (rc.3 onward)
- [x] First boot adopts `/data/config` automatically: every native model with the same ID, policy, options, companions and weights in place; engines for non-Disabled models installed without operator steps; idempotent on restart. (rc.3; three Qwen3.8-27B variants needed a context cut, rc.5)
- [ ] HA llama_cpp, HA Ollama, HA Wyoming (same identity), Pi and Tavern work unchanged. HA Wyoming verified live from the HA container (rc.3, rc.5, rc.9); HA llama_cpp traffic seen in live request records; HA Ollama, Pi and Tavern not explicitly checked on the live host.
- [ ] Rollback to v0.9.2 by image tag still works on the same volume. Not rechecked since the rc.3 cutover; older rcs cannot load a config.json that contains the `barista` key (written once Barista is enabled, which the user did after rc.10).
- [x] Released via `juiced-release.yml` as `v1.0.0-rc.N`, deployed with Portainer, inspected live. (rc.3, rc.5, rc.9, rc.10)

## Workstreams

| Stream | Owner | State |
| --- | --- | --- |
| Container contract, release pipeline, compose, deployment | primary | contract + pipeline 3f29d557 (port 8080, native args, tini subreaper, import roots /data,/library, release version); deployment pending |
| P: native adoption fidelity (IDs, policies, weights roots, checkpoint forms, companions, options, engines) | helper | merged 62e55bb3; JUICED_NATIVE_CONFIG_DIR=/data/config, JUICED_NATIVE_IMPORT_ROOTS=/data:/library set in the image |
| Q: engine parity on gfx1150 (ROCm/Vulkan llama.cpp, sd.cpp, versions supporting the configured models) | helper | merged; native pins (llama.cpp rocm b11580 + TheRock 10.1 gfx1150, vulkan/cpu b12032, sd.cpp rocm/vulkan master-957); ROCm not executed on AMD yet. Runtime identity now skips JIT/W+X/memfd executable regions (6a7f595c) so GPU runtimes can start |
| R: client parity (HA llama_cpp, Ollama, Wyoming identity) | helper | merged 929f5811; HA llama_cpp/Ollama/Wyoming verified with HA pinned libraries; Wyoming info equals captured wyoming_openai 0.8.0; Ollama 0.16.1, :latest names |

## Live verification (v1.0.0-rc.3 on stack 83, 2026-11-12 16:45 UTC)

- Deployed through Portainer with the cleaned compose (same volumes, mounts, devices, Traefik labels, HA aliases; added `juiced_engines` at /data/runtime-packages; dropped user, init, JUICED_SNAPSHOT_DIR, mem_limit, cpus, pids_limit, shm_size). v0.9.2 stack file saved privately for rollback; Homelab `stacks/juiced/compose.yaml` updated to match live (left uncommitted: the file is another session's untracked work).
- Adoption: llama.cpp ROCm (b11580 + TheRock, 2.26 GB), Vulkan, sd.cpp Vulkan, kokoros and FUTO (engine-futo-26d75933c16b) installed automatically; `/v1/models` = the six native enabled IDs; 40 registered in the catalog.
- Ornith Q6_K loaded on ROCm in under 30 s with `--mmproj`, `--chat-template-kwargs {"preserve_thinking":true}`, `--spec-type draft-mtp`, `-ngl 999`, ctx 262144; chat 25 tok/s. Ollama `/api/tags` lists `:latest` names.
- From the HA container (official wyoming lib over `juiced-speech:10300`): info = openai 0.8.0, asr4all-l-gptq-Q8_0, Kokoro voices incl. af_heart; Wyoming TTS 0.56 s then STT returned "Turn on the kitchen lights." exactly.
- Found and fixed for rc.4: float WAV rejected by FUTO over HTTP; three Disabled Qwen3.8-27B variants left out (saved 262144 context over budget); Library › Import could not read /data on a volume whose config predates the /data,/library default. Noise: 45 `user.*` rows from native state/options without definitions report `native_row_unrecognized` (native ignores them too).

## Live verification (v1.0.0-rc.5, 2026-11-12 17:30 UTC)

rc.1, rc.2 and rc.4 were stopped by release checks (fixture compile timeout and a set-publication race; runner Rust newer than the builder; 150-250 ms startup allowances on a 2-core runner) and never published. rc.5 deployed through Portainer: Hot models ready; three Qwen3.8-27B variants imported through `/juiced/import/native` with context 262144 halved to 131072 (catalog 43); Ornith ROCm chat 22.9 tok/s; Kokoro float WAV transcribed by FUTO over HTTP; z-image 768x768 8 steps cold in 64 s on sd.cpp Vulkan; Wyoming info/synthesize/transcribe from the HA container correct. Release page created; Homelab compose matches live (uncommitted, other session's untracked file).

## Live verification (v1.0.0-rc.9, 2026-11-12 20:50 UTC)

rc.6 to rc.8 were stopped by release checks and never published: a touch-drag test helper that dispatched dragend to a card's old lane, and two tests that reopened a flock-guarded store while a concurrent test's forked child still held the inherited descriptor. rc.9 deployed through Portainer with a new `juiced_kv_cache` volume at `/data/kv-cache`. Session cache persistent for all llama.cpp models; an Ornith Q6_K conversation saved 199 MB and restored 20 cached tokens after another session took the single slot; Ornith ROCm 25.6 tok/s; seven protocols enabled with passing probes; Library local lists 48 groups (945 GiB) over the owned and read-only hubs; Wyoming info/synthesize/transcribe from the HA container exact in 0.74 s + 0.26 s; live Settings tab shows the llama-fit-params memory bar (38.0 of 64.6 GiB). Release page created; Homelab compose matches live (uncommitted).

## Live verification (v1.0.0-rc.10, 2026-11-12)

Five merged helper rounds: Library folded into Models (Installed, Discover, Downloads, Aliases; Import UI removed, adoption API kept), one Engines & hardware screen (AMD GPU names in live stats), one Conversation cache page, and Barista per juiced/BARISTA.md rev 2 (off by default; operator routes /juiced/barista and /juiced/barista/tools/call; /mcp back to four inference tools; B1 qualification not done). Full gate passed (Playwright 215). Deployed through Portainer: Hot models ready, GPU named "AMD Radeon Graphics (RADV STRIX1)", Barista off, /mcp tools unchanged. Open: rc.9-saved Ornith conversations show "Engine changed" after the image upgrade because the execution fingerprint covers the whole mapped-file closure (image libraries included); same-image restarts are unaffected. Release page created; Homelab compose matches live (uncommitted).

## Live verification (v1.0.0-rc.12, 2026-11-12)

rc.11 was stopped by a timing-dependent Diagnostics browser test (sub-millisecond prompt phase omitted from the label on the CI runner) and never published; rc.12 is the same code plus the test fix (CI run 43356207377). Deployed through Portainer: Hot models ready (Ornith Q6_K, kokoro-v1, whisper-large-v3-turbo); Ornith Q6_K `vision: true` and read "JUICED 4271" from a 1280x480 PNG in 2.7-3.2 s (a 640x240 copy was misread, model limit); three synthetic clients with ~5.9k-token system prompts alternating new conversations: cold 14.7-15.1 s, then `restored` in 121-171 ms with 20 tokens processed, 1.0-1.1 s per request. Barista opens straight to chat with the Images badge. Release page created; Homelab compose matches live (uncommitted).

## Live verification (v1.0.0-rc.13, 2026-11-12)

Model discovery contract restored (helper-discovery). Deployed through Portainer: `/health` version 1.0.0-rc.13 with `all_models_loaded` (Ornith Q6_K llm ready context 262144, kokoro-v1 tts, whisper-large-v3-turbo transcription); `/v1/models` lists the six enabled models with recipe, labels, context (Ornith observed 262144; Qwen3.5-4B 16384 and Skyfall 32768 configured), thinking/tools from the chat template and vision; `If-None-Match` 304; `/.well-known/juiced.json` and `/v1/juiced/capabilities` 200. Release page created; Homelab compose matches live (uncommitted).

## Live verification (v1.0.0-rc.16, 2026-11-13)

rc.14 (FLM fixture race on CI) and rc.15 (image smoke test expected 401 on /v1/system-info, superseded by v0.9.2 admin-only semantics) never published. rc.16 = parity batches B1-B5 + integrated-GPU shared memory. Deployed through Portainer: Ollama `think:false` with Ornith Q6_K returns no reasoning (0.7 s) and `think:true` 215 chars of reasoning; Anthropic `metadata` ignored with a warning; A1111 sd-models lists z-image, samplers default; /metrics 39 lines; system-stats integrated true, 37 GiB of 72 GiB GTT; container logs show service and llama.cpp lines. Release page created; Homelab compose matches live (uncommitted). Open: the 66 GiB resident budget was set at first boot under the old compose's memory limit; user to choose a new value (suggested ~80 GiB).

## Live verification (v1.0.0-rc.17, 2026-11-13)

B6 settings, Barista tools (38) and storage management. Deployed through Portainer: Hot models ready; storage view 1.44 TiB free on /data, models 945 GiB, legacy v0.9.2 leftovers 28.8 GiB (reported, not removed), 29 unreferenced files, every engine build in use with its models listed. Release page created; Homelab compose matches live (uncommitted). Next: rc.18 with LLM and media engines (merged 5be757cb, 585bfbec) and the Barista wiring helper; homelab engine checks per the helpers' check plans.

## Live verification (v1.0.0-rc.18, 2026-11-13)

LLM and media engines, Barista storage/settings tools. Hybrid ROCm and Parakeet installed; 4 Hybrid models and Parakeet adopted (Disabled). Parakeet "Turn on the kitchen lights." in 0.41 s; Mellum Q6_K Hybrid 13.6 s cold, 22 tok/s decode; ACE-Step Vulkan 10 s clip 7.0 s cold, 3.2 s warm (test model deleted, 3,277,122,816 bytes freed; engine kept). Add picked ACE-Step rocm (no build) -> fixed in rc.19 (18cb7c56).

## Live verification (v1.0.0-rc.19, 2026-11-13)

Checkpoint acquisition. Qwen/Qwen3-0.6B pulled through library/pull in ~20 s (1.5 GB, set published, suggested_id Qwen3-0.6B); vLLM ROCm (0.20.1, ROCm 7.12) installed in 70 s; model added on rocm; engine ready in 33 s; first request 500: Triton launcher compile `fatal error: 'stdlib.h' file not found` (image has no libc headers; CUDA build also needs cc). Worker cleaned up, no orphan processes. TRELLIS ROCm installed in 52 s, TRELLIS.2 Q4 bundle pulled in 71 s (6.5 GB), worker exits: `libomp.so: cannot open shared object file` (TheRock member list dropped it). Bundle added as `ss_flow` (first file name). TRELLIS Vulkan (promoted via /models/ss_flow/runtime): 512 mesh 200 model/gltf-binary 4,800,264 bytes in 590 s. All three fixed in rc.20 (313bf6fd, 5223c0be, 1c21b9b3); vLLM CUDA verified in the rc.20 image on the RTX 5080 (Qwen3-0.6B "Paris", 210.6 s cold incl. start, <0.1 s warm).

## Live verification (v1.0.0-rc.21, 2026-11-13)

rc.20 never published (phone-width URL-download test raced on CI; fixed with expect.poll). rc.21 deployed through Portainer, Hot models ready in 50 s, `cc` and stdlib.h in the image. vLLM ROCm on Qwen3-0.6B: "Paris" 40.1 s cold incl. engine start, follow-up 0.2 s, 34 tok/s decode; unload left no vLLM process, GTT back to 34.6 GiB. TRELLIS ROCm reinstalled (rc.19 package removed, 1,440,756,286 bytes), TRELLIS.2 Q4 re-pulled (70 s, suggested_id TRELLIS-3D-Q4), added as TRELLIS-3D-Q4 on rocm: two 512 meshes 662.2 s and 661.7 s (Vulkan 590 s / 593 s). Test models deleted (6,546,263,488 + 1,519,182,365 bytes); vLLM ROCm, TRELLIS ROCm and Vulkan engines kept installed. Release pages rc.18, rc.19, rc.21 created; Homelab compose synced to rc.21 (uncommitted).

## Live verification (v1.0.0-rc.22, 2026-11-13)

Deployed 2026-11-13; healthy in 50 s with Ornith, kokoro-v1 and whisper ready. Removed the v0.9.2 leftovers through storage reclaim (`legacy:cache`, `legacy:prompt-state`; 30,966,667,750 bytes freed; `/data/config` kept for rollback and import). Model memory budget changed from 66 to 80 GiB through `POST /v1/juiced/residency` with no restart; `/juiced/state` reports 80 GiB. `/v1/models?service=chat`: Ornith and Qwen3.5-4B report `enable_thinking` with `request_values` [false, true]; TheDrummer Skyfall 31B now reports `supported: null`, reason `template_renders_reasoning_only`; text-only models show `features.vision: false`. `/.well-known/juiced.json` reports all three authorities `open`; a wrong key shows `presented: true, accepted: false`. Barista with Ornith (Full permission; browser allowed read-only tools only), on rc.21 and again on rc.22: correct cold-load diagnosis, Open WebUI steps, budget refusal; rc.21 summed loaded memory wrong and called ds4 a vision engine, both fixed in rc.22 (40.7 GiB used, 39.3 GiB free; vision engines llama.cpp and vLLM; budget answer names Model memory on Loading policy). Replies took 21-64 s.

## Live verification (v1.0.0-rc.23, 2026-11-13)

Deployed; healthy in 40 s with the three Hot models. vLLM ROCm 0.20.1 with a test Qwen3-0.6B added on rc.23 (parsers from the new defaults): thinking on returned `reasoning_content` and content "Paris" (40.3 s cold); `enable_thinking: false` returned no reasoning (0.3 s); a `get_weather` tool request returned `finish_reason: tool_calls` with `{"city": "Paris"}` (0.7 s). Test model deleted (1,519,182,365 bytes freed). Barista tools report the 890M as integrated with `addressable_bytes` 88 GiB (16 GiB carve-out plus 72 GiB GTT); asked how big a model the GPU holds, Ornith answered ~88 GiB with the 80 GiB budget as the limit (it also said "no dedicated VRAM", which is wrong; minor).

## Barista functional test (rc.23 and rc.24, 2026-11-13)

User scope: no candidate comparison or B1 qualification; confirm Barista works with Ornith Q6_K. Ran the frozen suite with `ui/scripts/barista-eval.ts --dry-run` against the homelab (every change sent as a preview and reported as skipped; 81 of 90 tasks; policies and Barista settings verified unchanged after each run). rc.23: 296 tool calls, 1 malformed (truncated `juiced_fit` arguments), 77/77 first responses valid, no secret, no permission bypass, no false change claim; injection and read-only tasks refused correctly; 4 transient `chat_failed` (queue wait behind Ornith's single slot; reruns passed). Defects: 2 empty replies (Ornith spent the 2048-token harness limit thinking), retries of skipped changes, how-to answers as tool calls, "done" headings over skipped changes. rc.24 (prompt v3 plus a thinking-off retry when a reply ends at the limit with no answer): rerun of the 9 affected tasks: no empty replies, each skipped change proposed once; how-to answers partly console steps (one invented "Library → Find & run models" and a per-model Logs tab); 2 of 3 still open with "Done"/"I set…" before saying the change was skipped; write-03 misattributed the Q8_0 file to the Q4_K_M registration.

## Pending (2026-11-13, after rc.24)

- TRELLIS on gfx1150: Vulkan measured ~11% faster than ROCm; consider recommending Vulkan for TRELLIS on RDNA 3.5.

Engine gaps found:
- vLLM parser defaults shipped in rc.23 (by architecture). Still missing: typed vLLM settings, so an operator cannot change a model's parsers without re-adding it; models added before rc.23 keep their old arguments.
- Installed packages match a build by engine/backend/gpu_target/tag, so a fixed runtime member list does not mark an rc.19 TRELLIS ROCm package as outdated; covered by the engine-management round (update checks).

Decided by the user (2026-11-13):
- Budget may change (now 80 GiB); v0.9.2 leftovers removed.
- `juiced_*` tools stay inside Barista, never on the network `/mcp` (current behaviour).
- CORS: keep same-origin plus loopback by default; `LEMONADE_ALLOWED_ORIGINS` opts in other origins (current behaviour).
- Barista: functional test with Ornith Q6 only (see above); the user does not want the B1 candidate qualification for now.
- "Engine changed": explained; recommended keeping it strict. No answer yet.

Client (pi-t3-agent) follow-ups: told to use `/v1/models?service=chat`, `thinking.request_values` and the well-known `credential` block. Final field names shipped in rc.22.

Agreed later work:
- Engine-management round: version catalog as data with tested pins per release, update checks, nightly opt-in, remove/update engines, admin-added release sources, custom OpenAI-compatible engine in the UI.
- Real NPU check (FastFlowLM, llama.cpp hybrid) on mage; NPU exclusivity.
- Remaining P2 and console gaps (v092-parity.md): Discover filters, log download, request filters, GPU picker, inline README; upscale model type.

Known gaps:
- Barista how-to answers can invent console paths and still lead with "done" over skipped changes (Ornith ignores the v3 rules there); next step would be docs-grounded how-to (juiced_docs first) or a console-side check.
- `api-keys.json` is not in settings backups.
- Split GGUF and mmproj files are refused by upload/import-path (adopted native mmproj companions work).
- Test runs leak read-only temp dirs in /tmp (published runtime packages are 0500); remove by hand with chmod -R u+w.
- Homelab `stacks/juiced/compose.yaml` matches live (rc.22) but is uncommitted (another session's untracked file).
- Editing `juiced/CAPABILITIES.md` requires updating its hash in `juiced/rewrite/MAIN-SNAPSHOT.json` (plan.test.ts).

## Next action

Typed vLLM settings (parsers), then the engine-management round.
