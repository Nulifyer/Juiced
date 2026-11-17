# Start with platform Compose files

The [Compose files](https://github.com/Nulifyer/Juiced/tree/main/deploy/compose)
share one service definition and persistent data volume. Choose one hardware
override. Run these commands from a checkout of the repository:

```bash
cd deploy/compose
```

The examples pin the Linux x86-64 release image `v0.11.6`. They publish the console
and API at `http://localhost:8080`. They do not download model weights or engines.
Juiced installs an engine when you add a model that needs it, or when you
install one under **Engines & hardware**.

## CPU

```bash
docker compose up -d
```

Juiced picks the llama.cpp CPU build for new models on a host without a GPU.

## NVIDIA CUDA

Install the host NVIDIA driver and configure the
[NVIDIA Container Toolkit for Docker](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html#configuring-docker).
Confirm that `nvidia-smi` works on the host.

```bash
docker compose -f compose.yaml -f nvidia.yaml up -d
docker compose -f compose.yaml -f nvidia.yaml exec juiced nvidia-smi
```

This override uses Docker's
[GPU reservations](https://docs.docker.com/compose/how-tos/gpu-support/).
It exposes all NVIDIA GPUs. To restrict access, replace `count: all` with
`device_ids: ["0"]` or your GPU UUID.

Open **Engines & hardware** and check that the GPU appears in the machine
summary. Install the llama.cpp CUDA build, add a model, and send a test request.
Engines download into `/data`, not into the image.

### NVIDIA with Podman and CDI

For a Podman host, install `podman-compose` and configure NVIDIA
[Container Device Interface support](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/cdi-support.html).
Confirm that `nvidia-ctk cdi list` includes `nvidia.com/gpu=all`.
Use the CDI override instead of `nvidia.yaml`:

```bash
podman-compose -f compose.yaml -f nvidia-cdi.yaml up -d
podman-compose -f compose.yaml -f nvidia-cdi.yaml exec juiced nvidia-smi
```

The engine and model steps are the same as for Docker.

## AMD or Intel Vulkan

Install the host GPU driver. Check the device permissions and group IDs:

```bash
ls -ln /dev/dri
getent group render
getent group video
export RENDER_GID=$(getent group render | cut -d: -f3)
export VIDEO_GID=$(getent group video | cut -d: -f3)
docker compose -f compose.yaml -f vulkan.yaml up -d
```

Use the actual numeric device group IDs if your host uses different group names.
The override requires both variables and exposes `/dev/dri`. The image includes
Mesa Vulkan drivers. Install the llama.cpp Vulkan build under
**Engines & hardware**. GPU support still depends on the driver and device.

## AMD ROCm

Install the host AMD GPU driver and check
[AMD's container prerequisites](https://rocmdocs.amd.com/en/latest/install/docker-containers.html).
Both `/dev/dri` and `/dev/kfd` must exist.

```bash
ls -ln /dev/dri /dev/kfd
export RENDER_GID=$(getent group render | cut -d: -f3)
export VIDEO_GID=$(getent group video | cut -d: -f3)
docker compose -f compose.yaml -f rocm.yaml up -d
```

Set the group IDs to match your devices. Install the llama.cpp ROCm build under
**Engines & hardware**, add a model, and test inference. Passing devices through
does not install ROCm or prove GPU support. Use the Vulkan override when your
hardware supports Vulkan but has no compatible ROCm build.

Apple Metal requires a native macOS process. Linux containers cannot use Metal.
See [runtimes and hardware](../runtimes.md) for other accelerator requirements.

## Change ports or images

Set these variables in your shell or in `deploy/compose/.env`:

```bash
export JUICED_PORT=8081
export JUICED_IMAGE=ghcr.io/nulifyer/juiced:v0.11.6
```

`JUICED_BIND_ADDRESS` defaults to `127.0.0.1`. Follow
[access and API keys](../access.md) before changing it for remote access.
To use a local source build, run `CONTAINER_ENGINE=docker juiced/rewrite/container/build.sh`
from the repository root. It tags the image `localhost/juiced-rewrite:dev`. Set
`JUICED_IMAGE` to that tag.

## Inspect, stop, and preserve data

Use the same file arguments for each operation. For NVIDIA Docker:

```bash
docker compose -f compose.yaml -f nvidia.yaml config
docker compose -f compose.yaml -f nvidia.yaml ps
docker compose -f compose.yaml -f nvidia.yaml logs --tail 100 juiced
docker compose -f compose.yaml -f nvidia.yaml down
```

For Podman, replace `docker compose` with `podman-compose` and use `nvidia-cdi.yaml`.
The default project is `juiced`, and its data volume is `juiced_juiced-data`.
Keep that project name when changing hardware overrides or images to reuse data.
Use a different project name with `-p` for an isolated test deployment.
`down` preserves the volume. `down -v` deletes downloaded weights and server state.
Do not run two managers against the same writable data volume.
