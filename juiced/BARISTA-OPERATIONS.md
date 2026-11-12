# Barista native-operation inventory

B0 implementation inventory, 2026-11-10. This maps the existing native surface
and the missing work required by [BARISTA.md](BARISTA.md). It does not enable
Barista or define an executable tool dispatcher. The platform plan and Barista
contract remain normative.

Source inspection started from `3791a27f`, including the pending snapshot-storage
binding changes. The integration review refreshes committed native references
through `62e82570`, including saved-profile suitability. Concurrent hierarchy and
root-alias changes require their separate acceptance. A native route's presence
does not establish assistant authority,
complete termination, useful service, physical fit or model qualification.

## Reading the inventory

Paths beginning `/v1/` use the canonical management prefix. The native registration
also provides `/v0/`, `/api/v0/` and `/api/v1/` aliases. Fixed `/internal/`, `/live`,
`/metrics` and well-known paths do not acquire those aliases. Barista should use
the canonical native owner rather than choose a different protocol to bypass a
failed policy, authorization, resource or lifecycle check.

`Native` means the operation has an existing native entry point. Every such row
still requires the assistant adapter, permission checks and qualification below.
`Missing` means an implementation obligation, not permission to emulate the
operation with shell commands, arbitrary HTTP, file edits or generated code.
`Retained` means historical observation or cleanup only, with no new producer or
arbitrary-job execution.

The operation IDs below identify requirements. They are not registered model
tools. Build the eventual schemas from the native operation's validated fields,
not a general `path`, `method`, `body`, `command` or filesystem tool.

## Inspect

| ID | Native entry point | Owner and current boundary |
| --- | --- | --- |
| inspect.health | `GET /live`, `GET /v1/health` | Native: `Server::handle_live`, `handle_health`. Distinguish manager liveness from model readiness and useful responses. |
| inspect.installation | `GET /.well-known/juiced.json`, `GET /v1/juiced/state` | Native: `JuicedService::routes`, `handle`. Version, API locations, state revision and operation status. A stable installation ID for invitation persistence is still missing. |
| inspect.catalog | `GET /v1/juiced/catalog` | Native: `JuicedService::handle`, ModelManager. Canonical catalog, policy, operations and catalog revision, including unavailable models. |
| inspect.discovery | `GET /v1/models`, `GET /v1/models/{model}`, `GET /v1/juiced/capabilities`, `GET /v1/juiced/protocols` | Native: Server, ModelManager, BackendOps and Juiced capability/protocol projections. Enabled discovery and complete operator catalog have different purposes. Preserve declared/configured/applied/observed context and unknown values. |
| inspect.files | `GET /v1/models/{model}/files` | Native: `Server::handle_model_files`. Published file and companion inspection. Do not infer immutable identity or complete companions merely from a path. |
| inspect.options | `GET /v1/models/{model}/options` | Native: `Server::handle_model_options_get`, ModelManager's coherent options snapshot. Keep saved overrides, effective values and defaults distinct. |
| inspect.execution | `GET /v1/juiced/execution?model=MODEL`, `GET /v1/juiced/devices` | Native: `JuicedService::execution_api`, `execution_devices`, native backend descriptors. Compatibility, selected profile and observed devices are not useful-service or performance qualification. |
| inspect.resources | `GET /v1/system-info`, `GET /v1/system-stats`, `GET /v1/juiced/admission`, `GET /v1/juiced/suitability?model=MODEL` | Native: Server, native resource observations and ResourceAdmission. Suitability compares operator-declared ceilings for saved checkpoint/options without reserving resources. It separates nominal capacity, current pressure, unknown inputs and policy blockers. It does not establish measured peaks, immutable artifact/device identity or a qualified recommendation. Full host-fit estimation, UMA accounting and fair actual-slot acceptance remain P1 obligations. |
| inspect.activity | `GET /v1/stats`, `GET /metrics`, `GET /v1/juiced/requests` | Native: Server metrics and bounded Juiced request records. Select relevant fields and a bounded interval. Do not dump all metrics or diagnostic data into model context. |
| inspect.cache | `GET /v1/juiced/cache`, `POST /v1/juiced/cache/profile` | Native: `JuicedService::cache_api`, exact worker state-operation lease. Profile inspection uses POST but does not grant qualification. Report pending writes, charges and cleanup separately. |
| inspect.slots | `GET /v1/slots` | Native: `Server::handle_slots`, selected llama.cpp worker. Observation does not authorize manual slot mutation or discharge a pending writer. |
| inspect.transfers | `GET /v1/downloads`, `GET /v1/juiced/imports`, `GET /v1/juiced/transfers` | Native: ModelManager and Juiced import/transfer owners. Read persisted state and runtime progress together where supported. A transfer record alone does not establish a running writer. |
| inspect.sync | `GET /internal/models/sync/status` | Native: `Server::handle_models_sync_status`, ModelManager generation-specific sync results. |
| inspect.configuration | `GET /internal/config`, `GET /internal/config/defaults`, `GET /internal/aliases` | Native: RuntimeConfig and ModelManager alias owners. A bounded, redacted assistant projection is required before exposing configuration. |
| inspect.voices | `GET /v1/audio/voices?model=MODEL` | Native: Juiced voice discovery for its supported loaded backend. Do not report generic speech compatibility from one backend's voice list. |
| inspect.legacy | `GET /v1/juiced/preparation`, `GET /v1/jobs`, `GET /v1/jobs/{id}` | Retained: historical preparation output/artifacts and JobManager metadata. No new conversion, graph execution or automatic resume. |

## Configure and operate models

| ID | Native entry point | Owner and current boundary |
| --- | --- | --- |
| configure.options_preview | `POST /v1/models/{model}/options` with `dry_run` | Native: Server validation and ModelManager snapshot preview. Pass normalized clear semantics through unchanged. Preview does not establish resource fit or permission to reload. |
| configure.options_set | `POST /v1/models/{model}/options` | Native: Server and ModelManager serialized options writer. Preserve normal model settings, validated recipe and saved/applied distinction. Add the assistant's target-revision checks without another options store. |
| configure.options_clear | `DELETE /v1/models/{model}/options` | Native: Server and ModelManager. Remove the chosen overrides and verify effective defaults afterward. Do not invent assistant-specific defaults. |
| configure.policy | `POST /v1/juiced/policy` | Native: Juiced durable policy/revision owner and Router residency projection. Use the observed revision. Hot, On-demand and Disabled remain authoritative. |
| configure.profile | `POST /v1/juiced/execution` | Native: execution selection through ModelManager and Juiced's queued operation owner. Preserve `expected_selected`, compatibility acknowledgement, normal options and required reload. |
| operate.residency | `POST /v1/juiced/operation` | Native: fixed `load`, `unload` and `reload` actions through Juiced and Router. Accepted status is not completion. Verify the current generation and pending cleanup. |
| operate.legacy_residency | `POST /v1/load`, `POST /v1/unload`, `POST /internal/pin` | Native compatibility entry points. The assistant should prefer the Juiced policy/operation owner. These paths are not a way around Hot intent or unresolved retirement. |
| configure.server | `POST /internal/set` | Native: RuntimeConfig publication and Server's effects owner. Verify saved intent, applied settings and pending/failed effects separately. The assistant schema must select explicit supported fields. Runtime executable and filesystem-root changes require operator-authorized native handles as well as existing ownership gates. Raw model-generated paths or environment maps are not assistant configuration inputs. |
| configure.alias_add | `POST /internal/aliases` | Native: ModelManager alias publication and namespace checks. Bind the intended target and verify resulting public resolution. |
| configure.alias_remove | `DELETE /internal/aliases/{alias}` | Native: ModelManager alias removal. Preserve model registration, files and unrelated aliases. |
| configure.routing_validate | `POST /v1/routing/validate` | Native: routing validation. Validation is not persisted routing policy, provider qualification or cloud consent. Further routing changes belong to the existing P5 owner. |
| operate.self_reload | No complete assistant operation yet | Missing: release Barista's inference lease before a tool mutates its own selected worker. Resume inference against the committed model/profile/generation, with no silent model substitution. |

## Find and acquire published variants

| ID | Native entry point | Owner and current boundary |
| --- | --- | --- |
| acquire.search | `GET /v1/registry/search` | Native: `Server::handle_registry_search`, provider metadata through existing acquisition owners. Respect offline mode and source provenance. A search result does not establish host fit or that it is the newest applicable model. |
| acquire.variants | `GET /v1/pull/variants` | Native: `Server::handle_pull_variants`. Inspect repository files, exact quant/variant and companions before presenting an acquisition choice. |
| acquire.download | `POST /v1/pull` | Native: Server, ModelManager and provider download owners. Retain native job handles, resource admission, companion decisions and originals. Completion is distinct from registration, enablement, loading and useful service. |
| acquire.download_control | `POST /v1/downloads/control` | Native: `Server::handle_download_control`, ModelManager. Use only native supported actions and identifiers. Cancellation acknowledgement does not prove terminal filesystem effects. |
| acquire.upload_start | `POST /v1/juiced/import/start` | Native: Juiced import journal and management owner. Bind the accepted upload identity and storage ownership. |
| acquire.upload_chunk | `POST /v1/juiced/import/chunk` | Native: existing binary chunk protocol and offset validation. Keep 1-byte-to-8-MiB chunks. The assistant receives handles/progress, not model-weight bytes in its prompt. |
| acquire.upload_finish | `POST /v1/juiced/import/finish` | Native: recoverable Juiced publication and ModelManager namespace/provenance authority. Preserve normalized replay semantics and unresolved publication ownership. |
| acquire.upload_cancel | `POST /v1/juiced/import/cancel` | Native: serialized Juiced import cancellation. Do not report cancellation when publication already owns the artifact. |
| acquire.server_import | `POST /v1/juiced/import/server` | Native: existing server-side import owner. The assistant must use an operator-authorized import handle or existing declared source, not gain arbitrary host filesystem browsing. The bounded handle adapter is missing. |
| acquire.register | `POST /v1/models/register` | Native: ModelManager definition/namespace/reference ownership. Validate the exact model definition and referenced artifacts. Do not expose unrestricted executable paths through an assistant tool. |
| acquire.check_updates | `POST /v1/models/check-updates` | Native: ModelManager's current update observation. An available revision is not permission to overwrite a referenced artifact or proof of compatible rollback. |
| acquire.sync | `POST /internal/models/sync` | Native: ModelManager sync operation, resource/writer ownership and generation-specific result. Keep explicit fetch/offline settings and native cancellation/recovery limits. |
| acquire.best_fit | No complete native recommendation receipt yet | Missing: versioned host-fit assessment of published candidates using actual capacity, configured context, incremental state/workspaces, companions and current reservations. Unknown fit stays unknown. |

## Maintain and recover

| ID | Native entry point | Owner and current boundary |
| --- | --- | --- |
| maintain.removal_preview | `GET /v1/juiced/removal?model=MODEL` | Native: Juiced removal plan and ModelManager complete-definition/reference checks. Revalidate through the mutation owner when executing. |
| maintain.remove | `POST /v1/juiced/removal` | Native: Juiced and ModelManager artifact/removal ownership. Preserve shared artifacts, expected definitions, pending import ownership and provider-managed checkpoint references. |
| maintain.legacy_delete | `POST /v1/delete` | Native compatibility entry point. Prefer removal preview/execution. Do not bypass the same import/reference/helper-result gates. |
| maintain.package_preview | `POST /v1/install/dry-run` | Native: native backend package planning. A plan does not qualify the package or release current worker references. |
| maintain.package_install | `POST /v1/install` | Native: BackendManager/provider package owner and ResourceAdmission. Retain mutation exclusion, referenced runtime trees and failed-stop recovery. Complete immutable bundle lifecycle remains P3. |
| maintain.package_remove | `POST /v1/uninstall` | Native: BackendManager and helper/worker reference owner. Do not reclaim a runtime with unresolved ownership. |
| maintain.cache_policy | `POST /v1/juiced/cache` | Native: Juiced cache policy, explicit model overrides and native state-operation exclusion. Ordinary session count is not a reason to add default per-user/model quotas. |
| maintain.snapshot_retain | `POST /v1/juiced/cache/retain` | Native: cache metadata owner. Retention changes do not establish write termination. |
| maintain.snapshot_forget | `POST /v1/juiced/cache/forget` | Native: committed removal/tombstones and retryable cleanup. Report retained charges and cleanup failure accurately. |
| maintain.snapshot_recover | `POST /v1/juiced/cache/recover` | Native: existing reconciliation of already owned writes, with no paths/overrides. It does not migrate snapshot roots or release unknown prior-manager writers. |
| maintain.cache_qualify | `POST /v1/juiced/cache/qualify` | Native qualification publication exists. The assistant must not invent acceptance evidence or raise write bounds after an observed violation. A qualified runner and receipt verification are missing. |
| maintain.model_cache | `POST /internal/cleanup-cache` | Native: ModelManager's exact cleanup plan/reference owner. Use its preview and destructive modes, bounded management work and retained references. Do not substitute recursive filesystem deletion. |
| maintain.preparation_observe | `GET /v1/juiced/preparation?id=ID`, `GET /v1/juiced/preparation?cache=1` | Retained: legacy records, bounded output and prepared-artifact references. No conversion planning, creation or resumption. |
| maintain.preparation_cleanup | `POST /v1/juiced/preparation`, actions `cancel` and `delete_cache` | Retained: original process/ticket/reference cleanup owners. Preserve uncertainty and shared prepared weights. This is not a producer fallback. |
| maintain.history_remove | `DELETE /v1/jobs/{id}` | Retained: JobManager historical metadata deletion. It does not stop processes, discharge resources or execute a graph. |
| maintain.admission_pause | `POST /v1/juiced/admission` | Native: ResourceAdmission's shared acquisition pause. A pause is not all-writer maintenance or a consistent backup boundary. |
| maintain.pressure_diagnostic | `POST /internal/simulate-vram-pressure` | Native diagnostic injection exists. Expose it only through a bounded, qualified diagnostic procedure that preserves essential services. Observing real pressure must not silently trigger this operation. |
| maintain.telemetry_flush | `POST /internal/telemetry/flush` | Native telemetry operation exists. Preserve configured telemetry policy and explicit export consent. Do not use it as a substitute for bounded local diagnostics. |
| maintain.shutdown | `POST /internal/shutdown` | Native: Server shutdown acknowledgement and owned teardown. Confirmed listener closure or manager exit alone does not prove descendants, device kernels or remote computation stopped. |
| maintain.storage_migrate | No validated native migration operation yet | Missing: bind legacy/new roots explicitly without reinterpreting old journals; preserve files, charges, referenced snapshots and retry/restart ownership. Never edit state paths to force startup. |
| maintain.prior_manager_recover | No complete operator recovery operation yet | Missing: reconcile prior-manager unsealed writes/helpers using evidence of the actual owner and termination. Reused generation numbers, idle inference and missing request handlers are insufficient. |
| maintain.consistent_backup | No complete all-writer snapshot operation yet | Missing: reject new mutations and drain all accepted operations through terminal filesystem effects across the twelve writer families and external roots. Existing leases/private JSON publication are prerequisites. |
| maintain.credentials | Existing `POST /v1/cloud/auth`, `DELETE /v1/cloud/auth/{provider}` require a separate adapter | Missing for assistant use: operator-owned credential handles and redacted results. Never accept a raw API key as model-generated tool input or return it to the model. Native operator setup remains available. |

## Compare, verify and explain

| ID | Native entry point | Owner and current boundary |
| --- | --- | --- |
| compare.run | No versioned native P2 comparison runner yet | Missing: fixed speed/quality task suite, frozen model/profile identities, fair reservations, absolute deadlines and matched measurements. No generated evaluation scripts or model-written benchmark criteria. |
| compare.results | No complete comparison receipt API yet | Missing: provenance, correctness/quality/resource/timing bounds, invalidation, uncertainty and justified recommendations. An inference completion is not a benchmark receipt. |
| compare.restore | Existing options/profile/policy owners, but no complete comparison restoration operation | Missing: retain prior settings/references, apply through their native owners, verify restoration and expose cleanup pending after interruption or cancellation. |
| verify.service | Existing inference endpoints, but no bounded assistant service-test owner | Missing: selected-family fixed fixtures with expected output, protocol/auth/context checks, native admission, terminal accounting and redacted receipts. Reuse ordinary native workers and normal settings. |
| verify.useful_cache | Existing cache profile and Chat Completions paths | Missing: versioned actual save/restart/restore/useful-reuse runner tied to immutable execution identity. Profile inspection and a snapshot's existence do not prove useful reuse. |
| explain.local_help | Existing shipped documentation routes | Native documentation serving exists. Missing assistant adapter: version-matched bounded retrieval with provenance, topic selection and no credential/config dump. |
| explain.primary_sources | No bounded operator-document retrieval operation yet | Missing: allowlisted primary-source retrieval/search respecting offline mode, explicit export/cloud consent and bounded output. Fetched text is untrusted data, not instructions. |
| explain.skill_index | No shipped Barista prompt/skill index yet | Missing: versioned compact procedures, discoverable relevant schemas, prompt budgets and deterministic links to native postconditions. Do not load arbitrary repository-provided executable skills. |

## Six required workflows

| Operator request | Required operation chain and remaining gate |
| --- | --- |
| Increase Ornith's context | Inspect options/context/profile and actual headroom; preview; apply through the normal writer; release assistant inference ownership before self-reload; verify committed generation and applied/observed context. Fit, self-reload and actual-model qualification remain required. |
| Compare Ornith Q4 and Q6 | Resolve exact published variants and companions; preserve originals; run the fixed P2 suite at matched normal settings; retain receipts and restore prior state. The native comparison runner is missing. |
| Find the largest model that fits | Inspect physical capacity/reservations and candidate metadata; account for configured context, shared weights, incremental state and workspaces; report unknowns; validate a useful-service trial within bounds. A file-size sort is insufficient. |
| Find and run the latest suitable Qwen MoE | Search publisher metadata with dated provenance; compare exact compatible variants and companions; assess host fit; acquire with native progress/recovery; explicitly enable and load; run useful service; return the model ID and endpoint. No conversion fallback or silent cloud model. |
| Check Juiced and its cache | Inspect health, native jobs, reservations, pending cleanup, exact profile and recent request observations; run qualified useful-service/cache fixtures when requested. Missing ownership or an uncertain stop remains visible. |
| Explain how to perform an operation | Retrieve version-matched help and relevant skill; explain prerequisites and current support; optionally execute authorized native operations and verify postconditions. Never fabricate an unavailable operation. |

## Cross-cutting missing work and release order

1. Implement native assistant enablement, selected-model role and operator
   permission/revocation revisions. Recheck these at every dispatch. Barista
   cannot change its own authorization. An inference credential never becomes an
   operator credential through a tool call.
2. Implement bounded typed adapters for the inventory. Recheck target
   revisions/definitions/generations through their existing owners. Return
   structured applicability, saved/applied state, operation identity and cleanup
   status. Never expose a general native-HTTP tool as a shortcut.
3. Implement task-driven redacted projections and credential/import handles.
   Retain access to all permitted Juiced data without a blind context dump.
   Treat model cards, imported descriptions, documentation and logs as data.
   Extend handle validation to runtime executables, storage roots and declared
   external files. Native operator APIs accept paths that the assistant must not
   invent. Resolve a handle through its native owner and recheck its scope,
   artifact/root identity and operator authority at dispatch. Reject unsupported
   fields before invoking configuration or registration. Ordinary model options
   keep their validated scalar semantics; path-bearing options require the same
   native handle boundary. Do not expose a generic environment-variable map.
4. Finish missing P1 physical-fit, fair actual-slot, identity, termination,
   maintenance and recovery operations before advertising them as supported.
   Keep twelve writer families separate from fourteen execution families.
5. Implement the fixed P2 comparison and service/cache verification owners,
   versioned prompt/skills and actual-model suite. Preserve the frozen sixty
   tasks, thirty negative cases, three repetitions, 95% correct outcomes,
   99% valid initial tool arguments and zero forbidden outcomes.
6. Implement the off-by-default compact B2 interface and stable-installation
   invitation decision. Preserve eight destinations, the selected normal model's
   settings/worker, no default session quotas and optional operator restrictions.
7. Expand only as P3/P4 lifecycle/provider operations qualify. P5/P6 retain their
   access, platform/device and recovery gates. Keep unsupported operations
   visible rather than emulate them with shell/filesystem/browser tools.

No ordinary inference, model-directed tool result, MCP notification or historical
job is allowed to activate this loop. New conversion/quantization/repacking/export,
arbitrary graph creation/resume, saved consumer chats, host administration and
self-granted assistant authority remain excluded.

Keep `POST /v1/slots/{id}` behind the qualified cache owner rather than exposing
raw snapshot paths. The retained `POST /v1/jobs` and job pause/interrupt/resume
paths do not authorize arbitrary work. Juiced's thread/history stubs do not
authorize saved consumer conversations. Protocol inference routes and MCP tools
do not authorize an operator loop. UI assets, raw log streams and server-sent
events are transports, not substitutes for bounded typed operation results.

## Source owners

- [Server route and native handlers](../src/cpp/server/server.cpp), including
  fixed internal routes and native-prefix registration.
- [Juiced routes, policy, operation and imports](../src/cpp/server/juiced/service.cpp).
- [Execution selection and device observations](../src/cpp/server/juiced/execution.cpp).
- [Cache publication, qualification and recovery](../src/cpp/server/juiced/cache.cpp).
- [Capabilities](../src/cpp/server/juiced/capabilities.cpp) and
  [protocol/voice compatibility](../src/cpp/server/juiced/compatibility.cpp).
- [Removal](../src/cpp/server/juiced/removal.cpp) and
  [retained preparation cleanup](../src/cpp/server/juiced/preparation.cpp).
- [ModelManager](../src/cpp/server/model_manager.cpp),
  [RuntimeConfig](../src/cpp/server/runtime_config.cpp),
  [Router](../src/cpp/server/router.cpp) and
  [ResourceAdmission](../src/cpp/server/resource_admission.cpp).

Reinspect the affected owner and update this inventory when native operations
change. Keep operation implementations, permissions, resources and lifecycle
ownership in those owners. This document must not become a second model catalog,
executor, scheduler or authority store.
