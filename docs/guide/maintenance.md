# Update, back up, and restore

Juiced keeps all of its writable state on the `/data` volume. Back up that
volume and any external weight mounts you cannot download again, and keep one
running Juiced per `/data`.

## Know what persists

[Manage storage](storage.md) lists every path under `/data` and what each one
holds. A backup of the whole volume covers settings and model registrations
(`/data/config.json`), access keys (`/data/api-keys.json`, which stores only key
hashes), downloaded weights, installed engines, saved sessions and in-flight
downloads. A container image contains none of these.

Juiced locks its stores with `.juiced-store.lock` files. Keep them in place, and
never point two running containers at the same `/data`.

## Make a consistent backup

1. Record the current image tag or digest and the deployment definition.
2. Wait for active requests and downloads to finish.
3. Stop the Juiced container.
4. Back up the complete `/data` volume with your volume backup tool.
5. Back up any external source weights you cannot download again.
6. Keep the deployment settings and secret references with the backup.
7. Start Juiced and send a real request.

A copy taken while Juiced runs can capture half-written files.

## Know when a release is out

Once a day, Juiced asks GitHub for the latest Juiced release. The request sends
no data about your installation, and a failed check stays silent. When a newer
release exists, the sidebar shows a **vX.Y.Z available** link next to the
version. To stop the check, open the service status menu in the top bar and turn
off **Release notices**. The setting is saved as `"release_check": false` in
`config.json`.

## Update a Compose deployment

1. Make a consistent backup.
2. Change the image tag or digest in `compose.yaml`.
3. Keep the volume identity, mount targets, devices and permissions.
4. Pull and recreate the service:

```bash
docker compose pull juiced
docker compose up -d juiced
docker compose logs --tail 100 juiced
```

5. Open the console and check model IDs, policies and loaded state. Each chat
   model's **Check** on its Overview runs after it starts with new settings or a
   new engine.
6. Send a representative request from each connected client.

The container health check shows that the service answers. It does not prove
that every model, accelerator or voice works.

## Move from v0.9.2

Juiced v0.10 and later use the same `/data` volume and port `8080` as v0.9.2.

1. Make a consistent backup of the v0.9.2 volume.
2. Change the image to `v0.11.11`. Keep the volume, mounts, devices and port.
3. Start the container. On its first start, Juiced adopts the models in
   `/data/config` in the background. Each model keeps its ID, loading policy and
   settings. The weights stay where they are, and Juiced only reads the v0.9.2
   files.
4. Check the models under **Models → Installed**. For an enabled model whose
   engine is missing, Juiced installs the engine first. A Disabled model waits
   until you install its engine under **Engines & hardware**.
5. Send a real request from each connected client.

`GET /v1/juiced/state` reports adoption progress and each model's outcome under
`native_adoption`. When everything works, you can delete the v0.9.2 engine builds
and prompt snapshots under **Engines & hardware → Storage**. See
[v0.9.2 leftovers](storage.md#v092-leftovers).

## Restore

1. Stop the current container.
2. Restore the matching `/data` backup into a separate volume.
3. Start the image tag recorded with that backup, with the original weight mounts.
4. Check the model list and a real request before reconnecting clients.

Keep the failed volume until the restored one works. An older image may not read
state a newer one wrote, so restore a backup together with the image that made it.
