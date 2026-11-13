# Manage storage

Juiced keeps everything it writes on the `/data` volume and reads extra weights
from read-only library mounts. Open **Engines & hardware → Storage** to see how
much space each store uses, what is safe to delete, and how much room is left.

## What lives where

| Path | Contents | Deleted by Juiced |
| --- | --- | --- |
| `/data/config.json`, `/data/.juiced-native-settings` | Settings and model registrations | Never |
| `/data/artifacts` | Downloaded and imported model weights, and partial downloads | Only unused files you reclaim |
| `/data/huggingface/hub` | Hugging Face cache Juiced owns | Only unused files you reclaim |
| `/data/runtime-packages` | Installed engines (often a separate volume) | Only engines no model uses, when you reclaim them |
| `/data/kv-cache` | Saved sessions and manual snapshots | Saved sessions are evicted to stay within the session budget |
| `/data/acquisitions.json` | In-flight downloads, so they survive a restart | Entries leave when a download finishes or is dismissed |
| `/library/*` | Read-only weight mounts | Never |

Logs stay in memory and in the container log; they use no space on `/data`.

### v0.9.2 leftovers

A volume that ran Juiced v0.9.2 still holds `/data/cache` (native engine builds,
about 29 GB on a typical install) and `/data/prompt-state`. Juiced lists them under
**v0.9.2 leftovers** and never removes them on its own. Delete them from the
Storage page when you no longer plan to roll back. `/data/config` is listed too
but kept: rollback and settings import read it.

## Reclaim space

**Reclaim space** lists unused model files, partial downloads, engines no model
uses, and v0.9.2 leftovers, largest first.

1. Select the items to delete.
2. Select **Review**. The dialog shows the space it frees and which downloads or
   models each item affects.
3. Select **Delete**. Juiced checks every reference again before deleting.

Juiced refuses an item that a model registration, a running download, a running
worker or a saved session still uses, and never deletes from a read-only library
mount. If storage changed since the review, nothing is deleted; review again.

To remove a model and its weights, use the model's page under **Models**. Files
another model shares stay until the last model that uses them is removed.
**Models on disk** shows what removing each model would free.

## Free-space guard

Downloads, engine installs and snapshot saves check free space before they
start. If the volume would drop below the reserve, Juiced refuses with the
shortfall instead of failing halfway. When a volume falls below the low-space
threshold, the Dashboard shows **Low disk space** under **Needs attention**.

Both thresholds are set in `config.json`:

```json
"storage": { "reserve_bytes": 2147483648, "low_space_bytes": 21474836480 }
```

The defaults are a 2 GiB reserve and a 20 GiB warning. Restart Juiced after
changing them.

## Downloads across restarts

Model downloads continue after Juiced restarts. Juiced resumes each file from
the bytes already on disk when the source supports it, and starts the file over
when it doesn't. A download you paused stays paused until you resume it.
