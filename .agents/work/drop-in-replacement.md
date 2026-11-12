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
| Container contract, release pipeline, compose, deployment | primary | in progress |
| P: native adoption fidelity (IDs, policies, weights roots, checkpoint forms, companions, options, engines) | helper | starting |
| Q: engine parity on gfx1150 (ROCm/Vulkan llama.cpp, sd.cpp, versions supporting the configured models) | helper | starting |
| R: client parity (HA llama_cpp, Ollama, Wyoming identity) | helper | starting |

## Next action

Primary: container contract + release workflow while helpers run.
