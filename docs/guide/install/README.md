# Install Juiced

Run the Juiced release container on a Linux x86-64 Docker host.
The browser console and model API share HTTP port `8080`.

## Start a local server

The example pins `v0.11.19`, the release used for this guide. Check the
[Juiced releases](https://github.com/Nulifyer/Juiced/releases) before selecting
another version. Keep the selected tag or digest in your deployment definition.

```bash
docker run -d --name juiced \
  -p 127.0.0.1:8080:8080 \
  -v juiced-data:/data \
  --restart unless-stopped \
  ghcr.io/nulifyer/juiced:v0.11.19
```

1. Open `http://localhost:8080`.
2. Wait for the top bar to show **Online** and the Dashboard to load.
3. Follow [run your first model](../README.md).

The image contains no model weights and no engines. Juiced downloads both into
`/data` when you ask for them, so downloads need network access. The persistent
`/data` mount holds configuration, downloaded weights, installed engines, and
saved conversations. The container runs as UID/GID `10001:10001`.

If the volume was used by Juiced v0.9.2, the first start adopts its models and
settings. See [move from v0.9.2](../maintenance.md#move-from-v092).

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
by the engine build you plan to use. Follow the
[platform Compose guide](compose.md) for device mappings and host prerequisites.
Apple Metal requires a native macOS process.

After startup, open **Engines & hardware**. Check that your GPU appears in the
machine summary without **No access**. Install the engine build you need, add a
model, and complete a test request. Device detection alone does not prove that
inference works. See [runtimes and hardware](../runtimes.md) for NPU and
experimental requirements.

## Reuse weights you already have

Mount the existing weight directory read-only at `/library`, for example
`-v /srv/models:/library:ro`. The default configuration lets the console read
`/data` and `/library`.

1. Open **Models → Discover**.
2. Select **Add file**, then **Pick from the server…**.
3. Open `/library` and pick the model file.
4. Under **Keep the file**, choose **Where it is** to use the file in place.
5. Select **Install**.

The files must be readable by UID `10001`. Keep the mount available for as long
as a model uses those files. See [import a local file](../models.md#import-a-local-file).

## Other installation routes

See the [contributor guide](https://github.com/Nulifyer/Juiced/blob/main/CONTRIBUTING.md)
to build Juiced from source.
The inherited Lemonade MSI, distro packages, desktop app, and embeddable downloads
are separate upstream products. They do not install the Juiced console described
in these guides.

Use [access and API keys](../access.md) before allowing remote connections.
