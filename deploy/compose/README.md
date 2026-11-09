# Juiced Compose

Run from this directory. Choose the base file and one hardware override:

| Hardware | Command |
| --- | --- |
| CPU | `docker compose up -d` |
| NVIDIA CUDA | `docker compose -f compose.yaml -f nvidia.yaml up -d` |
| AMD or Intel Vulkan | `docker compose -f compose.yaml -f vulkan.yaml up -d` |
| AMD ROCm | `docker compose -f compose.yaml -f rocm.yaml up -d` |
| NVIDIA with Podman CDI | `podman-compose -f compose.yaml -f nvidia-cdi.yaml up -d` |

Open <http://localhost:8080>. The image contains no model weights. CUDA and ROCm
backend packages require installation through **Backends & hardware**.
Vulkan and ROCm need `RENDER_GID` and `VIDEO_GID` set to the host device group IDs.

Read the [platform setup guide](../../docs/guide/install/compose.md) for host
prerequisites, model runtime selection, ports, data persistence, and stop commands.
