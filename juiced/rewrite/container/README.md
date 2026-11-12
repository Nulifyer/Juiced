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
podman run -d --name juiced -p 13305:13305 -v juiced-data:/data localhost/juiced-rewrite:dev
```

Open <http://localhost:13305>. Without API keys the service runs in trusted-LAN mode, and anyone who can reach the port has operator rights. Set keys before you publish the port beyond your own machine (see [Environment](#environment)).

`build.sh` accepts extra `podman build` arguments. `JUICED_IMAGE` changes the tag and `CONTAINER_ENGINE=docker` builds with Docker. Under Podman the script passes `--format docker`, because the OCI format drops the `HEALTHCHECK`.

The build has four stages. A Rust stage runs `cargo build --release --locked` for `lemond` and the `lemonade` CLI. A Bun stage builds `ui/dist`. A `futo` stage builds FUTO transcribe.cpp and its HTTP adapter (see [Bundled engines](#bundled-engines)). The runtime stage is `debian:trixie-slim` with the Vulkan loader, Mesa from `trixie-backports`, libgomp, libnuma, ca-certificates, curl and tini. The build context is the repository root, because the service embeds `src/cpp/resources/server_models.json` and the console uses fonts and license files from `juiced/ui`. `Containerfile.dockerignore` keeps the context to those paths.

## Bundled engines

FUTO publishes no transcribe.cpp builds, so the image builds one: the pinned source archive (commit `26d75933`, SHA-256 checked) and `juiced/runtime/futo_server.cpp`, CPU only. It adds about 5 MB under `/opt/juiced/bundled/futo`, beside a build-time import specification `futo.import.json`; the image is about 480 MB. On each start the entrypoint runs `lemond --import-runtime-bundle` for every `/opt/juiced/bundled/*.import.json` whose digest differs from the marker in `/data/bundled-engines/`, so the engine installs into `runtime-packages/` without network on first boot and again only when a newer image changes it. `GET /v1/juiced/engines` then lists `futo-transcribe.cpp` with an `installed_package` (its `unresolved` stays `not_published`), and `POST /v1/juiced/models/add` picks it for an asr4all GGUF such as `asr4all-s-gptq-Q8_0.gguf`. A failed import logs a warning and the service starts anyway.

## Data volume

Everything that persists lives in `/data`:

| Path | Contents |
| --- | --- |
| `config.json` | Service configuration. The entrypoint creates it only when it is missing. |
| `artifacts/` | Downloaded model weights |
| `runtime-packages/` | Installed engines (llama.cpp, whisper.cpp, stable-diffusion.cpp, Kokoros, the bundled FUTO build) |
| `bundled-engines/` | Digests of the bundled engine specifications already imported |
| `cache/` | Slot snapshots and computation cache |
| `home/` | `$HOME` and `$XDG_CACHE_HOME` for engines, such as Mesa shader caches |
| `import/` | Server-side model files to import. Bind-mount a host directory here, for example `-v /srv/models:/data/import:ro`. |

The service runs as uid 10001 and needs to own `/data`. A named volume gets the right owner automatically. For a host directory, use `-v /srv/juiced:/data:U` under Podman, or start the container once with `--user 0` so the entrypoint can `chown` the top directory.

To add a model file that is not on Hugging Face, upload it from the console or with `curl -X POST -T model.gguf -H 'Content-Type: application/octet-stream' "http://HOST:13305/v1/juiced/library/upload?filename=model.gguf&size=$(stat -c %s model.gguf)"`. A file already on the host can be mounted under `/data/import` and imported with `POST /v1/juiced/library/import-path`, either copied into `artifacts/` or linked in place. A linked file must stay mounted at the same path. The first-boot configuration lists `/data/import` in `import_roots`; a configuration created by an older image needs `"import_roots": ["/data/import"]` added by hand.

The default configuration sets `inhibit_suspend` to false, because the container cannot reach the host's logind. It sets `resident_budget_bytes` to three quarters of the smaller of host memory and the container's cgroup memory limit. `JUICED_RESIDENT_BUDGET_BYTES` overrides that value, but only on first boot. Later, edit the value in the console or in `config.json`.

## Ports

| Port | Use |
| --- | --- |
| 13305/tcp | HTTP API and console. `LEMONADE_PORT` changes it inside the container. |
| 10300/tcp | Wyoming speech server for Home Assistant. Disabled by default. |

The image does not expose UDP discovery. Clients connect to an explicit URL.

## Home Assistant voice

Home Assistant reaches Juiced in two ways. Its Ollama integration talks to the HTTP port for conversation. Its Wyoming Protocol integration talks to port 10300 for speech-to-text through whisper.cpp and text-to-speech through Kokoros.

The Wyoming protocol has no authentication. Anyone who can reach the port can transcribe and synthesize with your models, even when API keys are set. The listener is therefore off by default. Publish 10300 only to a trusted network, for example `-p 192.168.1.10:10300:10300` instead of `-p 10300:10300`.

To set it up:

1. Add and enable a transcription model and a speech model in the console. The policy must be on-demand or hot.
2. Publish the port: add `-p 10300:10300` to `podman run`.
3. Enable the listener. The `expected_generation` value is the `configuration_generation` that `GET /v1/juiced/wyoming` returns:

   ```bash
   curl -s -H "Authorization: Bearer $LEMONADE_ADMIN_API_KEY" http://localhost:13305/v1/juiced/wyoming
   curl -s -H "Authorization: Bearer $LEMONADE_ADMIN_API_KEY" -H 'Content-Type: application/json' \
     -d '{"settings":{"enabled":true,"listen":"0.0.0.0:10300","stt_model":null,"tts_model":null,"tts_voice":null},"expected_generation":GENERATION}' \
     http://localhost:13305/v1/juiced/wyoming
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
| `LEMONADE_PORT` | `13305` | Listen port. The healthcheck and the bundled `lemonade` CLI read it too. |
| `JUICED_LISTEN_HOST` | `0.0.0.0` | Listen address |
| `JUICED_RESIDENT_BUDGET_BYTES` | 75% of memory | First-boot resident budget |
| `JUICED_UPDATE_USERSPACE` | `0` | `1` runs `juiced-update-userspace` at start. Requires `--user 0`. |
| `JUICED_IO_THREADS` | runtime default | Tokio worker threads |

API keys exist only in the environment. The entrypoint never writes them to `/data`, and the service does not pass `LEMONADE_*_API_KEY` to engine processes. Engines do inherit `HF_TOKEN`. `podman inspect` shows plain `-e` values, so a Podman secret is the better way to supply a key:

```bash
printf '%s' "$(openssl rand -hex 24)" | podman secret create juiced-admin-key -
podman run -d --name juiced -p 13305:13305 -v juiced-data:/data \
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
podman run -d --name juiced -p 13305:13305 -v juiced-data:/data \
  --device nvidia.com/gpu=all localhost/juiced-rewrite:dev
```

With Docker, use `--gpus all`, or `--device nvidia.com/gpu=all` when CDI is enabled. The toolkit mounts `nvidia-smi`, `libcuda` and the NVIDIA Vulkan ICD from the host, so driver userspace always matches the host kernel module. The CUDA builds of llama.cpp and stable-diffusion.cpp bring their own CUDA runtime, about 600 MB per build. The host does not need the CUDA toolkit.

### AMD ROCm

ROCm needs `/dev/kfd` and the DRI nodes:

```bash
podman run -d --name juiced -p 13305:13305 -v juiced-data:/data \
  --device /dev/kfd --device /dev/dri --group-add keep-groups \
  localhost/juiced-rewrite:dev
```

`--group-add keep-groups` hands your own `video` and `render` memberships to the container under rootless Podman. Rootful Podman and Docker need the host's numeric group IDs instead: `--group-add "$(getent group render | cut -d: -f3)" --group-add "$(getent group video | cut -d: -f3)"`. Another option is `--user 0`, which makes the entrypoint read the group IDs from the device nodes before it drops to uid 10001.

The ROCm builds from `lemonade-sdk/llamacpp-rocm` and `lemonade-sdk/whisper.cpp-rocm` bundle the ROCm runtime, 160 to 820 MB depending on the GPU target. The host needs only the in-kernel `amdgpu` driver. `system-info` reads the GPU's gfx target from KFD topology and picks the matching build.

I have not tested this recipe on AMD hardware. If an engine fails with memory-mapping errors, try `--security-opt seccomp=unconfined`.

### Intel and AMD Vulkan

```bash
podman run -d --name juiced -p 13305:13305 -v juiced-data:/data \
  --device /dev/dri --group-add keep-groups localhost/juiced-rewrite:dev
```

The image ships Mesa's RADV (AMD) and ANV (Intel) drivers. It removes lavapipe, Mesa's CPU Vulkan driver, so a host without a GPU never reports Vulkan as available. Intel GPUs have no SYCL or Level Zero engine in the catalog. Vulkan is the only GPU path for Intel.

### AMD and Intel NPUs

```bash
podman run -d --name juiced -p 13305:13305 -v juiced-data:/data \
  --device /dev/accel/accel0 --ulimit memlock=-1:-1 --group-add keep-groups \
  localhost/juiced-rewrite:dev
```

`system-info` then reports the `amdxdna` or `intel_vpu` device and its access. No engine in the rewrite catalog runs on an NPU yet, so passthrough currently serves detection only. I have not tested NPU passthrough, and the `memlock` limit follows the `amdxdna` driver's requirement for pinned buffers.

## Updating engines

The engine catalog, with its pinned releases and digests, is compiled into `lemond`. Install engines from the console or the API:

```bash
curl -X POST http://localhost:13305/v1/juiced/engines/install \
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
podman run -d --name juiced -p 13305:13305 -v juiced-data:/data localhost/juiced-rewrite:dev
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
- `/v1/system-info` and `/v1/juiced/engines` answer, their summaries are printed, and the bundled FUTO engine is installed
- `/` serves the console and its script
- a restart leaves `config.json` unchanged

Arguments after the script name go to `podman run`. With `EXPECT_GPU`, the test fails unless a vendor tool confirms a GPU from that vendor. The script removes the container and volume when it exits.

## Init and user

`tini` is PID 1. It forwards `podman stop` to `lemond` and reaps orphaned processes. The image's default user is 10001. Starting with `--user 0` runs the optional userspace update, fixes `/data` ownership and adds device groups. The entrypoint then uses `setpriv` to drop to uid 10001 with no capabilities before it starts `lemond`. Any command that does not start with `-` runs instead of the service, for example `podman run --rm localhost/juiced-rewrite:dev lemonade --help`.
