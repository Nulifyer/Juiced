---
topic: increase-context
version: 1
title: Change a model's context
summary: Read saved and effective settings, preview the memory fit for a new context, apply with reload and verify the context the engine reports.
---
# Change a model's context

1. `juiced_model_details` for the model: `settings.saved`, the effective launch, the trained context and the current fit.
2. `juiced_set_model_settings` with `preview: true` and `changes: {"context_tokens": N}`. Read `data.fit`: `fits`, `total_bytes`, `available_bytes` and `largest_context_that_fits`.
3. If it does not fit, explain the bound with those numbers. Options: a smaller `context_tokens`, fewer `parallel_slots` (context is shared by slots), fewer `gpu_layers`, or a smaller quant. Context above the trained context is not useful.
4. Apply with the same changes and `apply: reload` if the model is loaded, or `next_load` otherwise.
5. Verify with `juiced_model_details`: the effective context must equal what you set. If the reload failed, the result says whether the previous settings were restored; read `juiced_logs` for the model.

Per-request context is the total context divided by `parallel_slots`. Say both numbers.
