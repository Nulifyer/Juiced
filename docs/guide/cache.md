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

## Start new conversations warm

Most clients send the same long system prompt at the start of every
conversation. Home Assistant, the Barista assistant and character cards are
examples. In Persistent mode, Juiced remembers the opening messages of each saved
conversation. When a new conversation opens with the same messages, Juiced
restores that saved state first. The engine then processes only the new
messages. This also works after the other slots were used by other clients,
and after an unload or restart.

- Juiced restores a saved state only if the shared opening messages amount to
  about 2 KiB of request text or more. Below that, prefilling costs less than
  the restore.
- If several saved states share the same opening messages, Juiced restores the
  smallest one.
- With several slots, Juiced sends a request to the slot that already holds
  its opening messages. If the request's first message changed partway, Juiced
  reuses that client's own slot instead of evicting another client.
- The engine checks the actual tokens, so a restored state never changes the
  reply. Juiced keeps the message fingerprints in `session-lineage.json` next
  to the configuration. The file holds digests, not message text.

Hybrid models such as Qwen3.5 and Qwen3.6 MoE keep recurrent state. They can
reuse a prompt only up to a point where llama.cpp saved a checkpoint, which is
the start of a message or the end of a prompt. If a system prompt changes
partway, for example with the current time, free memory or live device states,
these models process it again from the beginning. Keep changing values out of
the system prompt, or put them in a later message. Barista keeps live figures
out of its system prompt and reads them with `juiced_status`.

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
