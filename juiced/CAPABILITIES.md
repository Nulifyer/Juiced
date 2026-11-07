# Model service capabilities

`GET /v1/juiced/capabilities` is Juiced's optional native discovery contract,
schema version 1. It is registered under `/api/v0/`, `/api/v1/`, `/v0/` and `/v1/`.
`/.well-known/juiced.json` points to it. Strict OpenAI/Ollama/Anthropic clients
continue using their normal model and inference endpoints.

The response contains enabled downloaded model contracts, the existing protocol
subset manifest, product-scope declarations and request-identity behavior. It
contains no credential values. The operator catalogue includes the same
`capability_contract` for inspection of disabled or unavailable registrations.
Model names, policy, options and loaded workers still belong to the native manager.
This endpoint does not install, probe-load, enable or reconfigure a model.

## Version 1 fields

| Field | Meaning |
| --- | --- |
| `schema_version` | Native contract shape, currently 1 |
| `server_version` | Compiled native manager version |
| `product_version`, `source_revision` | Container build identity when supplied; null otherwise |
| `revision` | Existing native state revision, not an immutable weight hash |
| `models[].model` | Proper public model ID |
| `models[].policy` | Current enabled residency policy |
| `artifact.architecture` | Inspected GGUF architecture when known, otherwise null |
| `artifact.downloaded` | Native acquisition state |
| `artifact.components_complete` | Required/resolved paths exist when inspected; null means unknown. Existence does not prove correct weights or every optional feature |
| `artifact.missing_components` | Missing resolved/required component roles, without publishing absolute paths |
| `artifact.source_claims` | Native model labels; publisher/model metadata is not qualification |
| `execution.recipe`, `requested_backend`, `profile` | Selected execution identity; an unresolved backend remains null |
| `execution.settings_scope`, `settings_pending` | Selected profile metadata versus pending loaded-worker changes |
| `execution.compatibility` | Existing backend importer result when evaluated, otherwise compatible=null and a reason |
| `execution.worker_loaded` | Observed worker residency, not output-quality acceptance |
| `execution.hardware_qualification`, `performance_qualification` | `not_recorded` until matching operational receipts have a supported owner. Do not infer qualification from loaded status |
| `operations` | Selected native model mode intersected with its provider's declared modes |
| `operations[].support`, `source` | `declared` by `native_backend_descriptor`; not proof of all request features |
| `operations[].streaming` | null until an exact operation/profile contract supplies it |
| `operations[].conformance` | `not_recorded`; the protocol manifest separately describes implemented subsets |
| `limits.context_tokens` | Native model/config context metadata when known |
| `limits.context_scope` | `model_or_configured_total_budget`; do not assume it is available independently to each chat |
| `limits.concurrent_requests`, `per_request_context_tokens` | null where the native rich contract has not yet resolved exact limits |
| `features` | Declared model tools/vision/reasoning labels or null, not a model behavior guarantee |
| `scope` | Agent execution and user-authored workflows are false; interaction is temporary service testing |

Unknown values remain null or explicitly unrecorded. Clients must inspect enabled
policy, components and compatibility before interpreting a declaration. Runtime
selection still uses `/v1/juiced/execution`; it distinguishes import compatibility,
package readiness and host availability. Neither endpoint promises universal
GGUF conversion or NPU acceleration.

The operator's Effective pane shows declared model services with contextual evidence.
The Service APIs screen links discovery without presenting it as an OpenAI standard.
No saved-chat interface or agent tool runner is added.

## Runtime-only enforcement

Juiced does not promote, discover or execute `collection.omni`/`lemonade_omni`
planner loops. Existing registrations and weights remain inspectable as unavailable.
They cannot be enabled through loading policy. The MCP model-service gateway exposes
individual inference operations and filters model selection through the same policy.
It does not auto-enable/download a default in response to a model-service request.

Generic native job creation and legacy graph resume return 501 in Juiced. Existing
job records remain inspectable by an operator, deletable through the existing
lifecycle and interrupted after restart. Nested `definition.steps`, top-level `steps`
and alternative graph payloads do not run. The download and preparation controllers
remain separate fixed runtime operations. Additional fixed media jobs require a
future qualified operation contract; this release does not supply a generic runner.

## Credentials and retained state

HTTP authentication and persistent Chat Completions state use the same native
credential parser. Bearer is case-insensitive; `x-api-key` is also accepted. Bearer
has precedence when both headers are present. A malformed Authorization scheme is
not a valid API credential. Model/session/client headers cannot establish authority.

With distinct configured keys, inference and operator credentials resolve to separate
principals. The existing fallback where the admin key equals the regular key retains
its full-access behavior. Native control operations, model-option mutations, installation and job
inspection/mutation require the operator key when configured. Raw native slot
inspection/actions and cloud-credential configuration are operator controls too.
Reading model options
remains available to an inference credential for consumer context discovery. Existing key-free
trusted-LAN installations remain key-free; this is a shared trust domain.

Dummy keys in anonymous mode do not create private users. Anonymous persistent
snapshots share authority within that trusted-LAN domain, partitioned by session for
reuse rather than authorization. Session IDs are not access control.

Snapshot identity `principal-v1` binds normalized principal, configured credential
epoch, Chat Completions protocol, session, execution fingerprint and relevant request
semantics. Rotating a configured key or changing auth mode causes a cold rebuild.
Other protocols retain their existing persistent-state restrictions. Old snapshots
remain on disk under their original identities and are not silently reassigned.
The first request after upgrading from the old raw-header namespace may be cold.
No transcript/chat store or automatic cross-protocol session mapping is introduced.

Per-client key management, broader protocol cache qualification and resource
reservations are separate planned phases. This contract does not claim those are
implemented. See the approved staged [platform plan](PLATFORM-PLAN.md).
