# Juiced rewrite container

This image runs the Rust `lemond` service and the web console as one appliance. On first boot it writes a default configuration into the data volume. It then reports the host's CPU, GPUs and NPUs at `GET /v1/system-info` and offers the engine builds that fit at `GET /v1/juiced/engines`. Most engines are not in the image. The service downloads them on demand, checks the pinned SHA-256 digest and unpacks them into the data volume.

The image does not include a model. Models you add start Disabled until you enable them.

## Quick start

Build from the repository root. The script builds locally and never pushes:

```bash
juiced/rewrite/container/build.sh
```

Run it:

```bash
podman run -d --name juiced -p 8080:8080 -v juiced-data:/data localhost/juiced-rewrite:dev
```

Open <http://localhost:8080>. Without API keys the service runs in trusted-LAN mode, and anyone who can reach the port has operator rights. Before you publish the port beyond your own machine, create keys under **Connect → API keys** or set them in the [environment](#environment).

`build.sh` accepts extra `podman build` arguments. `JUICED_IMAGE` changes the tag and `CONTAINER_ENGINE=docker` builds with Docker. Under Podman the script passes `--format docker`, because the OCI format drops the `HEALTHCHECK`.

The build has three stages. A Rust stage runs `cargo build --release --locked` for `lemond` and the `lemonade` CLI. A Bun stage builds `ui/dist`. The runtime stage is `debian:trixie-slim` with the Vulkan loader, Mesa from `trixie-backports`, libgomp, libnuma, ca-certificates, curl and tini. The build context is the repository root, because the service embeds `src/cpp/resources/server_models.json` and the `docs/api` pages, and the console uses fonts and license files from `juiced/ui`. `Containerfile.dockerignore` keeps the context to those paths.

## Engines

The image ships no engines. Install them from Engines & hardware (or `POST /v1/juiced/engines/install`); each one downloads a pinned release, checks its SHA-256 and lands in `/data/runtime-packages`. FUTO publishes no transcribe.cpp builds, so Juiced publishes its own: pushing an `engine-futo-*` tag runs `.github/workflows/juiced-engine-release.yml`, which builds `juiced/rewrite/engines/futo/Containerfile` (the pinned source archive, commit `26d75933`, and `juiced/runtime/futo_server.cpp`, CPU only) and attaches the tarball to a GitHub release that the engine catalog pins like any other.

Mount a separate volume at `/data/runtime-packages` to keep engines apart from settings and models. An empty volume is claimed on first start; one with unrelated content is refused.

## Conversation cache

llama.cpp models save each conversation's processed prompt to `/data/kv-cache` after every reply. When a client sends that conversation again, the service restores it first and the engine processes only the new message. Restores also work after the model unloads (idle, memory pressure or Unload), after an options reload and after a container restart. This is on by default, with an 8 GiB budget (4 GiB per model). Change the mode and budgets on the Cache page or under `session_cache` in `config.json`.

Give the cache its own volume so it can grow, shrink or be wiped independently of settings and models:

```yaml
services:
  juiced:
    volumes:
      - juiced-data:/data
      - juiced-kv-cache:/data/kv-cache
volumes:
  juiced-data:
  juiced-kv-cache:
```

The image creates `/data/kv-cache` owned by uid 10001 with mode 0700, so a new named volume starts with the right owner. A host directory must be empty and owned by uid 10001 (`chown 10001:10001`, or `:U` under Podman). An empty directory is claimed on first start. A directory that already holds unrelated files is refused, and the service does not start.

Native Juiced kept its engine binaries in `/data/cache`, so the cache uses a different path. A rewrite release before rc.6 saved snapshots in `/data/cache` too. If that directory holds only snapshot files, the service moves it to `/data/kv-cache` on first start. If it also holds native files, the old snapshots stay in place, are no longer used, and a warning in the service log names them. Delete the `kv-*.bin`, `snapshots.json` and `.recovery-*` files there to reclaim the space.

## Data volume

Everything that persists lives in `/data`:

| Path | Contents |
| --- | --- |
| `config.json` | Service configuration. The entrypoint creates it only when it is missing. |
| `api-keys.json` | Hashes of keys issued in the console. Created with the first key. |
| `artifacts/` | Downloaded model weights |
| `runtime-packages/` | Installed engines (llama.cpp, whisper.cpp, stable-diffusion.cpp, Kokoros, FUTO and others). Can be its own volume. |
| `kv-cache/` | Saved conversation state and explicit slot snapshots. Can be its own volume (see [Conversation cache](#conversation-cache)). |
| `home/` | `$HOME` and `$XDG_CACHE_HOME` for engines, such as Mesa shader caches |
| `import/` | Server-side model files to import. Bind-mount a host directory here, for example `-v /srv/models:/data/import:ro`. |

The service runs as uid 10001 and needs to own `/data`. A named volume gets the right owner automatically. For a host directory, use `-v /srv/juiced:/data:U` under Podman, or start the container once with `--user 0` so the entrypoint can `chown` the top directory.

To add a model file that is not on Hugging Face, upload it from the console or with `curl -X POST -T model.gguf -H 'Content-Type: application/octet-stream' "http://HOST:8080/v1/juiced/library/upload?filename=model.gguf&size=$(stat -c %s model.gguf)"`. A file already on the host can be mounted under `/data/import` and imported with `POST /v1/juiced/library/import-path`, either copied into `artifacts/` or linked in place. A linked file must stay mounted at the same path. The first-boot configuration lists `/data` and `/library` in `import_roots`, so `/data/import`, an old Juiced data volume and read-only model mounts under `/library` are all reachable.

The default configuration sets `inhibit_suspend` to false, because the container cannot reach the host's logind. It sets `resident_budget_bytes` to three quarters of the smaller of host memory and the container's cgroup memory limit. `JUICED_RESIDENT_BUDGET_BYTES` overrides that value, but only on first boot. Later, edit the value in the console or in `config.json`.

## Ports

| Port | Use |
| --- | --- |
| 8080/tcp | HTTP API and console. `LEMONADE_PORT` changes it inside the container. |
| 10300/tcp | Wyoming speech server for Home Assistant. Disabled by default. |

The image does not expose UDP discovery. Clients connect to an explicit URL.

## Home Assistant voice

Home Assistant reaches Juiced in two ways. Its Ollama integration talks to the HTTP port for conversation. Its Wyoming Protocol integration talks to port 10300 for speech-to-text through whisper.cpp and text-to-speech through Kokoros.

`JUICED_WYOMING_ENABLED=true` turns the listener on at `JUICED_WYOMING_URI` (default `tcp://0.0.0.0:10300`) until Wyoming settings are saved, matching the native container.

The Wyoming protocol has no authentication. Anyone who can reach the port can transcribe and synthesize with your models, even when API keys are set. The listener is therefore off by default. Publish 10300 only to a trusted network, for example `-p 192.168.1.10:10300:10300` instead of `-p 10300:10300`.

To set it up:

1. Add and enable a transcription model and a speech model in the console. The policy must be on-demand or hot.
2. Publish the port: add `-p 10300:10300` to `podman run`.
3. Enable the listener. The `expected_generation` value is the `configuration_generation` that `GET /v1/juiced/wyoming` returns:

   ```bash
   curl -s -H "Authorization: Bearer $LEMONADE_ADMIN_API_KEY" http://localhost:8080/v1/juiced/wyoming
   curl -s -H "Authorization: Bearer $LEMONADE_ADMIN_API_KEY" -H 'Content-Type: application/json' \
     -d '{"settings":{"enabled":true,"listen":"0.0.0.0:10300","stt_model":null,"tts_model":null,"tts_voice":null},"expected_generation":GENERATION}' \
     http://localhost:8080/v1/juiced/wyoming
   ```

   With `stt_model` or `tts_model` set to null, Juiced uses every enabled transcription model and the first enabled speech model. `tts_voice` picks the voice used when Home Assistant does not ask for one. The setting is saved in `config.json` and survives restarts.
4. In Home Assistant, open **Settings → Devices & services → Add integration → Wyoming Protocol**. Enter the container host's address as **Host** and `10300` as **Port**.
5. Open **Settings → Voice assistants**, edit your assistant, and pick the Juiced entries for **Speech-to-text** and **Text-to-speech**.

`GET /v1/juiced/wyoming` reports whether the listener is up, its bound address, a bind error if any, the connection counts and a summary of the last request (no transcript text). A request that arrives while a model is unloaded waits for the model to load, so the first command after a cold start can take several seconds.

## Environment

| Variable | Default | Effect |
| --- | --- | --- |
| `LEMONADE_API_KEY` | unset | Requires this key for API routes |
| `LEMONADE_ADMIN_API_KEY` | unset | Requires this key for operator routes. With only `LEMONADE_API_KEY` set, that key is the operator key. |
| `HF_TOKEN` | unset | Sent only to Hugging Face metadata requests. Gated downloads still fail. |
| `JUICED_HUGGINGFACE_URL` | `https://huggingface.co` | Hugging Face base URL for library search |
| `LEMONADE_PORT` | `8080` | Listen port. The healthcheck and the bundled `lemonade` CLI read it too. |
| `JUICED_LISTEN_HOST` | `0.0.0.0` | Listen address |
| `JUICED_RESIDENT_BUDGET_BYTES` | 75% of memory | First-boot resident budget |
| `JUICED_UPDATE_USERSPACE` | `0` | `1` runs `juiced-update-userspace` at start. Requires `--user 0`. |
| `JUICED_IO_THREADS` | runtime default | Tokio worker threads |
| `JUICED_NATIVE_CONFIG_DIR` | `/data/config` | Native Juiced configuration directory adopted once in the background; nothing happens when it does not exist |
| `JUICED_NATIVE_IMPORT_ROOTS` | `/data:/library` | Colon-separated read roots for adopted weights |

Keys issued under **Connect → API keys** are stored as hashes in `/data/api-keys.json`; the secret is shown once. Environment keys exist only in the environment. The entrypoint never writes them to `/data`, and the service does not pass `LEMONADE_*_API_KEY` to engine processes. Engines do inherit `HF_TOKEN`. `podman inspect` shows plain `-e` values, so a Podman secret is the better way to supply a key:

```bash
printf '%s' "$(openssl rand -hex 24)" | podman secret create juiced-admin-key -
podman run -d --name juiced -p 8080:8080 -v juiced-data:/data \
  --secret juiced-admin-key,type=env,target=LEMONADE_ADMIN_API_KEY \
  localhost/juiced-rewrite:dev
```

The CLI inside the container uses the same key: `podman exec juiced lemonade status`.

## Hardware passthrough

Without passthrough the container still reads `/sys`, so `system-info` lists the GPUs. It marks their render nodes `missing` and recommends CPU builds. Each recipe below makes the matching backend `available`.

### NVIDIA (CUDA and Vulkan)

The host needs the NVIDIA driver and the NVIDIA Container Toolkit. Generate the CDI spec, and generate it again after every driver upgrade:

```bash
sudo nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml
nvidia-ctk cdi list
```

Then run:

```bash
podman run -d --name juiced -p 8080:8080 -v juiced-data:/data \
  --device nvidia.com/gpu=all localhost/juiced-rewrite:dev
```

With Docker, use `--gpus all`, or `--device nvidia.com/gpu=all` when CDI is enabled. The toolkit mounts `nvidia-smi`, `libcuda` and the NVIDIA Vulkan ICD from the host, so driver userspace always matches the host kernel module. The CUDA builds of llama.cpp and stable-diffusion.cpp bring their own CUDA runtime, about 600 MB per build. The host does not need the CUDA toolkit.

### AMD ROCm

ROCm needs `/dev/kfd` and the DRI nodes:

```bash
podman run -d --name juiced -p 8080:8080 -v juiced-data:/data \
  --device /dev/kfd --device /dev/dri --group-add keep-groups \
  localhost/juiced-rewrite:dev
```

`--group-add keep-groups` hands your own `video` and `render` memberships to the container under rootless Podman. Rootful Podman and Docker need the host's numeric group IDs instead: `--group-add "$(getent group render | cut -d: -f3)" --group-add "$(getent group video | cut -d: -f3)"`. Another option is `--user 0`, which makes the entrypoint read the group IDs from the device nodes before it drops to uid 10001.

`system-info` reads the GPU's gfx target from KFD topology and picks the matching build. The host needs only the in-kernel `amdgpu` driver; the image adds no ROCm packages.

- gfx1150 (Radeon 890M) gets the same ROCm builds as native Juiced v0.9.2: llama.cpp `b11580` and sd.cpp `master-957-90e86d1` from the `lemonade-sdk` TheRock 10.1 releases. Those archives link against ROCm without shipping it. The installer also downloads AMD's TheRock 10.1.0 gfx1150 runtime tarball (`therock-dist-linux-gfx1150-10.1.0.tar.gz`, 1.97 GB, from `stable.repo.amd.com`) and checks its pinned SHA-256. It keeps about 670 MB of it inside the package under `rocm/`: HIP, hipBLAS, hipBLASLt, rocBLAS, rocSOLVER, comgr with LLVM, TheRock's own `libdrm`/`libnuma`/`libelf` copies, and the gfx1150 kernel archives. Each package carries its own copy, so llama.cpp and sd.cpp ROCm use about 2.1 and 1.8 GB on disk.
- Other targets use the `lemonade-sdk/llamacpp-rocm` `b1338` builds, which bundle their ROCm runtime (gfx103X, gfx110X, gfx1151, gfx120X, gfx908, gfx90a). sd.cpp ROCm is catalogued for gfx1150 only.
- whisper.cpp ROCm builds from `lemonade-sdk/whisper.cpp-rocm` bundle their own ROCm 7.12 runtime (gfx110X, gfx1150, gfx1151, gfx120X).

On AMD GPUs, llama.cpp recommends ROCm. sd.cpp and whisper.cpp recommend Vulkan, matching native's backend order. A backend an adopted model names, such as `llamacpp_backend: rocm` or `sd-cpp_backend: vulkan`, still wins. When the recommended backend has no build for the detected target, for example gfx1152 or gfx942, automatic registration uses the best remaining backend that has one, usually Vulkan, and its `reason` names the skipped backend.

I have not tested this recipe on AMD hardware. If an engine fails with memory-mapping errors, try `--security-opt seccomp=unconfined`.

### Intel and AMD Vulkan

```bash
podman run -d --name juiced -p 8080:8080 -v juiced-data:/data \
  --device /dev/dri --group-add keep-groups localhost/juiced-rewrite:dev
```

The image ships Mesa's RADV (AMD) and ANV (Intel) drivers. It removes lavapipe, Mesa's CPU Vulkan driver, so a host without a GPU never reports Vulkan as available. Intel GPUs have no SYCL or Level Zero engine in the catalog. Vulkan is the only GPU path for Intel.

### AMD NPUs (Ryzen AI, XDNA 2)

Two engines use the XDNA 2 NPU in Ryzen AI 300-series and Strix Halo processors:

- **FastFlowLM** (`fastflowlm`, backend `npu`) runs chat models on the NPU alone. Release `ROCm/FastFlowLM` v1.0.12 is pinned by digest.
- **llama.cpp hybrid** (`llama.cpp`, backend `hybrid-rocm`) runs llama.cpp on a gfx1150 GPU (Radeon 880M/890M) and moves eligible prefill matrices to the NPU. Release `Nulifyer/llama-xdna-hybrid` rocm-v0.3.2 is pinned by digest. It is experimental, so Juiced never selects it automatically. Name it with `"backend":"hybrid-rocm"` in `POST /juiced/models/add`.

The host needs the in-kernel `amdxdna` driver and its NPU firmware. Both engines bring their own XRT userspace. The hybrid engine also brings its own ROCm runtime, so the archive is about 1.1 GB and unpacks to 3.9 GB. Pass the NPU node, raise the locked-memory limit, and add the GPU nodes for the hybrid engine:

```bash
podman run -d --name juiced -p 8080:8080 -v juiced-data:/data \
  --device /dev/accel/accel0 --device /dev/kfd --device /dev/dri \
  --ulimit memlock=-1:-1 --group-add keep-groups \
  localhost/juiced-rewrite:dev
```

The XDNA driver pins buffers in memory, and FastFlowLM refuses to start below its locked-memory minimum. A container cannot raise the limit above the hard limit of the user that starts it. Rootless Podman therefore keeps the host user's limit, often 8192 KiB, even with `--ulimit memlock=-1:-1`. Check it with `ulimit -Hl` on the host. Raise it with `LimitMEMLOCK=infinity` in the user's systemd manager or a `memlock` line in `/etc/security/limits.d/`, or run the container with rootful Podman or Docker.

At startup, the entrypoint logs a warning when a passed-through device is not readable and writable, when `/dev/kfd` arrives without `/dev/dri`, or when the locked-memory limit is not unlimited. `GET /system-info` reports the same conditions:

- `devices.npus[]`: the `amdxdna` device, its `family` (`XDNA2`), `access`, and `firmware_version` and `driver_version` when sysfs exposes them.
- `devices.gpus[].gfx_target`: the GPU target read from KFD topology.
- `devices.limits.memlock`: `unlimited`, `limited` (with `memlock_bytes`) or `unknown`.
- `backends[]`: `flm`/`npu` and `llamacpp`/`hybrid-rocm` with their state and reasons, for example `memlock_limited`, `npu_inaccessible`, `no_amd_npu`, or `amd_gpu_target_unsupported` for a GPU that is not gfx1150.

FastFlowLM keeps its own model store at `/data/flm`. Install the engine, then pull and register a checkpoint:

```bash
curl -X POST http://localhost:8080/v1/juiced/engines/install \
  -H 'content-type: application/json' \
  -d '{"engine":"fastflowlm","backend":"npu","expected_generation":0}'
curl http://localhost:8080/v1/juiced/flm          # NPU validation and the checkpoint list
curl -X POST http://localhost:8080/v1/juiced/flm/pull \
  -H 'content-type: application/json' -d '{"checkpoint":"gemma3:4b"}'
curl -X POST http://localhost:8080/v1/juiced/flm/models/add \
  -H 'content-type: application/json' -d '{"checkpoint":"gemma3:4b","policy":"on-demand"}'
```

Only chat checkpoints can be registered. FLM embedding and transcription checkpoints are listed but refused.

Detection, installation, launch arguments and environment are covered by tests with fixture hardware only. Neither engine has run on NPU hardware under this image. Ryzen AI's Windows-only engine (`ryzenai-llm`) is reported as unavailable on Linux.

### Intel NPUs

```bash
podman run -d --name juiced -p 8080:8080 -v juiced-data:/data \
  --device /dev/accel/accel0 --group-add keep-groups localhost/juiced-rewrite:dev
```

`system-info` then reports the `intel_vpu` device and its access. No engine in the rewrite catalog runs on an Intel NPU, so passthrough serves detection only.

## Updating engines

The engine catalog, with its pinned releases and digests, is compiled into `lemond`. Install engines from the console or the API:

```bash
curl -X POST http://localhost:8080/v1/juiced/engines/install \
  -H 'content-type: application/json' \
  -d '{"engine":"llama.cpp","backend":"cuda","expected_generation":0}'
```

Read `expected_generation` from `GET /v1/juiced/engines` first. Installed engines stay in `/data/runtime-packages` across container restarts and image upgrades. Newer engine releases arrive with a newer image, and earlier packages stay installed for rollback.

## Updating GPU userspace

The container runs for a long time, while Mesa and the Vulkan loader keep getting fixes. Update them in place without stopping the service:

```bash
podman exec -u 0 juiced juiced-update-userspace
```

The script runs `apt-get upgrade`, installs the latest `trixie-backports` Mesa and prints the installed versions. `lemond` keeps running. Engines are separate processes, so they pick up the new libraries at their next start. Unload and load a model to apply the update.

These changes live in the container's writable layer. Recreating the container from the image discards them. To apply updates on every start, run with `--user 0 -e JUICED_UPDATE_USERSPACE=1`. Boot then needs network access, and a failed update logs a warning and starts with the packages already installed.

NVIDIA userspace comes from the host. Upgrade the host driver, regenerate the CDI spec and restart the container. Kernel drivers (`amdgpu`, `i915`, `xe`, `amdxdna`) always come from the host.

## Upgrading the image

```bash
podman exec juiced cat /data/config.json > config.json.bak
juiced/rewrite/container/build.sh
podman rm -f juiced
podman run -d --name juiced -p 8080:8080 -v juiced-data:/data localhost/juiced-rewrite:dev
```

The new service migrates configuration in memory and saves the new schema on the next edit. An older image may fail to read a configuration that a newer one saved, so keep the backup until you no longer need to roll back.

## Smoke test

```bash
juiced/rewrite/container/smoke-test.sh
SKIP_BUILD=1 EXPECT_GPU=nvidia juiced/rewrite/container/smoke-test.sh --device nvidia.com/gpu=all
```

The script builds the image, starts it with a throwaway volume and a random admin key, then checks:

- `/v1/health` answers, and the container healthcheck passes
- an unauthenticated `/v1/system-info` gets 401
- `config.json` exists, has no models and does not contain the key
- `/v1/system-info` and `/v1/juiced/engines` answer and their summaries are printed
- `/` serves the console and its script
- a restart leaves `config.json` unchanged

Arguments after the script name go to `podman run`. With `EXPECT_GPU`, the test fails unless a vendor tool confirms a GPU from that vendor. The script removes the container and volume when it exits.

## Init and user

`tini` is PID 1. It forwards `podman stop` to `lemond` and reaps orphaned processes. The image's default user is 10001. Starting with `--user 0` runs the optional userspace update, fixes `/data` ownership and adds device groups. The entrypoint then uses `setpriv` to drop to uid 10001 with no capabilities before it starts `lemond`. Any command that does not start with `-` runs instead of the service, for example `podman run --rm localhost/juiced-rewrite:dev lemonade --help`.

## Published images

Pushing a `v*` tag runs `.github/workflows/juiced-release.yml` at that commit. It runs the service, process and console checks, builds this Containerfile with the tag as the service version, smoke-tests the exact image with Docker and pushes `ghcr.io/nulifyer/juiced:<tag>`. Only tags without a hyphen move `latest`, so release candidates such as `v1.0.0-rc.1` never replace the stable image.
