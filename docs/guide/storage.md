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
| `/data/acquisitions.json` | In-flight downloads, so they survive a restart | Entries leave when a download's model exists or the download is removed |
| `/data/api-keys.json` | Access keys, stored as hashes | Only keys you revoke |
| `/data/file-checksums.json` | Checksums of model files already read in full | Never. You can delete it to make Juiced read every file again |
| `/library/*` | Read-only weight mounts | Never |

Logs stay in memory and in the container log; they use no space on `/data`.

The volume list shows mounts that report the same free and total space once, for
example `/data, /library` when both are datasets of one pool.

### v0.9.2 leftovers

A volume that ran Juiced v0.9.2 still holds `/data/cache` (native engine builds,
about 29 GB on a typical install) and `/data/prompt-state`. This release does not
use them. **Usage** totals them as **v0.9.2 leftovers**, and **Reclaim space**
lists each one. Juiced never removes them on its own. `/data/config` is listed
too as **v0.9.2 settings**, kept for settings import.

## Model file checksums

Juiced computes a model file's SHA-256 by reading the whole file the first time
it uses the file. It saves the result in `/data/file-checksums.json` and reuses
it while the file keeps the same inode, size, modification time and change time.
Adding, starting and loading a model then skip the read, also after a restart.

If any of those four details changes, Juiced reads the file again. It refuses a
stored file whose content no longer matches its checksum. Juiced always reads a
download or upload in full when it arrives, and reads a file linked from a
library mount on its first load.

A saved checksum does not catch a change that leaves all four details untouched,
such as silent disk corruption. To check every file again, stop Juiced, delete
`/data/file-checksums.json` and start it. To check every file after each
restart, set the environment variable `JUICED_REMEMBER_CHECKSUMS=0`. Reading
takes about one second per GiB, so a start with 68 GiB of stored models then
takes about a minute.

The first start after updating from a release without this file reads every
stored model once.

## Reclaim space

**Reclaim space** lists files that are not models, partial downloads, engines no
model uses, and v0.9.2 leftovers, largest first. A file that is not a model is a
companion nothing uses (a projector or text encoder), part of an unfinished
split download, or a file Juiced can't read as a model; its row gives the
reason.

1. Select the items to delete.
2. Select **Review**. The button shows the selected size. The dialog shows the
   space it frees and which downloads or models each item affects.
3. Select **Delete** with the item count, such as **Delete 2 items**. Juiced
   checks every reference again before deleting.

Juiced refuses an item that a model registration, a running download, a running
worker or a saved session still uses, and never deletes from a read-only library
mount. If storage changed since the review, nothing is deleted; review again.

To remove a model and its files, use **Remove model** on the model's **Manage**
tab under **Models**. Files another model shares stay until the last model that
uses them is removed.
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
