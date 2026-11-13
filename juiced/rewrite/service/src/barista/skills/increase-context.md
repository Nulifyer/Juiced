---
topic: increase-context
version: 2
title: Change a model's context
summary: Read saved and effective settings, preview the memory fit for a new context or KV cache type, apply with reload and verify the context the engine reports.
---
# Change a model's context

1. `juiced_model_details` for the model: `settings.saved`, the effective launch, the trained context and the current fit.
2. `juiced_set_model_settings` with `preview: true` and `changes: {"context_tokens": N}`. Read `changed` (before -> after) and `data.memory`: `before_bytes`, `after_bytes`, `available_bytes`, `fits` and `largest_context_that_fits`.
3. If it does not fit, explain the bound with those numbers and preview an option: `cache_type_k` and `cache_type_v` set to `q8_0` (about half the KV memory), fewer `parallel_slots` (context is shared by slots), fewer `gpu_layers`, or a smaller quant. Context above the trained context is not useful.
4. Apply the same changes with `apply: reload` if the model is loaded, or `next_load` otherwise.
5. Verify with `juiced_model_details`: the effective context must equal what you set. If the reload failed, the error says whether the previous settings were restored; read `juiced_logs` for the model.

Per-request context is the total context divided by `parallel_slots`. Say both numbers. Typed settings apply to llama.cpp models; other engines report `settings_unsupported`.
