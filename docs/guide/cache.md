# Manage cache and storage

Use **Cache** to control saved prompt computation and retained weights.
Juiced does not store your client's conversation history.

## Choose a cache mode

| Mode | Behavior |
| --- | --- |
| Off | Disables Juiced's prompt reuse for the model |
| Memory | Reuses supported state while the worker remains alive |
| Persistent | Saves and restores qualified Chat Completions computation snapshots |

1. Open **Cache → Storage policy**.
2. Choose the default mode, retention period, and disk budget.
3. Select **Save changes**.

Changes remain drafts until you save. **Cancel** discards them.

## Override one model

1. Open **Cache → Model overrides**.
2. Select **Add override** and choose a model.
3. Choose its mode.
4. Select **Save changes**.

Only explicit overrides appear in this list. A model using the default stays in
the add chooser. Select **Use default** to remove an override, then save.

Persistent snapshots require a qualified engine/profile and a stable session
identity. Use Memory for Ollama, Anthropic, Responses, and MCP inference when a
persistent deployment rejects their request path. Browser service tests use
memory reuse.

Consumer conversations can outnumber the engine's physical slots. For an observed
llama.cpp slot inventory, active requests wait in Juiced's shared bounded queue;
idle sessions keep no live slot. A qualified persistent session restores its
snapshot when its turn arrives. Queue deadlines and resource pressure can still
return overload errors. Juiced does not shrink the model's context while waiting.

## Connect a client with persistent session reuse

For qualified OpenAI Chat Completions, send the full current message history
with a stable header:

```http
X-Juiced-Session-Id: your-stable-session-id
```

Use a different ID for each client conversation. Session IDs do not authenticate
users. Snapshots bind the authenticated principal, credential epoch, protocol,
session, and execution profile. Key rotation or an incompatible profile causes
a cold rebuild. Clients without usable session affinity retain the memory fallback.

A running worker keeps the input identity captured when it started. Registering
another checkpoint under the same model name does not relabel that worker's saved
computation. Changed checkpoint, companion or model-type settings appear as
pending. Loading the model explicitly applies the changed definition through
worker replacement.
Checkpoint and companion content hashes distinguish changes even when file size
and modification time stay the same. A changed profile requires fresh useful-reuse
qualification; older snapshots remain retained under their original identity.

The current fingerprint schema also records supported template, LoRA and
control-vector files, the launch executable and reported engine build. Linux also
captures the actual child executable and loaded executable-library files. Unknown
custom argument or relevant backend-environment semantics prevent persistent
qualification and provide a reason in the cache profile. Ordinary model serving remains available.

Content identity does not qualify every adapter, speculative or vision cache
operation. Linux also binds supported initial backend environment inputs and verifies
environment-selected template content against the ready engine. Physical-device,
runtime environment mutation and complete request/state identity remain unfinished.
Changed or newly loaded runtime modules prevent qualification until the model reloads and passes useful-reuse checks. The frozen
profile remains visible, and ordinary serving can continue without persistent reuse.
The Linux image has a separate CPU runtime default. An older explicit binary
override can still select the combined Vulkan package. Saved overrides remain
in force. If that runtime cannot qualify, select the packaged CPU runtime for CPU
serving or qualify the intended accelerator profile separately.
Windows and macOS process-image qualification remains unfinished. Existing receipts
cover only their named model/runtime combinations.

## Retain or forget a snapshot

1. Open **Cache → Snapshots**.
2. Search by model or consumer session.
3. Select a snapshot to inspect its size and reuse details.
4. Use the retention control to keep it, or the forget control to remove it.

Forgetting removes saved computation. The client still owns its messages.
Retained snapshots can prevent automatic reclamation. If the budget cannot
accommodate protected state, inspect retention and free space rather than
assuming that the configured budget guarantees every write will fit.

## Reclaim retained weights

Open **Cache > Retained weights**. Inspect references before reclaiming an unused
copy. Registered or active artifacts can block reclamation. This inventory is
separate from downloaded source weights and prompt snapshots.

Use [backup and restore](maintenance.md) before moving persistent data.

Snapshot storage stays bound to its original state directory and canonical root.
Keep `juiced/state.json`, the snapshot files and `.juiced-snapshot-store.json`
together in backups. Changing `JUICED_SNAPSHOT_DIR` or copying state to another
directory requires an explicit migration. Startup rejects a mismatched binding
before it can apply old snapshot journals to another directory.

An older store with snapshot records or files but no binding also requires
recovery before this version can start. Preserve both directories and restore the
previous release if needed. Do not delete the binding marker or edit journal
paths to force startup. Automatic migration is not available.

## Clean misplaced companion files

The CLI command `lemonade cleanup-cache --dry-run` previews legacy companion files
stored in the wrong repository directory. Run `lemonade cleanup-cache` to remove
eligible entries. This operation does not clear the entire download cache or saved
prompt snapshots. Reported bytes are apparent sizes, not guaranteed free disk space.

Unload affected models and finish downloads or metadata changes before cleanup.
An unresolved worker stop or retained preparation writer can also block deletion.
Review unknown custom arguments if cleanup reports `artifact_reference_unknown`.
Use absolute paths for declared template, adapter or control-vector files.
Preview again before retrying after a conflict or filesystem error.

Changing the model directory also requires affected workers and accepted mutations
to drain. If settings were saved but application failed, retry after resolving the
reported ownership failure. Juiced preserves the old directory and its files.

## Interrupted snapshot writes

An interrupted save keeps its disk reservation until Juiced can establish that
its writer stopped and reclaim the file. Cold inference can still work while
saving is unavailable. Do not delete pending-write records to free space.

New writes on supported Linux kernels retain a process identity that survives a
manager restart. Juiced can then recognize the original direct process exiting,
commit cleanup and release the reservation after deleting its file. A reused PID
alone is insufficient. Unavailable platform support, changed environments and
older writes without this identity remain pending. This does not establish that
all descendants, device work or external writers stopped.

The authenticated native `POST /v1/juiced/cache/recover` endpoint accepts `{}` and
retries reconciliation. The same operation is available under the other native
API prefixes. It does not accept a file path or an override claiming a writer has
stopped. Failed metadata publication or file deletion keeps the charge for retry.
