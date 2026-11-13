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

The operator catalog includes disabled registrations. Consumer discovery at
`/v1/models` includes only enabled available models. Native capabilities report
declared operations, component status, selected execution, and unknowns.

`/.well-known/juiced.json` points clients to native discovery.
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

## Retained workflow history

`GET /v1/jobs` and `GET /v1/jobs/ID` inspect historical workflow records.
Queued or running history appears as interrupted; Juiced never executes it.
Creating, resuming, pausing, or interrupting arbitrary workflows returns HTTP501.
These endpoints also use the other native API prefixes and require operator access.

`DELETE /v1/jobs/ID` deletes only the selected historical metadata and returns
`metadata_only: true`. It does not stop workers or remove models, artifacts,
or resource charges. Failed storage publication preserves the committed record.
The list response includes `recovery`: unresolved prior cleanup is quarantined,
with up to 50 record IDs and its total count. Ordinary deletion returns HTTP409
for those records. Malformed or unsupported storage also blocks deletion and
preserves the file. Recovery of an uncertain prior writer requires its own
termination evidence; removing history cannot establish that it stopped.

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
