# Native management API compatibility

Juiced retains native command and route compatibility from its serving core.
For current operator instructions, use [Juiced management](juiced.md).
For client requests, use the [API guide](README.md).

This page remains at its original path because native clients can link to it.
Juiced's current guides and native contracts define its supported behavior.

## Health and model list fields

`GET /v1/health` keeps the native fields: `status`, `version`, `model_loaded` (the
most recently used loaded model or `null`), `all_models_loaded`, `max_models`,
`pinned_models`, `telemetry` and `update_check_done`. Each `all_models_loaded` entry
has `model_name`, `type` (`llm`, `embedding`, `transcription`, `tts` or `image`),
`recipe`, `recipe_options.ctx_size`, `device`, `status`, `loaded`, `backend_alive`,
`is_busy`, `pinned` (Hot policy), `pid`, `last_use`, `max_context_window`,
`context_length` and `context_observation`. Juiced limits loaded models by memory,
not by count, so `max_models` counts enabled models of each type. It has no
`websocket_port`: realtime transcription uses `/v1/realtime` on the HTTP port.

`GET /v1/models` keeps `show_all=true` and the `service` filter, and its entries
carry `recipe`, `downloaded`, `labels`, `context` and `context_length`. Native
`checkpoint`, `checkpoints`, `size` and `suggested` are not reported.

## Backend tiers and formats

Backend descriptors distinguish native, experimental, and guest implementations,
and record the artifact formats each importer accepts. These declarations do not
qualify every model or hardware combination. Check the selected profile in
Juiced's Runtime pane and complete an inference test.

See [runtimes and hardware](../guide/runtimes.md) for user instructions and
[generated backend metadata](https://github.com/Nulifyer/Juiced/blob/main/juiced/BACKENDS.md) for contributor reference.
