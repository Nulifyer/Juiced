# Juiced

Juiced hosts model weights, supervises backend processes and serves model APIs.
It is derived from Lemonade and uses compact T3-derived model-management controls.
Agents, saved conversations, documents and creative workspaces belong to downstream
clients. Interaction in this UI is temporary service testing.

## Current capabilities

- Inspect downloaded models, proper names, quants, components and effective settings.
- Search the curated catalogue or Hugging Face, review grouped quants, manage downloads
  and import GGUF files. New models start Disabled until explicitly enabled.
- Set Hot/on-demand/disabled policies, load/unload workers and apply settings with
  profile reload/recovery. Busy workers and protected Hot models retain safeguards.
- Select compatible runtime providers and hardware variants. CPU/Vulkan/ROCm and
  other qualified llama.cpp variants can reuse a GGUF; other engines have exact
  architecture/importer restrictions. Experimental XDNA2+ROCm is partial prefill,
  uses a matched bundle and does not promise a speed gain.
- Prepare supported local HF/GGUF variants without overwriting sources, with
  precision/capability-loss disclosure, cancellation and reference-aware storage.
- Serve documented OpenAI, Ollama, Anthropic and A1111 subsets plus optional private
  Wyoming speech. Native discovery describes declared operations and unknowns.
- Control qualified operational prompt snapshots, retention, per-model cache modes
  and reclamation. Memory/persistent reuse depends on the exact engine/profile.
- Inspect hardware/token metrics, raw/parsed logs and bounded request metadata.

The model-service MCP gateway serves individual operations; Omni agent loops and
caller-authored job graphs are excluded. Legacy records and weights are preserved.
There is no stored-conversation API, agent tool execution or creative gallery.

Read [runtime profiles and preparation](RUNTIME-PROFILES.md),
[capability discovery and identity](CAPABILITIES.md), and
[operator UI design](UI-DESIGN.md) for implemented behavior and limits.
The approved [platform plan](PLATFORM-PLAN.md) and its
[feature disposition](PLATFORM-FEATURES.json) describe future phases. Their approval
is not evidence those features or every hardware combination already work.

## Deployment

The default image is one Linux runtime container. Weights/state are persistent data,
not image layers. No Docker socket, external gateway/database or agent stack is
required. Give the container only the actual devices, permissions and host prerequisites
its selected engines require. Native Apple Metal is not available through a Linux
Docker image. Native cross-platform core builds and device qualification are different
support levels; broader packaged platform delivery remains planned.

Build, UI, preparation and engine dependencies use separate stages. The final runtime
excludes compilers, headers, Make/CMake, Node/Bun, Git and package installers. Optional
engines use native package installation and private dependencies. Host kernel drivers,
firmware and BIOS remain host-owned. An installed package is not inference proof.

Current standard ports are HTTP8080 and optional private Wyoming TCP10300. Preserve
existing volume identities and read-only original weight mounts during updates.
Use the deployment's exact release image/digest and retain previous image/state for
recovery. Do not run two writable managers against one state directory.

Qualified persistent snapshots bind the principal, credential epoch, consumer session,
protocol and model/engine profile. An identified session starts with a cold prefix
unless its validated snapshot is restored. Clients without session affinity keep
the memory fallback. Juiced stores computation snapshots, not conversation history.

## Development and checks

- Native: `cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_WEB_APP=OFF -DBUILD_TAURI_APP=OFF -DBUILD_TESTING=ON`, then `cmake --build build -j4 --target lemond test_request_identity`.
- Native API: `python3 juiced/tests/test_api.py build/lemond` uses isolated model-free fixtures.
- Identity: `ctest --test-dir build --output-on-failure -R '^RequestIdentityTest$'`.
- UI: `cd juiced/ui && bun install --frozen-lockfile && bun run build`.
- UI workflows: `bun run test:e2e` in `juiced/ui`; `JUICED_TEST_CHROMIUM` can select an installed test browser.
- Actual CPU snapshot gate: `python3 juiced/tests/test_engine_cache.py --help`; a retained tiny fixture checks useful save/reload/restore, not general model quality.
- Cache admission/isolation: `python3 juiced/tests/test_cache_admission.py --help`; isolated real-engine checks cover alternate model names, failure cleanup, same-worker principal changes and restart/credential boundaries.
- Image: `docker build -f juiced/Dockerfile -t juiced:development .`.

The UI builds root `web/`, copied into native resources by the image. Its development
proxy defaults to a local manager on port13305; set `JUICED_DEV_API` to a test runtime
for isolated UI work. Release CI checks UI, native contracts, runtime packaging and
actual-container behavior. Compilation or fixtures do not qualify unseen hardware.
