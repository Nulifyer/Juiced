---
topic: cache-check
version: 1
title: Check that the cache works
summary: Read session cache settings and eligibility, then prove reuse from request records, optionally with a bounded two-request check.
---
# Check that the cache works

1. `juiced_cache` with the model: global `mode`, the model's `effective` mode and `reason`, budgets, saved sessions and recent reuse evidence.
2. If `effective` is `off`, the `reason` says why (for example `engine_unsupported`). Explain it; `juiced_set_cache` can change the mode or budgets when the engine supports it.
3. Evidence of reuse is only in request records: `juiced_requests` for the model. Look for `cached_tokens` or a `session_cache.path` that restored state, and `prompt_processed_tokens` well below `input_tokens`.
4. With permission, run a bounded check: `juiced_test_model` with `kind: chat` twice with the same `input`. Compare `prompt_processed_tokens` between the two results.
5. Report what the records show. Files on disk or a successful reply are not proof of reuse.
