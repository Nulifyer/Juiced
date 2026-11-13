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

## Management routes

These native routes work under `/api/v0`, `/api/v1`, `/v0` and `/v1` with the
operator key. Request and response bodies follow the v0.9.2 shapes. Errors use
`{"error":{"code","message","type"}}`, which native clients read.

| Route | What Juiced does | Differences |
|-------|------------------|-------------|
| `POST /pull` | Downloads a catalog model by its native name, or a `user.*` model from `checkpoint` (`owner/repo[:VARIANT]`) or `checkpoints.main`, verifies it and registers it as On demand. `stream: true` sends `progress`, `complete` and `error` events. `stream: true, subscribe: false` returns the job at once. | ModelScope, collections and a separate `mmproj` file are refused. Pull never installs an engine: install one first. |
| `GET /downloads`, `POST /downloads/control` | Lists pull jobs; `pause`, `cancel` and `remove` act on them. | Jobs live in memory and are gone after a restart. Downloaded files remain. |
| `GET /registry/search` | Searches Hugging Face. | `source=modelscope` is refused. `total` counts the returned results. |
| `GET /pull/variants` | Lists up to five quantizations of a Hugging Face repository. | |
| `POST /models/register` | Records a `user.*` definition that `POST /pull` then downloads. | The record lives in memory until the pull. The model appears in `/v1/models` only after its files are downloaded. |
| `POST /load` | Loads by `model_name`, pulling a missing catalog model first. `ctx_size` and `llamacpp_args` set `--ctx-size`, `--threads`, `--gpu-layers` and `--parallel`; `pinned: true` sets the Hot policy. | Options are always saved for the model (`options_saved` in the reply). Other recipe options, and a `llamacpp_backend` different from the installed one, return 422. A Disabled model returns 409. |
| `POST /unload` | Unloads `model_name`, or every loaded model when the body is empty or `{}`. | |
| `POST /delete` | Disables, unloads and unregisters `model_name`, then removes the downloaded files no other model uses. The reply reports `freed_bytes`. | Files linked from your own folders are never deleted. |
| `GET /models/{id}/files` | Lists the model's files. `include_paths=true` needs the operator key. | |
| `DELETE /models/{id}/options` | Resets the saved launch options. | Unload the model first; a loaded model returns 409. |
| `POST /install`, `POST /uninstall` | Installs or removes an engine build by `recipe` and `backend`; `stream: true` sends progress. | Uninstall refuses with 409 while a registered model uses the build. Cloud providers return 501. |
| `POST /install/dry-run` | Resolves the release file the install would fetch. `arch` stands in for GPU detection. | |
| `GET /system-info` | Adds the native `recipes` block, `Processor` and `Physical Memory` to Juiced's hardware report. | No `model_storage`, native `devices` map or `unavailable_recipes`. |
| `GET/POST /internal/aliases`, `DELETE /internal/aliases/{alias}` | Reads and changes model aliases. | The target must be a registered model. |
| `POST /internal/pin` | `pinned: true` sets Hot; `false` sets On demand. | |
| `POST /internal/shutdown` | Stops the service gracefully. | |

## Backend tiers and formats

Backend descriptors distinguish native, experimental, and guest implementations,
and record the artifact formats each importer accepts. These declarations do not
qualify every model or hardware combination. Check the selected profile in
Juiced's Runtime pane and complete an inference test.

See [runtimes and hardware](../guide/runtimes.md) for user instructions and
[generated backend metadata](https://github.com/Nulifyer/Juiced/blob/main/juiced/BACKENDS.md) for contributor reference.
