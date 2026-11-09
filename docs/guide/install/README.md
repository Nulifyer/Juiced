# Install Juiced

Run the Juiced release container on a Linux x86-64 Docker host.
The browser console and model API share HTTP port `8080`.

## Start a local server

The example pins `v0.9.2`, the release used for this guide. Check the
[Juiced releases](https://github.com/Nulifyer/Juiced/releases) before selecting
another version. Keep the selected tag or digest in your deployment definition.

```bash
docker run -d --name juiced \
  -p 127.0.0.1:8080:8080 \
  -v juiced-data:/data \
  --restart unless-stopped \
  ghcr.io/nulifyer/juiced:v0.9.2
```

1. Open `http://localhost:8080`.
2. Wait for the catalog to appear.
3. Follow [run your first model](../README.md).

The image contains no model weights. Model downloads require network access.
The persistent `/data` mount holds configuration, downloaded weights, runtime
packages, and saved computation. The container runs as UID/GID `10001:10001`.

## Use Compose

Ready-to-run [platform Compose files](compose.md) cover CPU, NVIDIA CUDA,
AMD or Intel Vulkan, and AMD ROCm. NVIDIA also has a Podman CDI override.
For example, from a repository checkout on a configured NVIDIA Docker host:

```bash
cd deploy/compose
docker compose -f compose.yaml -f nvidia.yaml up -d
```

Compose gives its volume a project-specific name. Preserve that volume identity
when updating or renaming a deployment. For a bind mount, create a dedicated
writable directory owned by `10001:10001` before starting the container.

## Give the container GPU access

Configure the host driver first. Then add only the devices and groups required
by your chosen runtime. The default image includes a Vulkan llama.cpp runtime.
CUDA and ROCm profiles require additional backend packages. Follow the
[platform Compose guide](compose.md) for device mappings and host prerequisites.
Apple Metal requires a native macOS process.

After startup, check **Backends & hardware**, select the model's runtime, and
complete a test request. Device detection alone does not establish inference.
See [runtimes and hardware](../runtimes.md) for NPU and experimental requirements.

## Reuse weights you already have

Mount the existing weight directory read-only, for example at `/library`.
Under **Library → Import GGUF**, use its path inside the container.
A server-side path must exist inside the container and be readable by UID `10001`.
Keep the original mount available for as long as a registration uses those files.

## Other installation routes

The native core can be built on Linux, Windows, and macOS. See the
[contributor guide](https://github.com/Nulifyer/Juiced/blob/main/CONTRIBUTING.md) for source builds.
The inherited Lemonade MSI, distro packages, desktop app, and embeddable downloads
are separate upstream products. They do not install the Juiced console described
in these guides.

Use [access and API keys](../access.md) before allowing remote connections.
