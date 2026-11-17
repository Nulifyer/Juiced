# Inspect and manage Juiced

Use native discovery to inspect the server. Use the browser console for routine
mutations so you can review progress, dependencies, and reload consequences.
Operator controls require the admin credential when configured.

## Read the server state

```bash
curl --fail-with-body http://localhost:8080/v1/juiced/state
curl --fail-with-body http://localhost:8080/v1/juiced/catalog
curl --fail-with-body http://localhost:8080/v1/juiced/capabilities
curl --fail-with-body http://localhost:8080/v1/juiced/protocols
```

The operator catalog includes disabled registrations. Each entry has a
`vision` boolean. It is `true` when the model's llama.cpp launch binds a vision
projector (`--mmproj`), so `/v1/chat/completions` accepts `image_url` content
parts for it. It is `false` for other engines, for models without a projector,
and for models under a protected request profile, which accepts text parts
only. Juiced forwards chat request bodies to the worker unchanged; the 2 MiB
JSON request limit includes any images. Consumer discovery at
`/v1/models` includes only enabled available models. Native capabilities report
declared operations, component status, selected execution, and unknowns.

`/.well-known/juiced.json` points clients to native discovery. Its `auth` object
says whether `read`, `inference` and `operator` routes are `open` or need a `key`.
Its `credential` object describes the key sent with the request: `presented`,
`accepted` and the `authority` it grants (`read`, `inference`, `operator` or
`null`). An open deployment ignores keys, so a mistyped key shows
`accepted: false` there instead of failing later. When every route needs a key,
this file needs one too, and a `401` is the answer.

## Discover models for a client

A client that registers chat models needs one request:
`GET /v1/models?service=chat` (read key). It lists enabled, downloaded chat models
with the fields below. Without `service` it lists every enabled model and each
alias. `GET /v1/juiced/catalog` carries the same fields for every registration,
disabled ones included, plus operator details; it is the operator view. Check
`discovery_schema_version` (currently `1`) before parsing;
`/.well-known/juiced.json` and `GET /v1/juiced/capabilities` carry the same
number. Fields not listed here are operator details and may change.

| Field | Meaning |
| --- | --- |
| `id` | Exact model ID for inference requests |
| `operations` | Served APIs; a chat model lists `chat/completions` |
| `policy`, `downloaded`, `loaded`, `state` | Usable now: not `disabled`, weights present; `loaded` says whether a worker is running |
| `context.per_request_tokens` | Tokens one request can use. Same value as `context_length` |
| `context.per_request_source` | `observed` (the running engine reported its slot size), `applied` or `configured` (computed from the launch settings the way llama-server sizes slots), `declared` (the engine sizes the context itself; this is the training context split by slots, and `--fit` may lower it), or `null` (unknown) |
| `context.declared_tokens`, `configured_tokens`, `configured_slots`, `kv_unified` | Training context, the `--ctx-size` total budget, the slot count and whether slots share one cache |
| `context.observed_per_request_tokens`, `observed_slots` | What a loaded llama.cpp worker reported on `/props`; `null` otherwise |
| `max_output_tokens` | Present only when the launch sets `--predict` |
| `vision` | `true` when image input is accepted (`image_url` parts) |
| `thinking.supported` | `true` when the template has a reasoning switch or starts the reply in a reasoning block; `null` with reason `template_renders_reasoning_only` when it only renders earlier reasoning, which does not show the model reasons; `false` when it has no reasoning markup |
| `thinking.request_control` | The request field the template reads: `chat_template_kwargs.enable_thinking` (boolean; top-level `enable_thinking` and `thinking: {"type": "disabled"}` also work), or `reasoning_effort` (top-level; llama-server copies it into `chat_template_kwargs.reasoning_effort`, and either place works). `reasoning_effort` sets how much the model reasons; on templates such as gpt-oss it cannot turn reasoning off |
| `thinking.request_values` | `[false, true]` for `enable_thinking`. For `reasoning_effort`, the levels the loaded engine reports as rendering distinctly (`chat_template_caps.reasoning_effort_levels`), else `null`. A template that echoes any string lists every level llama.cpp probes, not only those the model was trained on |
| `thinking.default` | `on` or `off` from the launch settings; else `on` for `enable_thinking` (llama-server enables it unless told otherwise) and for templates that start the reply in reasoning; else `null` |
| `tool_calls.supported` | `true`, `false` or `null` |
| `thinking.source`, `tool_calls.source` | Evidence: `engine_template_caps` or `engine_chat_template` (the loaded engine), `gguf_chat_template` (the weights' chat template), `launch_chat_template`, `launch_arguments` or `operations`; `reason` explains a `null` |
| `labels` | Native labels derived from the same evidence: `chat`, `embeddings`, `transcription`, `tts`, `image`, `vision`, `reasoning`, `tool-calling` |
| `capability_contract` | The native contract entry, including `limits.per_request_context_tokens` and `features` (`tools`, `vision`, `reasoning`: `"declared"`, `false` or `null` for unknown) |
| `resident_bytes` | Memory reserved against the model budget while loaded. An unloaded model shows the reservation its next load takes; it is not the size on disk |
| `loading_started_unix_ms` | While `state` is `loading`, when the engine process was started; absent otherwise |
| `last_load_ms` | How long the last successful load of this model on its current engine took, in milliseconds. Kept in memory until the service restarts; absent before the first load |

Context is never a naive division of the launch total: llama-server pads the total
to 256 tokens, divides it between slots unless the cache is unified, pads again and
caps the result at the training context. For example `--ctx-size 1000 --parallel 3`
gives 512 tokens per request, not 333.

The catalog response carries a top-level `revision` and a weak `ETag`. Both change
when a model is added or removed, its policy, options or load state change, or an
alias changes. Send `If-None-Match` with the last `ETag` to get `304 Not Modified`
when nothing changed. `GET /v1/models` and `GET /v1/juiced/capabilities` use the same
revision.

`GET /v1/juiced/context?model=MODEL_ID` (read key) gives the same `per_request_tokens`
and `per_request_source` for one model, loaded or not, plus the engine observation.
Operator credentials also see the worker process details.
The [capability contract](https://github.com/Nulifyer/Juiced/blob/main/juiced/CAPABILITIES.md) defines fields and their
limits. Unknown or unrecorded qualification is not a successful test.

## Read operational inventories

| Endpoint | Contents |
| --- | --- |
| `GET /v1/juiced/devices?recipe=llamacpp&backend=cpu` | Devices for the selected recipe/backend; choose an installed variant for accelerator enumeration |
| `GET /v1/juiced/execution?model=MODEL_ID` | Selected profile, available profiles, and compatibility |
| `GET /v1/juiced/preparation` | Retained legacy job records; `?id=` reads bounded output and `?cache=1` lists retained weights. Producer planning/creation/resume is retired with HTTP410 `feature_retired` |
| `GET /v1/juiced/transfers` | Acquisition jobs and their state |
| `GET /v1/juiced/cache` | Cache settings, snapshots, and operational state |
| `GET /v1/juiced/admission` | Pending work, reservations, allocations, and uncertain termination |
| `GET /v1/juiced/requests` | Bounded request metadata for diagnostics; `?limit=N` (1 to 1000, default 128) |
| `GET`/`POST /v1/juiced/requests/capture` | Opt-in body capture state; `POST {"enabled":true}` turns it on (operator) |
| `GET /v1/juiced/requests/{id}/capture` | The captured request and response bodies of one request (operator) |
| `GET /v1/juiced/state` | Version, resources, residency, v0.9.2 adoption and `release`: `check` (the release check is on), `current`, `latest` (the newest published tag or `null`), `url`, `checked_at_ms` and `newer`. With the check off, `latest` is `null` |
| `POST /v1/juiced/release` | `{"enabled": false}` turns the daily GitHub release check off and saves `release_check` in `config.json`; `true` turns it on. Returns `{"release": ...}` |
| `GET /v1/system-stats` | CPU, memory, GPU and NPU use. A GPU's `gpu_percent` is `null` with `utilization_source: "amdgpu-sysfs-unverified"` while an AMD GPU has reported only 100% busy since the service started, because some integrated GPUs report 100% while idle |
| `GET /v1/health` | Liveness plus `version`, `model_loaded`, `all_models_loaded` (one entry per running worker with `model_name`, `type`, `recipe`, `status`, `pid`, `context_length`), `max_models` and `pinned_models` per model type |

Each `GET /v1/juiced/requests` record carries `id`, `model`, `operation`,
`outcome`, `status`, `elapsed_ms`, `queue_wait_ms`, `completed_at_ms` (Unix
milliseconds), and the engine-reported `input_tokens`, `output_tokens`,
`prompt_tokens_per_second` and `decode_tokens_per_second`; unknown values are
`null`. Engines that report prompt reuse add `cached_tokens` and
`prompt_processed_tokens`, and prompt-cache requests add `session_cache` with its
`path` (`slot-hit`, `restored`, `cold` or `bypass`), `slot`, and `restore_ms` when
restored. `protocol` names the sender: `openai`, `ollama`, `anthropic`,
`automatic1111`, `mcp`, `realtime`, `wyoming`, `console` (the Juiced console,
detected by `Sec-Fetch-Site: same-origin`), `internal` (checks and Tune) or
`native` (other Juiced routes). `key_name` is the API key's name, or
`LEMONADE_API_KEY` / `LEMONADE_ADMIN_API_KEY` for environment keys, and `null`
without a key. Records are newest first. The service keeps the last 1000 records
or 7 days, whichever is less, in `request-history.json` in the configuration
directory, so history survives a restart. Request IDs continue after restored
ones. A record with `"captured": true` has a capture.

Capture is off by default, memory only and operator only. While on, the service
keeps the last 20 requests' bodies, each capped at 64 KiB. Each capture has
`request` and `response` parts with `text` (null for binary bodies), `bytes`,
`truncated` and `assembled` (true when a streamed reply was reassembled into
`content`, `reasoning` and `tool_calls` text). Turning capture off deletes every
capture; a restart turns it off. Captures never reach disk, logs or metrics.

Admission observations include `physical_slots` for requests with an observed
native slot inventory. Each record binds a worker generation to its inventory,
unique claimed slot IDs and pending count. Nested cache-state and inference
tickets share a physical claim; their memory charges remain distinct. An expired
wait for these slots returns `execution_slots_busy`. Protected or leased paths
reject missing inventory with `execution_slots_unknown`; conflicting observations
use `execution_slots_changed`. Unprotected legacy requests without an inventory
retain the backend's existing behavior and have no qualified slot claim. These observations do not qualify memory peaks or latency.

URL-encode model IDs in query parameters. Request metadata is not a stored
conversation API. The default native port for a source build is `13305`.
The Juiced container explicitly uses `8080`.

## Storage

`GET /v1/juiced/storage` (read key) reports every store Juiced writes or reads.
`?summary=1` returns only `volumes`, `warnings` and `settings` without measuring
directories, for frequent polling. The full view:

```json
{
  "schema_version": 1, "summary": false, "revision": "9f3c0d2a41b7e655",
  "settings": {"reserve_bytes": 2147483648, "low_space_bytes": 21474836480},
  "volumes": [{"path": "/data", "paths": ["/data", "/data/artifacts"], "roles": ["data", "models", "session_cache"],
               "total_bytes": 0, "available_bytes": 0, "used_bytes": 0, "read_only": false, "low": false}],
  "warnings": [{"code": "low_space", "path": "/data", "available_bytes": 0, "message": "..."}],
  "categories": [{"id": "models", "title": "Model weights", "bytes": 0, "reclaimable_bytes": 0}],
  "models": [{"model": "ID", "id": "model:ID", "bytes": 0, "files": 1, "shared_bytes": 0,
              "read_only_bytes": 0, "removal_frees_bytes": 0, "shared_with": [], "engines": ["package-..."]}],
  "shared": [{"id": "file:PATH", "name": "x.gguf", "bytes": 0, "used_by": ["a", "b"], "quants": [], "read_only": false}],
  "unreferenced": [ITEM], "partials": [ITEM], "engines": [ITEM], "legacy": [ITEM],
  "leftover_files": [{"filename": "job-....part", "reserved_bytes": 0, "reclaimable": false, "message": "..."}],
  "session_cache": {"path": "/data/kv-cache", "bytes": 0, "session_bytes": 0, "snapshot_bytes": 0,
                    "sessions": 0, "budget_bytes": 0, "over_budget": false},
  "other": [{"name": "home", "path": "/data/home", "bytes": 0, "files": 0, "truncated": false}],
  "truncated": false
}
```

Category IDs are `models`, `library`, `downloads`, `engines`, `session_cache`,
`config`, `logs`, `legacy` and `other`. Each `ITEM` is
`{"id", "kind", "title", "path", "bytes", "reclaimable", "refusal", "message", "references"}`.
Item IDs are `file:<local inventory id>`, `partial:<job>`, `engine:<package>`,
`legacy:cache`, `legacy:prompt-state`, `legacy:config` and `model:<id>`.
`refusal` is one of `read_only`, `not_owned`, `in_use`, `busy`, `active`,
`publication_uncertain`, `install_running`, `kept` or `not_found`; `references`
names the models or downloads involved. Unreferenced files only come from stores
Juiced owns. Warning codes are `low_space`, `session_cache_over_budget` and
`acquisition_journal_unwritable`.

`POST /v1/juiced/storage/reclaim` (operator key) deletes items with the owning
store's rules. Preview first:

```json
{"items": ["legacy:cache", "file:/data/huggingface/hub/.../x.gguf"], "preview": true}
```

The preview returns `{"schema_version": 1, "preview": true, "applied": false,
"revision", "freed_bytes", "refused", "items": [ITEM]}` and changes nothing. Apply
with the preview's revision:

```json
{"items": ["legacy:cache", "file:/data/huggingface/hub/.../x.gguf"], "expected_revision": "9f3c0d2a41b7e655"}
```

The result is `{"schema_version": 1, "preview": false, "applied", "freed_bytes",
"items": [{"id", "kind", "title", "applied", "freed_bytes", "error"}]}`. Errors:
`storage_revision_required` (400, no revision), `storage_revision_conflict` (409,
storage changed since the preview), `storage_item_refused` (409, an item is still
in use; nothing is deleted) and `invalid_storage_reclaim` (400, 1-64 items).
`model:<id>` disables, unloads and unregisters the model like `POST /v1/delete`,
then deletes the weights only that model used. Read-only library mounts are never
written.

Downloads, engine installs and snapshot saves refuse with HTTP 507
`insufficient_storage` when the target volume would drop below
`storage.reserve_bytes`; the message gives the shortfall. Library pulls and
`POST /v1/pull` jobs are journaled in `/data/acquisitions.json`, survive a
restart and resume from committed bytes; `GET /v1/downloads` and
`GET /v1/juiced/library/pulls` list them again after the restart.

## Search and download models

`GET /v1/juiced/library/search` searches the catalog, files on this machine and
Hugging Face. Query parameters: `q` (at most 128 bytes), `task` (`chat`,
`embeddings`, `transcription`, `tts`, `image`, `audio-generation`, `3d`,
`classify`), `engine`, `limit` (1-20, default 10), `cursor` and `hub=false`
(skip Hugging Face). Without `engine`, the first page lists GGUF repositories and
original checkpoints; later pages follow the GGUF listing. `engine=vllm`,
`onnxruntime` or `thenoise` lists only checkpoints for that engine; any other engine
lists GGUF files. An unknown engine returns 400 `invalid_library_query`.
`repo=owner/name` looks up that one repository instead of searching, with the
quants on this machine attached. It can't be combined with `q` or `cursor`. A
repository Hugging Face doesn't have returns no Hugging Face result, and the
Hugging Face status stays `ok`.

Each result has `repo`, `license`, `gated`, `downloads` and `quants`. A checkpoint
quant looks like this:

```json
{"label": "Checkpoint", "kind": "checkpoint", "format": "transformers", "engine": "vllm",
 "file": "config.json", "bytes": 1519182365, "pullable": true,
 "files": [{"file": "config.json", "bytes": 726, "sha256": null, "member": "config.json"},
           {"file": "model.safetensors", "bytes": 1503300328, "sha256": "f47f…", "member": "model.safetensors"}],
 "local": {"present": false, "set": null, "storage": null}}
```

`kind` is `quant` (GGUF), `bundle` (a catalog model with companions) or
`checkpoint`; `format` is `transformers`, `onnx-classifier` or
`thenoise-diffusion`. `member` is the file name inside the downloaded folder.
`sha256` is null for small files Hugging Face keeps outside LFS until Juiced has
downloaded them. `sources.huggingface.authenticated` reports whether the server
has an `HF_TOKEN` for gated repositories.

`POST /v1/juiced/library/pull` downloads one variant:

```json
{"repo": "Qwen/Qwen3-0.6B", "file": "config.json", "expected_generation": 12}
```

`expected_generation` is the store generation from `GET /v1/juiced/artifacts` or
the search reply's `artifact_generation`; a stale one returns 409
`artifact_revision_conflict`. Download progress does not change the generation.

`file` names any file of the variant, or `files` lists several from one variant;
Juiced downloads the whole variant at the repository's current commit, or at
`revision`. Small files without a published SHA-256 are fetched first and checked
against their git blob id (`library_file_mismatch`, 502, when they differ). The
reply and `GET /v1/juiced/library/pulls/ID` return the pull with `state`
(`downloading`, `waiting`, `publishing`, `complete` or `failed`), `files`, `jobs`,
`set`, `suggested_id` and, for checkpoints, `format` and `engine`. Pause, resume or
cancel a pull through its `jobs` with `POST /v1/juiced/artifacts/jobs/ID`.

When the connection to the hub drops, the pull shows `waiting` and resumes the file
where it stopped, up to six times over about nine minutes, before it fails. A new
pull of the same files picks up the bytes of a download that failed this way.

Register a finished checkpoint with `POST /v1/juiced/models/add`:

```json
{"artifact_set": "set-…", "policy": "on-demand"}
```

The id defaults to the repository name for a checkpoint and to the catalog name
(`TRELLIS-3D-Q4`, `kokoro-v1`) for a catalog bundle; the pull reports it as
`suggested_id`. Pass `"engine": "thenoise"` for split
diffusion checkpoints that sd.cpp could also read. The engine reads the set as a
folder of its member names. Without the engine installed the reply is 409
`engine_not_installed` with the build to install; on a host the engine cannot run
on it is 409 `no_usable_backend`.

### Add or update when a download finishes

`POST /v1/juiced/library/pull` takes an optional `then` that registers the files
once the pull completes:

```json
{"repo": "unsloth/Qwen3-0.6B-GGUF", "file": "Qwen3-0.6B-Q4_K_M.gguf",
 "expected_generation": 12, "then": {"policy": "on-demand", "id": "Qwen3-0.6B-Q4_K_M"}}
```

`policy` is required; `id` and `engine` work as in `POST /v1/juiced/models/add`.
The intent is journaled with the pull, so it survives a page reload and a
restart. When the engine Juiced picks is not installed, the service installs that
build first. The pull view carries `then: {kind, id, policy, engine, state,
model, error, message}`; `state` is `pending`, `done` (with `model`) or `failed`
(with `error` and `message`, and the files stay on disk). A pull with a pending
`then` cannot be dismissed (409 `library_pull_active`). Joining a running pull of
the same files keeps the first `then`.

`POST /v1/models/{id}/update` with `{"expected_revision": 4}` downloads the
current upstream revision of a stored model's main file and then moves the model
to it. It returns the pull, whose `then` is `{kind: "update", model,
from_artifact, state, …}`. The model keeps its settings and policy; a loaded
worker stops and starts again on the new files, after its running requests
finish. On failure the model keeps its old files. The old files stay in storage
without a reference, so Storage lists them as unused. Errors: 409
`revision_conflict` (stale revision), 409 `update_unsupported` (the model's files
were not downloaded from Hugging Face by Juiced, or were combined into a set at
add time), 409 `update_check_offline` (hub unavailable) and, in the pull's `then`,
`model_files_changed` when the model's files changed during the download.

`POST /v1/juiced/flm/pull` takes `then: {"policy": "hot"}` the same way and adds
the checkpoint when FastFlowLM finishes. FastFlowLM pulls live in memory, so this
intent does not survive a restart.

### Download a file from a URL

`POST /v1/juiced/artifacts/pull` takes `{"download": {"source_url": "https://…"},
"expected_generation": 3}`. Only `source_url` is required.

- `expected_sha256` is optional. Without it the store keeps the SHA-256 it
  computes, the job reports `computed_digest: true`, and the stored file is
  listed with `unverified: true` in `GET /v1/juiced/library/local`. With it, a
  mismatch quarantines the file (`artifact_hash_mismatch`).
- `expected_bytes` defaults to the length the server reports, from `HEAD` or a
  one-byte range request. When the server reports neither, the transfer may use
  the free space above `storage.reserve_bytes` and fails with
  `artifact_size_mismatch` beyond it. With no free space above the reserve the
  request returns 507 `insufficient_storage`.
- `maximum_bytes` defaults to the expected size, or to that free-space ceiling.

## Workflow jobs and stored conversations

Juiced runs no caller-defined workflows and stores no conversations. These
routes answer under every native API prefix so clients can detect that:

| Request | Reply |
| --- | --- |
| `POST /v1/jobs`, `POST /v1/jobs/ID/resume` | HTTP 501 `{"error":"Juiced does not execute caller-defined workflows or resume legacy graphs"}` |
| `GET /v1/jobs` | `{"jobs":[]}` |
| `GET` or `DELETE /v1/jobs/ID` | HTTP 404 `{"error":"unknown job"}` |
| `POST /v1/jobs/ID/pause` or `/interrupt` | HTTP 404 |
| `GET /v1/juiced/threads`, `GET` or `POST /v1/juiced/thread` | HTTP 410 `{"error":"Juiced does not store conversations. Downstream clients own chat history."}` |

Writes and the thread routes need operator access.

## Pause new work for maintenance

```bash
curl --fail-with-body http://localhost:8080/v1/juiced/admission \
  -H 'Content-Type: application/json' \
  -d '{"paused":true}'
```

This operator control rejects new and pending work. It does not interrupt active
inference or change model policies. Wait for active work to settle and unload
idle workers before changing resource topology. Resume with `{"paused":false}`.

Execution and retained-artifact cleanup share the native manager's model identities
and publication rules. See [runtime profiles](https://github.com/Nulifyer/Juiced/blob/main/juiced/RUNTIME-PROFILES.md)
and [resource admission](https://github.com/Nulifyer/Juiced/blob/main/juiced/RESOURCE-ADMISSION.md) for the detailed
source contracts. Raw `/internal/*` configuration operations are operator controls.

Native routes register `/api/v0/`, `/api/v1/`, `/v0/`, and `/v1/` aliases.
Ollama, Anthropic, AUTOMATIC1111, and MCP retain their protocol-specific paths.
