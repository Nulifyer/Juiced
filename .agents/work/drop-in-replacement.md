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

- [ ] Same compose (image tag change only, optional junk removal) serves on 8080 with the healthcheck passing.
- [ ] First boot adopts `/data/config` automatically: every native model with the same ID, policy, options, companions and weights in place; engines for non-Disabled models installed without operator steps; idempotent on restart.
- [ ] HA llama_cpp, HA Ollama, HA Wyoming (same identity), Pi and Tavern work unchanged.
- [ ] Rollback to v0.9.2 by image tag still works on the same volume.
- [ ] Released via `juiced-release.yml` as `v1.0.0-rc.N`, deployed with Portainer, inspected live.

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

## Next action

Engine-management round agreed with the user: version catalog as data with tested pins per release, update checks against release sources, nightly opt-in, remove, admin-added release sources, custom OpenAI-compatible engine in the UI. Then a real NPU check (FastFlowLM, hybrid) on mage.
