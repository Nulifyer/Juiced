---
topic: setup-voice
version: 1
title: Set up Home Assistant voice
summary: Pick speech-to-text, text-to-speech and a chat model that fit together, enable Wyoming, verify each, and hand over the connection details.
---
# Set up Home Assistant voice

Preconditions: operator permission to change models; enough memory for three models at once.

1. `juiced_status` and `juiced_hardware`: note the memory budget, free headroom and the recommended backends.
2. `juiced_list_models` with `task` set to `transcription`, `tts` and `chat`. Prefer models that are already downloaded.
3. Missing a model: `juiced_search_models` for the task, compare `fit` on each quant, then `juiced_download` and `juiced_wait_job`, then `juiced_add_model`. Install a missing engine first with `juiced_install_engine` when `juiced_add_model` reports `engine_not_installed`.
4. Check the three fit together: `juiced_fit` on each, and compare the sum of `total_bytes` with the free budget from `juiced_status`. If they do not fit, pick a smaller chat quant or a lower context. Do not guess sizes.
5. `juiced_set_policy` the chat model to `hot`, and the speech models to `on-demand` or `hot` as the budget allows.
6. `juiced_set_wyoming` with `enabled: true`, `stt_model`, `tts_model` and a `tts_voice` from `juiced_connections`.
7. Verify: `juiced_test_model` with `kind: transcribe` on the speech-to-text model, `kind: speech` on the text-to-speech model and `kind: chat` on the chat model. Report each result.
8. `juiced_connections`: give the Wyoming host and port (default 10300) and the OpenAI base URL for the Home Assistant conversation agent.

Recovery: if a load fails, read `juiced_logs` for that model (`level: error`), then lower its context with `juiced_set_model_settings` or choose a smaller quant. Wyoming has no authentication, so tell the operator to expose it only on a trusted network.
