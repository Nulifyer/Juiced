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

`/.well-known/juiced.json` points clients to native discovery.

## Discover models for a client

A client that registers models needs one request: `GET /v1/juiced/catalog`
(read key). Check `discovery_schema_version` (currently `1`) before parsing the
fields below; `/.well-known/juiced.json` and `GET /v1/juiced/capabilities`
carry the same number. Fields not listed here are operator details and may change.

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
| `thinking.supported` | `true`, `false` or `null` (unknown) |
| `thinking.request_control` | The per-request switch the template reads, usually `chat_template_kwargs.enable_thinking` |
| `thinking.default` | `on` or `off` for requests that do not set the switch |
| `tool_calls.supported` | `true`, `false` or `null` |
| `thinking.source`, `tool_calls.source` | Evidence: `engine_template_caps` or `engine_chat_template` (the loaded engine), `gguf_chat_template` (the weights' chat template), `launch_chat_template`, `launch_arguments` or `operations`; `reason` explains a `null` |
| `labels` | Native labels derived from the same evidence: `chat`, `embeddings`, `transcription`, `tts`, `image`, `vision`, `reasoning`, `tool-calling` |
| `capability_contract` | The native contract entry, including `limits.per_request_context_tokens` and `features` |

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
| `GET /v1/juiced/requests` | Bounded request metadata for diagnostics |
| `GET /v1/health` | Liveness plus `version`, `model_loaded`, `all_models_loaded` (one entry per running worker with `model_name`, `type`, `recipe`, `status`, `pid`, `context_length`), `max_models` and `pinned_models` per model type |

Each `GET /v1/juiced/requests` record carries `id`, `model`, `operation`,
`outcome`, `status`, `elapsed_ms`, `queue_wait_ms`, `completed_at_ms` (Unix
milliseconds), and the engine-reported `input_tokens`, `output_tokens`,
`prompt_tokens_per_second` and `decode_tokens_per_second`; unknown values are
`null`. Engines that report prompt reuse add `cached_tokens` and
`prompt_processed_tokens`, and prompt-cache requests add `session_cache` with its
`path` (`slot-hit`, `restored`, `cold` or `bypass`), `slot`, and `restore_ms` when
restored. Records are newest first; the service keeps the latest 128.

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
