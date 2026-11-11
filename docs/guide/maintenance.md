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
On POSIX systems, Juiced also locks directory inodes. Removing a control file
cannot release a running manager's ownership. Separate managers cannot own
overlapping parent/child roots. Independent sibling roots can run together.
Managed roots and their ancestors must be readable for these locks.
Trailing separators and `/.` aliases resolve to the same root before startup or
configuration changes. A normal-path restart keeps the same snapshot-store identity.

A manager can retain nested roots beneath an ancestor it already owns. A later
request for a broader root rejects if it would drop live narrower ownership.
Layouts requiring that expansion need a separate storage migration. Moving or
replacing whole directories remains outside this protection. Windows currently
protects exact configured roots; parent/child exclusion is not implemented there.
A directory change must finish active work and acquire the destination before it
can apply. Failed application can retain both roots until retry or shutdown.

Use external read-only source mounts for original weights. A managed read-only
cache needs an existing usable control file to acquire ownership. Root locks do
not prove that older binaries, surviving helpers or external tools stopped.

## Inspect snapshot storage before startup

Use the server executable to inspect snapshot state when ordinary startup requires
recovery:

```bash
lemond /data/cache /data/config --inspect-snapshot-store
```

The command uses the default snapshot directory or the existing
`JUICED_SNAPSHOT_DIR` environment setting. It prints one JSON report. Exit status
0 means the inspected metadata and files passed these checks. Status 2 means
inspection found an issue or reached a limit. Serving and logging options cannot
be combined with this command.

Inspection starts no workers, claims no writable roots and changes no source
files or directories. Reads can update filesystem access times. The report
checks snapshot binding, files, checksums, publication records and retained writer
metadata. It does not inspect every import, runtime package or historical job.

`application_authorized` and `writer_termination_verified` remain false in every
report. A successful inspection never authorizes migration, releases retained
charges or proves a consistent backup. Concurrent writers can change storage
after it is observed. Preserve the state and files when an issue remains.
On Windows, run inspection after stopping known writers. Retained directory
handles can temporarily deny directory changes while the command reads files.
This sharing restriction does not stop file-content writers or establish quiescence.

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

## Legacy configuration-directory upgrades

Native startup can relocate legacy configuration metadata as one recoverable
set. This includes model registrations, selected profiles and saved options,
retained jobs, configuration and pending native-helper records. If the cache root
changes too, aliases move to the new cache. Runtime binaries and model weights
stay in their original locations.

Keep a consistent backup of the source cache, destination configuration and any
external model or snapshot roots before changing paths. The migration retains its
source files and a private staged generation. Restart with the same paths and
binary to resume an interrupted publication. Later settings edits remain active
and are not replaced by the retained source.

If startup reports `configuration_migration_recovery_required`, preserve both
stores and the migration files. Resolve the reported ownership or conflict through
recovery before retrying. Do not delete the marker, merge individual JSON files,
or start an older binary against a partial destination. To roll back, restore the
complete matching backup and its original executable and paths.

This legacy metadata transport does not relocate an existing `juiced/` state
store. Snapshot path bindings, imports and unresolved writers need their own
validated recovery. Startup rejects that case without silently creating a new
state store. Successful modern-store relocation and current-candidate recovery
remain qualification gates. A resumable process restart does not prove recovery
from power loss.

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
