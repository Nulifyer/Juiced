---
topic: free-disk
version: 2
title: Free disk space
summary: Find what uses storage, preview deletions with the storage revision, and remove only unreferenced files, partial downloads, unused engines and stale cache entries.
---
# Free disk space

1. `juiced_storage`: free space per volume, usage by category, models on disk with `removal_frees_bytes`, and reclaimable `items` with ids. `summary: true` is a cheap free-space check.
2. Candidates, safest first:
   - Partial downloads (`partial:` ids) the operator no longer wants, and unused files (`file:` ids).
   - Engines no model uses (`engine:` ids), or `juiced_uninstall_engine` by name.
   - Session cache: `juiced_cache`, then `juiced_forget_sessions` with `preview: true`.
   - Models the operator no longer wants: set them Disabled, then `juiced_remove_model` with `delete_files: true` and `preview: true`. Files another model shares stay.
3. `juiced_delete_files` with the ids and `preview: true`. Show the operator `freed_bytes` and any refused item with its reason. Apply only what they approve: the same `items` plus `expected_revision` from the preview (`storage_revision` for `juiced_remove_model`).
4. If the apply reports `storage_revision_conflict`, storage changed: preview again. `storage_item_refused` means an item is in use; drop it or ask the operator.
5. Run `juiced_storage` again and report the space that came back.

`legacy:` ids are v0.9.2 leftovers a rollback uses. Delete them only when the operator names them, with `legacy: true`. Juiced never deletes from read-only library folders or anything a model, download, worker or saved session uses.
