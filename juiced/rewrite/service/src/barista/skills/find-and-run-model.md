---
topic: find-and-run-model
version: 1
title: Find and run a model that fits
summary: Search a family, compare quants against this host at a target context, download, add with the right engine, load, test and report speed.
---
# Find and run a model that fits

1. `juiced_hardware`: GPU memory, RAM and the backends this host can run.
2. `juiced_search_models` with the family words (for example `Qwen3 MoE`), the `task`, and `context_tokens` if the operator named one. Results are ordered by the publisher's data, not by release date; say which release you picked and why.
3. `juiced_inspect_repo` on the candidate: license, gated status, companion files (such as `mmproj` for vision) and every quant with its size. MoE models load all experts: the whole file size counts, not the active parameters.
4. Pick the largest quant whose `fit.verdict` is `fits`, `partial_offload` or `cpu_only`, leaving room for the current Hot models. If none fits, say so and stop.
5. `juiced_download` with `repo` and `quant` (companion files are added automatically when listed), then `juiced_wait_job` until the job is `complete`.
6. `juiced_add_model` with the `artifact` or `path` from `juiced_list_local_files`. If it reports `engine_not_installed`, run `juiced_install_engine` with the named engine and backend, wait, and add again.
7. `juiced_fit` on the new model at the target context; adjust with `juiced_set_model_settings` if needed.
8. `juiced_load`, then `juiced_test_model` with `kind: chat`. Report the measured `decode_tokens_per_second` and `prompt_tokens_per_second` from the result, not an estimate.

Never convert or quantize a model: Juiced only uses published files. If the newest release has no compatible quant, say so and offer the newest one that has.
