# Update, back up, and restore

Preserve Juiced's writable data and any external weight mounts when replacing
the container. Use one writable manager per state directory.

## Know what persists

| Container path | Contents |
| --- | --- |
| `/data/config` | Native configuration, model registrations, profiles, and Juiced state |
| `/data/cache` | Model/runtime caches and downloaded packages |
| `/data/huggingface` | Hugging Face cache used by the container |
| `JUICED_SNAPSHOT_DIR`, when set | Prompt snapshots at the configured path |
| External read-only mounts such as `/library` | Original weights referenced by registrations |

Without `JUICED_SNAPSHOT_DIR`, snapshots live under the Juiced state directory
inside `/data/config`. Backing up all of `/data` covers either default storage
or a snapshot directory configured inside that mount. Include a separate snapshot
mount in the backup if you configured one.

Preserve the snapshot root's `.juiced-snapshot-store.json` with `juiced/state.json`
and its files. The binding records both canonical directory paths. Restore them
at their original paths. A different path, lost marker or copied state requires
explicit migration; this version does not provide that operation. Unbound legacy
snapshot inventories or nonempty roots also block startup. Rehearse an update on
an isolated copy before replacing a release that created such data.

Juiced protects its managed cache, configuration, model-cache and snapshot roots
against another participating manager. Keep `.juiced-store.lock` files in place.
On POSIX systems, Juiced also locks the directory inode so removing that control
file cannot release a running manager's ownership. Moving or replacing whole
directories and nested-root exclusion still require separate protection.
A directory change must finish active work and acquire the destination before it
can apply. Failed application can retain both roots until retry or shutdown.

Use external read-only source mounts for original weights. A managed read-only
cache needs an existing usable control file to acquire ownership. Root locks do
not prove that older binaries, surviving helpers or external tools stopped.

## Make a consistent backup

1. Record the current image tag or digest and deployment definition.
2. Stop new client work and wait for active requests and management jobs to settle.
3. Stop the Juiced container.
4. Back up the complete `/data` volume with your volume backup tool.
5. Back up any external source weights you cannot download again.
6. Keep the deployment settings and secret references with the recovery instructions.
7. Start Juiced and verify a real request.

A live file copy can capture partially updated state. A container image alone
contains neither your model library nor your configuration.

## Update a Compose deployment

1. Create a consistent backup.
2. Change the image tag or digest in `compose.yaml`.
3. Keep the existing volume identity, mount targets, devices, and permissions.
4. Pull and recreate the service:

```bash
docker compose pull juiced
docker compose up -d juiced
docker compose logs --tail 100 juiced
```

5. Open the console and check model IDs, policies, runtime profiles, and loaded state.
6. Complete a representative request from each connected service.

Container health checks establish manager responsiveness. They do not prove
that every model, accelerator, or voice is working.

## Restore or roll back

1. Stop the current container.
2. Restore a matching data backup into a separate recovery volume.
3. Configure the matching image and original external weight mounts.
4. Start one manager against the recovery data.
5. Check the catalog, profiles, and a real request before reconnecting clients.

Keep the failed data volume until recovery is verified. An older image may not
understand newer state. Restoring its matching backup avoids assuming backward
compatibility. Never run two writable managers against the same data directory.
