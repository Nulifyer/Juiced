---
topic: free-disk
version: 1
title: Free disk space
summary: Find what uses storage, then remove only unreferenced files, unused engine builds and stale cache entries with previews.
---
# Free disk space

1. `juiced_storage`: usage by category and free space per volume. If it reports `not_supported`, use `juiced_list_local_files` (sizes and `used_by`) and `juiced_list_engines` (installed builds and the models using them) instead.
2. Candidates, safest first:
   - Session cache: `juiced_cache`, then `juiced_forget_sessions` for a model with `preview: true`.
   - Engine builds no model uses: `juiced_uninstall_engine` with `preview: true`.
   - Models the operator no longer wants: `juiced_remove_model` with `preview: true`. The model must be Disabled first.
   - Unreferenced files: `juiced_delete_files` with `preview: true`.
3. Show the operator the preview totals (`bytes_freed`) and what stays. Apply only what they asked for.
4. Run `juiced_storage` or the same list again to confirm the space came back.

Never delete a file a registered model, alias or job uses. Read-only library roots cannot be deleted from Juiced.
