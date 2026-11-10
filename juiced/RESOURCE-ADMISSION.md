# Resource admission

This native source batch adds admission under Router ownership. It is not yet a
qualified essential-service protection release. The default `legacy` mode retains
existing operator-approved profiles and reports `latency_qualified: false`.

## Retired model producers

Juiced no longer creates converted, quantized, repacked or exported model weights.
Producer planning/creation/resume rejects HTTP410 `feature_retired` before resource
admission or filesystem/process dispatch. The image excludes the producer Python
environment, conversion scripts and quantizer executable. Library finds existing
published variants; normal runtime arithmetic/KV precision remains supported.

Existing preparations are retained recovery records. Their bounded inspection,
cancellation and reference-safe artifact cleanup remain available. Unconfirmed
writers retain process/ticket/staging ownership and block unsafe source deletion.
Prior-manager uncertainty remains quarantined, never auto-resumed or called stopped.
Historical preparation assertions below describe those retained ownership paths;
they do not authorize new model production. Complete operator recovery and all-writer
maintenance remain separate unfinished P1 requirements. The cleanup-only completion
owner and its stop-failure fixture remain until legacy retirement/recovery closes.
No public API or worker queue can dispatch a model producer through that owner.

## Discovery context observations

Native model list/detail and catalog entries include `context.schema_version: 1`.
Router derives this projection from one ModelInfo and one worker-generation
snapshot. It separates declared model/provider context, resolved configured
context, the applied launch setting and observed per-request context/slot count.
Unknown values are null. Automatic settings remain automatic, rather than
advertising the `-1` sentinel as a capacity. Speech/image services do not inherit
a generic text-context claim.

llama.cpp reads its bounded, read-only `/props` response once during owned startup,
outside Router's lifecycle mutex. The probe has a two-second timeout and a 1-MiB
response bound. Its allowlisted observation includes source, timestamp and build
when available. Discovery performs no network I/O. Missing or invalid properties
leave observed capacity unknown. Reload/replacement binds a new observation to
the new worker; failed cleanup never advertises a usable serving context.

`context_length` prefers observed per-request capacity, then applied/configured
settings, then declared model/provider metadata. `context.advertised_source`
identifies that choice. A requested total budget can differ from the engine's
actual per-request limit. The UI uses observed context directly and never divides
launch settings to manufacture a serving limit. Native capabilities retain unknown
qualified concurrency even when a physical slot count is observed.

OpenAI-compatible discovery, Ollama show and MCP available-model discovery share
this native projection. Ollama omits unknown context/parameter/file-type values
instead of fabricating zero metadata. The rich context object is a Juiced
extension. It does not establish model fit, successful concurrent serving,
useful cache reuse or device/quality qualification. Other backends retain explicit
unknown observations until their native reporting contracts are implemented.

## Physical execution slots

llama.cpp also reads `/slots` during owned startup with the same two-second and
1-MiB bounds. Unique nonnegative IDs become a generation-bound inventory; an
available `/props` count must agree. IDs are not inferred from a configured thread
limit. Router uses this inventory for native text requests and manual slot mutation.

The existing ResourceAdmission queue owns physical claims, FIFO eligibility,
priorities, shared bounds, cancellation and deadlines. The trusted native request
ID permits an inference ticket to share its own cache-state ticket's slot. Parallel
child inference cannot share that slot, nor can another principal or generation.
State and request byte charges remain independent. The public `physical_slots`
projection counts unique physical claims rather than logical tickets.

Cache operations retain policy/session ownership separately from physical slots.
There is no second slot queue or active-slot registry. Deferred providers restore
native identity and deadlines. A settled stream finalizes its snapshot and releases
its state lease before publishing the terminal event. Unresolved work retains its
claim; idle inference still cannot discharge an uncertain state writer.

Protected or leased paths reject an unknown inventory. Unprotected legacy requests
without one retain backend behavior and remain unqualified for native slot control.
No launch argument, context setting, principal quota or model quota is invented by
slot admission. A one-slot worker can queue many conversations; an explicit
multi-slot profile must provide its intended per-request context and resource budget.
Functional one/two-slot serving does not establish automatic parallel tuning,
physical/UMA peak bounds, accelerator termination or essential-service latency.

## Admission controls

`GET /v1/juiced/admission` reports pending work, active reservations, resident worker
allocations, uncertain termination, and each declared physical pool. The same route
exists under `/v0/`, `/api/v0/`, and `/api/v1/`. `POST` accepts only
`{"paused": true}` or `{"paused": false}`. These are operator controls. Pausing
rejects pending and new work without changing model policies or interrupting active
inference. Unload idle workers separately when draining for a configuration change.

The native `admission` setting is a complete value in `config.json`. Update it alone
through `POST /internal/set`. Changing resource topology requires no active,
resident, or pending claims. Use pause, wait for requests to settle, unload workers,
apply settings, then resume. Updates are staged and persisted before publication. A revision-bound transition blocks new admission during publication. Cloud metadata mutations stage a candidate under the registry owner, use the serialized configuration publication owner, and become visible only after durable commit. Failed install, opt-in or uninstall leaves metadata and process keys unchanged. HTTP cloud mutations serialize discovery and credential effects. Credentials remain process/environment owned. Configuration commits use atomic platform replacement, without overwrite-copy fallback or throwing post-commit cleanup. Write/replacement failure retains the old configuration and returns an error. Hot policies remain saved. Runtime pause is temporary
and does not survive manager restart.

Settings include `mode`, `active_limit`, `pending_limit`, `service_pending_limit`,
`principal_pending_limit`, `essential_wait_ms`, `interactive_wait_ms`, `batch_wait_ms`,
`pools`, and `models`. Pending defaults are 64 globally and 16 per service. There
is no default per-principal pending quota or per-model concurrency quota.
`principal_pending_limit` and each model's `concurrency` accept a positive integer
for an explicit operator cap. Omit the field or set it to `null` to leave that cap
unset. Explicit existing caps remain enforced; there is no migration that removes
them. Zero, negative, fractional and oversized values reject.

The admission snapshot advertises `optional_limits.schema_version: 1` with the
effective optional caps. Clients must inspect this support before sending null
to an older server. Global active/pending bounds, per-service pending bounds,
physical reservations, deadlines and fairness still apply when quotas are unset.
For observed llama.cpp inventories, Router joins actual inference and cache-state
slots to this queue. Functional slot tests remain separate from full resource and
essential-service qualification.

Queue wait defaults are 2, 10 and 30 seconds respectively. These bound queue
waiting; model profiles declare a separate completion deadline. FIFO applies within
a class among eligible work. After eight essential grants, the oldest eligible
ordinary waiter gets an opportunity while essential headroom remains reserved.

Each pool declares `capacity_bytes`, `essential_headroom_bytes`, and `source`.
Use one identifier for one physical pool. CPU/GPU UMA allocations must share the
same pool rather than adding virtual GPU capacity to host RAM. A null capacity is
unknown, and zero means exhausted. The initial native sampler supports `host`,
including Linux cgroup constraints. Other physical pools require provider-specific
sampling before protected execution. A declared capacity is a conservative ceiling,
not evidence of measured memory use or hardware qualification.

Each model profile binds its public model ID, `checkpoint`, and exact effective
`options`, excluding `pinned`. It declares `source`, `priority`, `concurrency`,
`completion_ms`, and allocation maps `load_bytes`, `resident_bytes`, `request_bytes`,
`state_bytes`, or `realtime_bytes`. Map keys identify physical pools. Context ceilings
must be positive and explicit; zero is not a ceiling. The llama.cpp embedding context floor is resolved before admission and stored in the effective options. Loading reserves its full peak, then retains only resident weights
and workspace for that worker generation. Requests add their incremental ceilings.
Concurrent reservations update one ledger atomically. Sampled headroom also accounts
for outstanding requests whose allocations may not yet appear in operating-system
readings. This is deliberately conservative.

Protected mode rejects missing profiles, changed execution options, unknown capacity,
and operations without explicit ceilings. The selected backend resolves each operation before reservation, and Router dispatches that same resolved request. Protected text requests also require `request_limits` with positive `input_bytes`, `batch`, and, for generation, `output_tokens`. Input size uses a conservative bound over the parsed JSON. Generations require an explicit positive token limit. llama.cpp Chat/Completions normalize its supported aliases to `max_tokens`; Responses uses `max_output_tokens`. Conflicting or unsupported aliases, nested `extra_params`, unknown parameters and unqualified probability output reject. Completion count is normalized to the native `n_cmpl` field and included in the batch bound. Multimodal and unqualified request shapes reject before dispatch. State requests with caller input use the same input bound. These are execution bounds, not an ingress/upload budget. Request JSON and client headers cannot
promote a request to essential priority. Authenticated queues use the accepted
principal and credential epoch. Anonymous callers share one queue authority.

Resource activation follows actual managed dispatch. Local validation and
unsupported capabilities that never dispatch release without quarantine. A completed
backend transport can release its request reservation. Disconnect,
exception, timeout, and an unreachable health check do not establish termination.
Uncertain reservations remain held. A supported engine's observed idle inference
slots can reconcile request work. They cannot certify that failed state exports or
loads stopped. Confirmed worker unload reconciles that exact generation. Successful
metadata calls cannot overwrite an earlier incomplete transfer in the same operation.

The initial observed wrappers are llama.cpp, FUTO/asr4all, whisper.cpp, Kokoro and
stable-diffusion.cpp. Other backends retain legacy behavior and reject protected
admission until their wrapper reports a managed lifetime. Observation alone does not qualify a workload: protected image, speech and transcription requests remain rejected until their request envelopes qualify. The loopback speech tests
verify HTTP/cancellation behavior, not transcription quality or accelerator performance.

Persistent cache inspection, including `/props`, holds the exact worker and uses its state lease deadline and accepted request cancellation. A canceled inspection cannot proceed to slots, inference, restore or save. Actual transfer dispatch activates the state claim; local metadata errors do not. Automatic model loading preserves typed admission status, code and retry metadata across inference endpoints.

Manual save, restore and erase reject while persistent caching owns that model's slots. Memory-mode mutations claim the same slot owner, preventing cache-mode changes during the operation. Failed state exchanges retain uncertain generation accounting; observed idle inference cannot clear them.

Every load generation, including a legacy retry, retains residency ownership. A failed model load does not evict unrelated workers. Replacement retains its selected generation across admission waits, revalidates registry membership, and retires it atomically against inference acquisition. Completion deadlines cancel managed load/state/GET work; teardown may outlast the deadline, and accounting remains held until termination is established. This is not a hard realtime execution guarantee.

Juiced rejects direct `/images/upscale` execution until it uses the same policy, loading and admission owners. Realtime rejects protected execution before either direct TCP or batch fallback, and pause rejects new Realtime input. WebSocket callback exceptions become bounded protocol errors rather than escaping the callback boundary.

Current limits remain explicit: realtime termination, downsized restore peaks,
provider installation and preparation scratch, early HTTP status
for deferred streaming admission, compute interference, and a complete shared runtime
state projection still require integration or qualification. Protected cloud passthrough
and realtime execution are rejected until their upstream lifetime is qualified. New protected providers
must not bypass those owners. No GPU/NPU latency or HA service protection is implied
by the CPU fixtures. The milestone requires the frozen interference suites and council
review before promotion.

The request-envelope tests inspect actual loopback provider JSON for Chat,
Completions and Responses in JSON/SSE modes. The supported llama.cpp schema is
reviewed against the [packaged revision](https://github.com/ggml-org/llama.cpp/blob/dde879bc4f954b809892fd0d660515f42536394a/tools/server/server-schema.cpp)
and its [Responses conversion](https://github.com/ggml-org/llama.cpp/blob/dde879bc4f954b809892fd0d660515f42536394a/tools/server/server-chat.cpp).
A future provider version still requires exact-profile qualification. Synthetic
portable tests inject known physical headroom; they do not certify their runner's
memory or accelerator telemetry.


Persistent-cache inspection tracks each direct backend exchange separately. Aborted
`/props` or `/slots` inspection retains its reservation until exact-generation stop.
Typed admission failures retain their HTTP status, machine-readable code and retry
header through persistent Chat, Ollama and Anthropic formatting.

Model registration, saved options, execution profiles and Juiced state use the
shared atomic file replacement helper. Saved option changes publish in memory after
their file commits. Retained job deletion acknowledges success only after durable
deletion or a durable tombstone. These single-file guarantees do not establish P3
complete-model promotion or multi-file rollback.

Policy changes and legacy pin requests stage the Juiced state candidate and its
revision before publication. That saved policy owns eligibility and Hot residency;
saved engine options do not duplicate policy. Runtime pin effects follow the
committed policy. A separate effects lock orders successful policy commits and
worker effects, without holding the state mutex during Router calls. Reconciliation
and queued unloads carry the committed per-model policy revision and exact worker
generation. Superseded effects cannot unpin or unload a newer Hot service.

Profile selection and saved options share ModelManager's writer. Option writers
resolve the committed recipe inside that transaction. Native option APIs reject a
recipe change since validation. Final readers recheck cache validity under their
read lock. Direct, supported and downloaded model views apply the same selected
profile. This does not establish crash-consistent P3 multi-file promotion.

Cloud forwarding and discovery use one per-provider snapshot of endpoint, credential,
auth header, plaintext permission, wire format and revision. Existing requests retain
their captured snapshot. New requests observe subsequent updates. Credentials remain
in process/environment memory. Delayed discovery checks its revision at publication.
Provider removal rejects busy generations before committing. Idle generations have
an acquisition gate while metadata commits. Failed publication reopens those gates.
Committed cleanup failure returns retryable pending generations; repeating uninstall
can finish cleanup after the provider record has already been removed.

The Juiced state owner retains a private last-committed checkpoint. Failed writes
restore legacy state mutations under the same mutex, including cache controls.
Revision advances only after commit. This checkpoint does not provide a consistent
multi-writer volume backup or complete model/runtime revision recovery.

Snapshot forgetting and pruning commit removal metadata and cleanup tombstones
before deleting saved computation. A failed metadata commit preserves every file.
Filesystem cleanup failures remain retryable across restart and do not advertise
forgotten snapshots. Pending cleanup blocks writes to that snapshot key. Inference can execute cold
while opportunistic saving skips unavailable disk capacity. Source weights and consumer messages are unaffected.

Snapshot startup binds `state.json` to the canonical state and snapshot directories
and a shared store identifier. The snapshot root retains
`.juiced-snapshot-store.json`; the state owner commits an unfinished binding before
publishing that marker, then commits readiness before starting its workers.
Only an empty snapshot root and empty inventories can initialize a new binding.
A matching unfinished binding can finish publication. Root changes, copied or
missing state, missing or mismatched committed markers, unbound legacy inventories
and ambiguous temporary files require recovery without replaying snapshot journals.
There is no automatic migration or reassignment. This binds cooperating state/root
selection across restart, not immutable filesystem identity, nested-root exclusion,
prior-worker termination, whole-store consistency or power-loss durability.

Router keeps a revisioned runtime projection of Juiced's durable residency policy.
Native load reuse, fresh publication, retry and restoration apply that projection.
Policy-generated Hot commands bind their revision before admission and revalidate
it through startup and publication. Explicit On-demand loading remains available.
Slow backend startup does not hold the policy effects or Juiced state mutex.
Deferred downsizing retains its selected worker and atomically rechecks generation,
pin, readiness, activity and idle duration before claiming maintenance.

Native options responses capture model/profile, saved values and default inputs
through one ModelManager snapshot. Expensive derived settings are computed after
releasing that lock. A dry-run derives its preview from the same snapshot and the same validated
set-or-clear mutation as a committed update, including sentinel clear values.

Cache disk diagnostics distinguish saved bytes, pending-cleanup bytes and reserved
write bytes. Undeleted files retain their charge. Pending snapshot writes reserve
capacity before dispatch and include aggregate temporary-write headroom. Exact
profile qualification may declare `max_snapshot_bytes`, a conservative bound that
must be supported by its evidence. Legacy qualification reserves the full disk
budget. A file exceeding its declared bound invalidates further writes for that
qualification, including error responses, malformed responses and late writes after
transfer cancellation. Disk evidence is checked again before cleanup and before
another save. Explicit requalification follows termination and cleanup of uncertain
writes. These declared provider envelopes are not a generic filesystem quota.

A completed snapshot save seals its checksum and metadata in a durable publication
intent before replacing the final file. The intent retains prior and candidate
ownership and charges pending, final and previous files. Previous-file cleanup uses
a durable tombstone after new metadata commits. The previous checksum is hidden during
publication. Metadata failure retains the intent; recovery verifies content outside
the shared state lock, then commits matching metadata. Each publication retains exclusive ownership
through verification and mutation; a duplicate reconciliation returns pending
without blocking unrelated state access. Sealed publication can recover
after manager restart; unsealed writes still require termination evidence. Conflicting
retain/forget operations reject until publication resolves.

Aborted writes remain charged until the current manager observes exact-generation
stop. Idle inference is insufficient. Interrupted writes from an unknown previous
manager remain conservatively charged and visible in `pending_writes`. Complete
operator recovery of those interrupted writes remains a P1 prerequisite before
promotion. Snapshot restore and cold inference do not require a new opportunistic
save to succeed. None of these cache controls changes source weights or stores
consumer conversation messages.


Recovery qualification currently covers manager-process restart with filesystem
contents retained, plus injected synchronous write/replacement failures. The
flush/close/rename path does not establish ordered storage across host crash or
power loss. That durability remains unqualified; atomic pathname replacement is
not evidence of synchronized file and directory contents.

`POST /v1/juiced/cache/recover` (also registered under the other three native
prefixes) takes an empty object and retries cache-owner reconciliation. A sealed
publication with missing/corrupt candidate restores checksum-verified previous
content when available. Otherwise it commits tracked cleanup. Failed deletion
retains file charges. Unsealed uncertain writes still require termination proof.

The existing Juiced state owner retains an authentication namespace epoch across
manager-process restarts. Unchanged restarts retain it; every
observed credential/auth-mode transition commits a new opaque epoch before serving.
Returning to earlier keys or anonymous mode does not reactivate earlier namespaces.
Failure to publish the new epoch prevents manager activation. Credentials remain
process/environment owned; only a configuration fingerprint and opaque epoch persist.

Chunked import publication commits its file/checksum, destination and registration
intent before moving the upload. Finish, cancel and late chunks share one import
claim. ModelManager commits registration without replacing another variant and
recognizes retries by import provenance. Restart or finish retry reconciles pending
publication. A competing name leaves explicit conflict ownership and can finish
under a distinct name. Published intent must finish before model removal; cancel
cannot delete its artifact. Earlier cancellation commits before deleting the part
file, and failed cleanup remains visible and retryable. The 1-byte-to-8-MiB chunk
protocol, declared-size bounds and free-space accounting remain in place.

The effective admin credential inherits the regular API key when its environment
variable is unset or empty. Empty/malformed credentials never match a configured
key. Enforcement, principal resolution and transition fingerprints consume that
same normalized configuration. With no keys, trusted-LAN access remains open;
an admin-only configuration protects control routes while regular APIs remain open.

ModelManager serializes each model's registration, import publication and removal.
Unresolved import intent rejects deletion, including native/gateway calls through
ModelManager, and upload removal claims ownership before touching files. Public
and user-prefixed names share the guard. Import no-overwrite checks the current
native namespace, including built-in/discovered names, and retains same-publication
idempotency. Initial and replayed recipe/purpose values use the same effective
labels. Library rows expose pending publication, errors, conflict-name resolution
and cleanup retries through those native operations.

Removal rechecks the complete registry definition after acquiring model mutation
ownership. ModelManager serializes reference acquisition against artifact deletion.
A new shared reference changes an uploaded-file removal into registration-only
removal when the operator reviews it again. Uploaded-file deletion first persists
`juiced_removal` in the existing registration. That marker hides the model from
usable discovery and reserves its name and file until deletion and the final
registry write succeed. Failed cleanup remains retryable through Juiced removal
or native delete after manager restart. Missing appliance files cannot acquire a
new registration reference. External files retain their existing registration
semantics. No shared model-cache or Juiced-state mutex spans file deletion.

This covers participating native registrations and owned upload cleanup, not a
complete immutable model-revision lifecycle or uncooperative external writers.
Pending import and preparation writers veto native removal of their source model.

Model-cache cleanup enters the existing writer and resource-admission owners.
Load and acquisition tickets bind the canonical model-cache root, retaining that
reference while resident or uncertain. Cleanup rejects if another accepted writer,
queued load, active acquisition or worker holds the affected root. Preview also
holds metadata stable, while permitting resident workers. Protected mode still
rejects unqualified physical management work.

ModelManager captures registrations, built-ins, selected settings and saved
profiles under its reference owner. It releases the shared model-cache mutex before
resolving paths, scanning or deleting files. References include checkpoint files,
directories, GGUF shards, configured provider directories, runtime overrides and
backend-declared argument files. Opaque arguments or
relative declared inputs reject cleanup with `artifact_reference_unknown`.
Only exact legacy auxiliary paths in repository roots or immediate snapshot
revisions are candidates. Blob directories and filename substrings are excluded.
Deletion rechecks each candidate's size, modification time and symlink target.
Retained unresolved preparation writers veto destructive cleanup.

Changing `models_dir` reserves both canonical roots through configuration
persistence and effects. Active references prevent that transition. A failed effect
reports saved intent separately from application. Later retries, including other
settings requests, must reacquire storage ownership before applying pending effects.
Failed persistence preserves the previous configuration and releases the reservation.
New registrations cannot reference a missing file inside the managed cache.
Admission observations expose cache references, inspections, cleanups, root changes
and uncertainty without publishing private filesystem paths.

The cleanup plan has a 10,000-entry ceiling and checks its completion deadline
between entries. These checks do not bound a blocking filesystem call, backend path
resolver or shard traversal. Apparent file sizes do not prove physical reclaimed
space. External writers, moved control roots and complete management resource
qualification remain unfinished P1 work.

Cache-profile qualification parses the native slot-save argument and compares its
filesystem directory with Juiced storage. It accepts quoted paths containing spaces
or backslashes and rejects missing or duplicate storage flags. An unrelated option
containing the directory does not establish cache configuration. Backend version
and exact-profile useful-reuse acceptance still apply.

Dynamic provider discovery runs outside ModelManager's shared cache and options-
writer locks. One builder serializes discovery, captures the native invalidation
revision and discards results if that revision changes. Provider-specific revision
checks remain at insertion. Native projection and alias publication stay under the
existing cache owner.

Model existence, download state, aliases and filtering observations use that same
final-validated cache lock. Sync previews resolve names and inspect download state
under one current snapshot. Registry update checks capture candidate names and the
cache revision before remote work. A changed revision rejects those results with
a retry message before updating cached flags or advertising an update for a new
definition under the same name.

Options/profile writes capture the recipe, selection and registry defaults after
discovery, with per-model mutation and file-writer ownership. Registration cannot
change that captured definition during persistence. Nested changes defer model-
change notifications until the outer claim releases, after writer ownership ends.
Watcher start/stop is serialized without holding the cache lock while joining;
directory changes publish with cache invalidation under that lock.

This lock boundary does not establish request-wide lookup deadlines, pre-body or
aggregate ingress admission, all-writer maintenance or essential-service protection.
Those remain P1 gates, alongside exact execution identities and terminal accounting.

Server lifetime teardown closes ModelManager notification admission and joins its
directory watcher and drains accepted callbacks before provider/backend dependencies are
released. Directory changes cannot restart the watcher after terminal stop. Cache
reads remain available during drain. Server construction failure uses the same
cleanup path because its destructor body does not run. Native member order retains
model/provider owners through Router teardown. The retained-job worker joins before
its Server dependencies disappear. BackendManager clears its process accessor only
when it still names that instance.

This terminal drain does not establish a reversible all-writer maintenance barrier.
Downloads, preparation, ingress and every other mutating path still require their
P1 bounds, pause, termination and recovery acceptance.

HTTP shutdown returns its response and signals Server's existing owner loop. It
uses normal teardown instead of a detached thread and process exit. POSIX signal
handlers publish atomic intent; an owned worker applies shutdown/reload operations
outside signal handlers and joins before Server destruction. Startup bind failure
also returns through teardown. Parent-watchdog signaling remains separate.

ModelManager and BackendManager share the Router admission ledger. Protected mode
rejects unqualified weight acquisition, registry update/sync work, runtime package
installation/removal and preparation before asset or tool dispatch. Native JSON,
SSE and server-owned entry paths preserve admission status, code and retry metadata.
A protected load can reuse an installed runtime without checking for updates.

Legacy management calls hold transition tickets through native execution. Those
tickets also follow accepted deferred streams, server-owned jobs and preparation
queues. They exclude configuration publication without consuming inference queue
capacity. Discarded undispatched streams and terminally stopped queues release
unused tickets. Unadmitted queued work rechecks policy before dispatch; rejected
model sync work reaches a terminal status. These legacy tickets provide lifetime
tracking without resource-ceiling qualification.

The llama.cpp owner contains native HF loading and opaque load controls. The
protected CLI subset accepts its single-slot default and snapshot-directory syntax.
Other raw controls, including URL/HF model selection and dynamic model directories,
remain unqualified. Persistent-cache ownership still validates the configured
directory and exact-profile useful reuse. Legacy controls retain native lifetime
tracking. Backend install-path lookup leaves the binary-cache directory uncreated; install
helpers retain explicit directory creation.

This containment does not finish ingress/import memory bounds, metadata/discovery
I/O qualification, durable-writer maintenance, full execution identity or provider
termination. Standalone helpers outside a live Router and external writers retain
their separate reference, promotion and recovery gates.

HTTP body admission runs after authentication and before buffering, including
`Expect: 100-continue`. Both native listeners and fixed protocol paths use it.
All modes now enforce an 8 MiB decompressed HTTP body ceiling. Existing import
chunks remain 1 byte to 8 MiB. Larger files use the existing chunk protocol.
The optional `admission.ingress` object accepts `request_bytes`, `total_bytes`
and `active_limit`. Defaults are 8 MiB per body, 32 MiB aggregate and four incomplete ordinary
body reads. The per-body setting cannot exceed the transport ceiling.

Known uncompressed lengths reserve their declared bytes. Compressed, chunked and
multipart exchanges reserve the full transport ceiling before reading. The HTTP
response owns the byte reservation through body reads, handler execution and deferred
response completion. Disconnects and rejected reads release local body ownership.
Ingress does not consume model execution slots. The admission snapshot reports
its active count and reserved body bytes separately. Byte exhaustion returns 429
with retry metadata. A body above its configured ceiling returns 413.

Compact admission, settings and shutdown bodies use the control reserve while execution is paused. Configuration
publication preserves existing ingress charges and rejects lower bounds that do
not contain them. Other native work still prevents admission changes. This does
not establish reversible all-writer maintenance or a total heap bound. Parsing,
protocol structures, response storage and provider execution retain their own
qualification obligations. Slow-client fairness and essential-service acceptance
remain open.

Import start, chunks, server-file registration and publication now use native
management admission. Unqualified protected work rejects before mutation. Pending
publication recovery also rechecks admission. Cleanup cancellation remains
available. The import journal, registration provenance and removal ownership
continue to define retry and recovery behavior.

Provider-driven cloud and FLM discovery now use the same native management
admission as acquisition. Protected or paused discovery defers before network or
CLI dispatch. ModelManager keeps local inventory usable and records timestamped
last-attempt observations. Model-list responses expose those observations with
`scope: last-attempt`. They describe attempted operations, not provider health or
an atomic snapshot of the whole catalog.

Cloud install/auth metadata still commits through its publication owner. Its
response separately reports discovery status and code. A deferred refresh does
not turn a committed metadata update into an HTTP failure. Explicit refreshes
retain provider-revision validation, so late results cannot replace current
models. Legacy discovery holds its transition ticket through publication.

Cloud model-list GET responses stop at 1 MiB before JSON parsing. Their existing
15-second timeout remains. FLM installed-model checks also use native admission
and conservatively report unavailable after rejection. This contains those
provider-driven paths. Filesystem catalog scans, hardware probes, legacy CLI
output/termination, parsing peaks and full metadata resource qualification remain
part of the P1 audit.

ModelManager JSON files now share the existing JsonUtils atomic byte writer.
This includes registration, options, profile selection, startup option migration,
collection manifests and registry provenance. ModelManager retains candidate,
transaction, repository and rollback ownership. The duplicated private byte
writer is removed.

Serialization completes before storage changes. A unique sibling is flushed and
closed before platform replacement. Existing POSIX file modes are preserved.
Failures preserve the committed file and clean staging when possible. If cleanup
fails, the error identifies the retained uncommitted path. Such files do not
become registered models or committed options. Complete staging-file recovery,
cross-writer maintenance, multi-file promotion and host-crash durability remain
separate gates.

Ingress read slots finish when a registered handler receives its buffered
body. The response still owns its byte charge through completion. Long inference
therefore retains bytes without occupying a read slot. Admission observations
report ordinary `reading`, `control_reading` and retained `active` bodies separately.
The existing `ingress.active_limit` applies to incomplete ordinary reads.

Compact settings, shutdown and admission controls have one independent read slot.
Up to 64 KiB of aggregate headroom beyond the configured per-request limit is
reserved for them. Larger settings bodies use ordinary admission. The total body
byte bound still applies. This protects compact control body capacity, not every
HTTP worker or socket queue.

The shared native handler wrapper checks JSON structure through SAX before
handlers build trees. Limits are depth 64 and 32,768 key/value events. Structural
excess returns 413 with `ingress_structure_limit_exceeded`. Ordinary syntax errors
retain native handler formatting. Native and fixed-protocol handlers share the
wrapper. Import chunk bytes bypass JSON interpretation. Fresh protected multipart
input rejects before parsing until its envelope qualifies.

Input ownership also charges `host`, using the same ledger and sampled available
bytes as model work. The current planning estimate is `65536 + 8 * body_bytes +
512 * min(body_bytes, 32768)`. It allows for buffering, JSON values and temporary
copies. It is reported as `native-http-input-v1` with `peak_qualified: false`.
This estimate is not a measured upper bound on every handler, provider converter
or response. Those peak measurements remain P1 acceptance work.

Protected input admission requires a known host-memory sample. A configured host
ceiling and essential reserve also apply. Without a configured host pool, input
uses sampled headroom only. Its observation explicitly reports
`limit_kind: sampled-headroom` and `configured: false`. An explicitly null host
capacity remains unknown. Unknown memory returns 503 `ingress_memory_unknown`;
known exhaustion returns 429 `ingress_capacity_exceeded` before body buffering.
Legacy mode records input estimates without enforcing physical input admission.

Input and model reservations constrain each other. Completed reads retain their
host charges through response completion. Compatible protected configuration
changes must preserve all live input estimates. Compact recovery controls can
use essential reserve but still require known physical capacity. Body-free
metadata and shutdown remain outside input-memory reservation.

These contracts do not complete absolute input deadlines, measured parsing and
response peaks, multipart qualification or HTTP worker fairness. Those remain P1
gates.

The Windows embedded entry point owns its background server run. Tray exit asks
that run to stop; HTTP shutdown also stops the tray event loop. Headless fallback
waits for run completion. Every return or exception joins the worker and destroys
Server before Winsock cleanup. The UI stays on its creating thread. Silent startup
failures log and return a failure status without an error dialog.

Windows qualification requires `EmbeddedServerShutdownTest`, which builds the
actual embedded executable and checks HTTP shutdown and occupied-port cleanup.
Linux terminal-shutdown checks do not establish Windows execution evidence.

AliasManager serializes alias writers, reloads and directory changes. Candidates
persist before live publication. File I/O runs outside its read lock, so model
lookups retain the committed map during a staged save. Alias byte persistence
uses JsonUtils atomic replacement; it has no overwrite-copy fallback. POSIX alias
files retain owner-only permissions. Windows ACL behavior remains unqualified.

Failed alias storage returns HTTP500 and preserves live bindings. Invalid JSON
and field types remain HTTP400; validation conflicts remain HTTP409. Retry and
restart consume only committed changes. Corrupt-file recovery retains separate
backups. These are per-owner publication contracts, not cross-process exclusion
or whole-store maintenance.


`lemond` and the Windows embedded server acquire an exclusive OS lease for each
canonical cache/config directory before configuration loading or migration.
They pass that same owner into Server. Startup claims the selected model-cache
root before ModelManager construction and the snapshot root before snapshot writes.
Normal teardown retains these claims through member/process-owner destruction.
Constructor failure releases its partial claims after owned cleanup. The OS releases
locks when the process exits. Neither release nor shutdown acknowledgement proves
that a surviving backend or helper stopped.

The empty `.juiced-store.lock` control file remains in place and must not be removed
while a manager is running. Directory aliases share ownership within the existing
owner. Fixed startup and migration roots remain protected for the entry-point
lifetime. Scoped claims share the same native lease object and release its handles
when their last view finishes. Temporary directory candidates do not accumulate live handles.
An existing read-only control file can be opened for locking without rewriting it.
Windows permits this through a read-capable [LockFileEx handle](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex).
POSIX claims also retain a close-on-exec, read-only directory descriptor with an
exclusive `flock`, acquired before opening the control file. Shared ancestor
directory locks exclude foreign parent/child claims without serializing siblings.
Each missing component is created only under its parent's intent lock. The owner
retains those descriptors through partial failure or final scoped release.
Existing read-only directories are inspected without attempting creation.

Startup roots supplied together acquire broader ownership first. A nested claim
retains an existing owned ancestor. A later broader claim rejects if acquiring it
would require dropping a live narrower lock. There is no non-atomic lock upgrade
or temporary loss of protection. Root/ancestor readability is required. This
does not pin pathnames against external moves. Windows hierarchy exclusion remains
unimplemented; its existing exact-root control-file ownership stays separate.
The same native root resolver normalizes trailing separators and dot aliases for
preflight, acquisition and snapshot-store binding before creating missing roots.
A read-only managed root without an existing usable control file cannot acquire
ownership. Ordinary read-only external source directories remain outside writable
store claims and are not modified. Their external consistency still needs validation.

Model-directory changes first retain current-ledger exclusion and acquire the
candidate process claim, then persist. Failed persistence releases the candidate.
Failed application retains current and pending claims. Successful application
pins filesystem resolution to the claimed canonical directory and releases the
previous direct claim. A nested destination retains its inherited ancestor lock
until the last dependent claim releases. Returning to a former directory requires
fresh acquisition unless that ancestor remains retained by a dependent claim.
ModelManager publishes the directory and cache invalidation under its cache owner.
Model selections retain their resolved root. Router rejects obsolete selections
before dispatch, after serialization waits and after load admission. Canonical path
resolution stays outside Router's lifecycle lock. Snapshot paths also pin the claimed
canonical directory, with UTF-8 environment paths supported by native path utilities.

Local model-free process acceptance covers conflicting cache/config/model/snapshot
roots, failed activation cleanup, independent roots, directory handoff, normal
restart and process termination. The independent managers share an external source
directory without claiming or modifying it. Unicode model/snapshot roots are included.
Windows recovery permits bounded retries because OS lock release can lag process exit.
Local Linux regression acceptance removes a live model root's control file and
verifies that a second isolated native manager is rejected. Parent-first and
child-first native fixtures reject overlapping stores before descendant creation
or ancestor-marker publication. Siblings remain usable, and confirmed release
permits retry. Source changes and Linux checks do not qualify macOS or network
filesystem behavior. These checks do not establish moved-directory recovery,
network filesystem locking, foreign/older-manager exclusion, prior-manager
writer termination, whole-store backup consistency or host-crash durability.
Backend, helper, device and remote writers retain their P1 termination gates.

The standalone `lemond --inspect-snapshot-store` path runs before telemetry,
directory claims, migration or worker startup. It reads Juiced snapshot state and
the selected default/environment snapshot root, with bounded parsing, records,
file inventory, checksums and redacted diagnostics. Binding schema validation is
shared with startup. Other journal checks are diagnostic predicates, not proof
that an incomplete record owns a terminated writer.

Inspection has no mutation, cleanup, migration or termination authority. Its
report always leaves `application_authorized` and `writer_termination_verified`
false. Exit status0 describes completed metadata/file validation only; status2
reports issues or an exhausted inspection budget. No result establishes a writer
fence, an atomic filesystem snapshot or the consistency of other store families.
Reads use retained regular-file handles beneath anchored directories. Windows
leaf handles permit delete sharing so ordinary replacement can proceed; retained
ancestor handles restrict directory changes. Use that platform's inspector offline
from known writers. These [Win32 sharing rules](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew)
remain in effect until handle close and are not an all-writer maintenance fence.


Router host admission and CPU/NPU automatic sizing consume the native
SystemMetricsPlatform memory budget. GPU discovery metadata no longer supplies
CPU capacity. Linux samples MemAvailable, the visible cgroup hierarchy and
`/proc/self/limits`. Finite or unreadable address-space/data limits leave process
allocation headroom unqualified and return unknown capacity. These limits do not
add another memory pool. Windows samples physical, current-process commit and address-space
headroom through GlobalMemoryStatusEx. A failed sample or job-membership query
keeps headroom unknown. Windows job membership is explicitly unqualified because
an immediate-job query does not establish all ancestor limits. The sampler does
not infer unlimited jobs from host capacity.

macOS samples raw Mach free pages with the native page size and physical total.
It conservatively excludes inactive and compressed memory from admission headroom.
Finite or unreadable process address/data limits keep headroom unknown. Display
memory totals and usage remain separate from this available-byte contract.

A protected configuration update with unknown live input headroom returns503
`ingress_memory_unknown` and preserves its committed mode and revision. Known
exhaustion retains its capacity/conflict errors. A configured ceiling cannot
replace an unknown native sample. HTTP acceptance records the native sample and
whether positive protected cases ran. Unqualified native containment requires
actual unknown-memory rejection across native and gateway body routes. All hosts
also test an explicitly unknown configured pool after restart. This is containment,
not successful Windows-job or finite-limit service qualification.

The source contracts are documented in
[MEMORYSTATUSEX](https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/ns-sysinfoapi-memorystatusex),
[IsProcessInJob](https://learn.microsoft.com/en-us/windows/win32/api/jobapi/nf-jobapi-isprocessinjob),
[Windows nested jobs](https://learn.microsoft.com/en-us/windows/win32/procthread/nested-jobs),
and Apple's [Mach page counters](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/vm_statistics.h).
These references do not establish local platform execution or latency qualification.


Background sync/download dispatch handles worker-start failure before reporting
successful acquisition. Sync target resolution finishes before publishing its new
generation. Server reserves thread-owner storage before starting a joinable sync
thread. Failed dispatch completes only that queued generation with a retained
failure result and returns500 `worker_start_failed`. It releases its accepted
management ticket. A late failure for an older generation cannot terminate a
newer queue.

Download start failure retains an error row with `running:false`, releases the
accepted operation capture and permits retry under the same ID. Accepted workers
remain owned until joined. Controlled native fixtures inject a failure immediately
before thread construction, then execute successful retries through actual threads.
These checks do not exhaust OS thread quotas or establish all-writer quiescence.
The complete native writer/termination audit remains a P1 prerequisite.

Sync terminal results share one publication owner for completion, start failure,
terminal error and exception unwinding. It retains the latest 32 generation
outcomes. Queries for evicted results return `not_found` for the requested
generation, rather than another generation's current state.


Native direct-process stop now requires observed exit and owned handle cleanup.
`ProcessManager::has_exited()` is a positive, non-mutating observation. A failed
liveness query, unknown PID ownership, invalid handle or stop timeout is not exit
proof. Windows waits for the process object before closing its handle. Linux and
macOS share an owned-child helper that checks wait ownership before signaling,
uses bounded TERM/KILL waits and keeps unknown outcomes conservative. Existing
backend cooldown remains separate from device-termination evidence.

ServerProcess keeps its handle when explicit stop cannot confirm cleanup.
WrappedServer retains failed process owners for retry, including previous processes
whose cleanup failed during replacement. Stop success requires every retained
owner to finish and the worker's process membership to remain unchanged. A detached
process remains tracked while another stop owns it. An empty current-process slot
cannot certify that concurrent stop as completed. Watchdog
cleanup failures retain ownership and report failure without escaping their thread.
A failed worker destructor retains uncertain residency in the live admission ledger.
Dispatched loads retain logical uncertainty even when their legacy estimate is zero.

Derived worker destruction contains stop failures. Whisper temporary audio and
vLLM startup shims remain on disk while exit is unconfirmed. Automatic eviction
retains the failed generation as `cleanup_pending`, preserves its reservation and
retries cleanup before releasing ownership. Both the idle evaluator and pressure
monitor contain failures at their thread boundaries. Ollama unload reports a
retryable `unload_incomplete` error for busy or unconfirmed cleanup. An absent
worker still satisfies that protocol's unload convention.

Ordinary configuration publication and its effects serialize through Server.
Listener readers retain a shared lifetime while one lifecycle owner replaces or
stops the listener. Shared RuntimeConfig and Router locks are released before
listener stop/start. Cloud metadata publication retains its separate registry
ordering and does not wait on listener effects.

A failed effect returns503 `configuration_application_failed`, with `saved:true`
and `applied:false`. An identical settings request retries pending effects. A
runtime binary change blocks new loads for its recipe until affected workers
confirm cleanup and installation succeeds. Failed drain or stop prevents install,
promotion and reclamation. Pending reloads survive an application retry in this
manager process. Complete bundle manifests, references and restart recovery
remain P3 requirements. These controls do not qualify arbitrary package writers.

BackendManager install and uninstall operations exclude another mutation for the
same normalized recipe/backend key while their native operation ticket remains
held. This includes progress callbacks and terminal publication. Runtime helper
references can share that key, but exclude replacement in both directions.
Unrelated keys remain independent. An unresolved dispatched ticket continues to
block matching replacement after its initiating scope ends. These checks do not
establish cross-recipe dependency identity, all-worker bundle references or
all-writer maintenance; those remain separate lifecycle obligations.

Participating native writers now acquire an operation scope through the existing
ResourceAdmission owner before taking publication locks. Configuration scopes span
saved intent and Server effects. Alias scopes cover ordinary edits, reload
quarantine and directory changes. Model scopes cover options/profile transactions,
registration, acquisition and deletion through their final native publication.
Nested synchronous writes share the accepted operation. Opaque continuation tokens
retain it across a deferred boundary and restore its context on another thread.
Tokens are internal lifetime handles, not client-provided authorization.

Internal writer maintenance closes new mutation scopes and new execution work;
it differs from ordinary inference admission pause. Accepted continuations can
finish their state/helper effects during drain. Writer records consume no physical
inference slot or per-user quota. A wait includes participating nonresident work
through final owner release, even after device work reports completion. Active or
uncertain helper tickets also prevent that drain from completing.

Server-owned downloads and SSE providers retain their writer continuation before
deferring execution. Queued Juiced model commands retain it through profile journals,
model publication and terminal status. Shutdown cancels proven undispatched commands
before releasing their queue ownership. Import operations span file replacement,
registration and final journal publication. Persistent-chat finalizers retain their
accepted context through snapshot save, cleanup and file/metadata publication.
Background reconciliation takes a fresh operation scope before changing native state.

Download controls retain their own accepted mutation scope through the final worker
join. Read-only download inspection defers history expiry when maintenance closes
writers, then permits normal expiry after reopening. Control acknowledgements do
not establish that a still-running downloader stopped. Completion timestamps use the
explicit empty time point as their unset marker. Initialized negative steady-clock
values remain valid for elapsed-time expiry; the visibility interval is unchanged.

Historical JobManager records have no executor or model-operation callbacks.
Metadata deletion enters the same admission owner before locking the record store,
commits before updating memory, and preserves unknown fields in other records.
Read-only history never rewrites the file. Previously active graphs appear
interrupted and cannot resume. Existing deletion tombstones remain quarantined,
with bounded recovery metadata; neither startup nor ordinary history deletion
can discard them or certify a prior writer stopped. Unsupported or malformed
storage is preserved and blocks mutation. Fixed native acquisition, inference,
media and Barista operations continue through their existing owners.

Model synchronization retains one accepted writer per queued generation. Native
workers claim that generation exactly once; stale dispatch/failure handles cannot
consume a newer queue. Cancellation before claim publishes a terminal result with
no metadata/process dispatch. Active cancellation retains ownership until accepted
metadata effects finish. Startup failure and unexpected exceptions use the same
bounded terminal-result owner. Target-name resolution occurs before the sync-state
lock, including full-check results and failure reporting. Status inspection remains available.

FLM helper ownership retains its accepted writer through process collection,
terminal journal and caller publication. Unconfirmed cleanup or failed publication
keeps both tickets and writer context. Trusted cleanup can finish that accepted
operation during maintenance; new helper dispatch rejects. Caller-owned deletion
publication still requires its matching native owner. Prior-manager ambiguity
still requires independent recovery and never gains a fabricated continuation.

Retained voice and legacy preparation cleanup use their existing management
tickets through confirmed process stop, staging cleanup and terminal metadata
publication. Failed stop or publication keeps maintenance undrained. Cleanup of
that accepted work can finish while new dispatch is closed. Prior-manager voice
intent and legacy preparation records remain quarantined until separate recovery
establishes termination.

Maintenance rejects new loads and user inference. An accepted native mutation may
finish a required load or restoration through its continuation, with the same exact
resource-profile, physical-capacity, queue and deadline checks. A continuation never
authorizes new user inference. Ordinary pause and shutdown still reject acquisition.
Draining waits for queued admission entries as well as active owners and finalizers.
Read-only native state inspection remains available without opening a writer.

This is operation accounting, not whole-server maintenance acceptance. No public
maintenance-ready endpoint is exposed. Remaining helper/metadata writers,
external roots, idle-worker stop and prior-manager recovery still need
complete integration and evidence. Empty participating-operation counts cannot
prove storage consistency or that another process stopped writing. Synchronous
profile rollback evidence still does not establish crash-consistent multi-file
recovery or rollback-write failure recovery.

Preparation retains its process, accepted management tickets and staging reference
through cancellation or shutdown until cleanup confirms direct-child exit. A
failed stop leaves `cleanup_pending:true` and a pending job. The owning worker
retries cleanup, and terminal status commits before its tickets release. Dispatched
zero-cost management tickets retain uncertainty if their owner is destroyed.
Idle inference cannot release those tickets.

A persisted dispatch intent precedes helper startup. On restart, an unresolved
writer remains pending and blocks Juiced's source-removal plan. Only a
proven queued job becomes Interrupted without writer reconciliation. Prior-manager
writer recovery and full descendant termination remain P1 prerequisites. The
retained native fixture controls helper startup and stop outcomes, with no model
conversion, installation or accelerator claim.

Wyoming helper startup now acquires a native management ticket and persists its
dispatch intent. Replacement and shutdown explicitly stop and confirm the owned
direct process before discarding it. Failed stop or terminal-state publication
keeps the process and ticket, exposes `cleanup_pending`, and permits cleanup retry
without launching a duplicate. Changes to the helper command or endpoint also
require retirement of the prior owner. Paused/protected admission rejects new
unqualified helper dispatch. On manager restart, a pending prior-run intent blocks
duplicate startup and remains unresolved. Complete operator recovery, descendant
termination, actual speech and end-to-end stop deadlines remain P1 prerequisites.
The helper fixture uses controlled process substitution and metadata failures.

A present Wyoming intent must contain a bounded nonempty owner, boolean pending
state and a matching dispatching/stopped outcome. Missing, inconsistent or
unsupported records remain unchanged and report `recovery_required`; constructor,
reconciliation and shutdown cannot replace them or authorize a new helper.
An absent legacy record or valid stopped record can start an owned helper.

Native loading and automatic residency reuse reject `worker_cleanup_pending`
with HTTP 503 while retirement remains unresolved. A live child retained after
failed unload is unavailable for successful reuse. Confirmed exact-generation
cleanup must precede a replacement.

WrappedServer retains a process before invoking its startup and retires the prior
owner before dispatching a replacement. Startup exceptions can therefore leave
an owned process for cleanup retry. vLLM retains its ROCm shim after failed load;
confirmed unload or destruction precedes shim reclamation.

OpenMoss voice design uses the same worker process owner as speech. Failed designer
cleanup blocks speech restart and marks the worker unavailable. Ending an active
request does not restore readiness while cleanup is pending. New inference/state
leases and already accepted speech/audio calls reject that state. Successful
cleanup and speech readiness are required before serving resumes. Controlled
process/capability fixtures establish these caller boundaries, not actual speech,
ROCm execution, descendants or device-kernel termination.

FLM load-time pull, explicit download and removal now share ModelManager's native
command owner. `native_helpers.json` records dispatch intent and an opaque command
identity before spawn. The process layer attaches its handle before output callbacks
and delegates terminal cleanup to that retained owner. Timeout, cancellation,
callback failure and failed terminal publication keep unresolved handles, tickets
and mutation exclusion. A matching retry can publish a previously collected success
without executing the command again. Native health exposes pending helper state.

FLM deletion treats the provider checkpoint ID as shared storage. ModelManager
serializes its decision, then retains a durable checkpoint reservation through
command dispatch without holding shared reference/cache locks over the process
exchange. Provider discovery is a storage observation, not another authored
registration retaining its own checkpoint. A
shared user registration is removed without invoking `flm remove`; a built-in
checkpoint with other references rejects destructive deletion.

A `juiced_removal` provider intent binds the expected registration, checkpoint,
program and unique publication ID before dispatch. Native discovery hides that
registration and reference acquisition rejects the reserved checkpoint. The
helper journal seals its collected success with `publication_pending` before the
caller records completion. Failed caller publication retains that result and
its ticket. Matching retry/reconstruction seals the caller result without another
remove command; generic helper cleanup cannot acknowledge it. A durably completed
caller marker survives later unrelated helper commands until final unregistration.
Unknown prior-manager uncollected processes remain quarantined, not replayed.
These fields change persisted state and require candidate-specific consistent
backup/restore acceptance before deployment. Manager reconstruction and synchronous
fault fixtures do not establish host-crash or power-loss durability.

Helper tickets hold a `flm:npu` runtime reference in the existing ledger. Participating
BackendManager install/uninstall operations exclude that reference, including
uncertainty reconstructed from a prior-manager journal. A new helper cannot acquire
runtime files during their managed replacement. Unknown prior-manager ownership
blocks another FLM command and reclamation. Activation can reject when a paused or
protected bootstrap cannot restore that conservative reference. Complete operator
recovery, immutable bundle manifests and external-executable replacement remain
separate P1/P3 obligations.

Owned command output lines have a 64-KiB limit. POSIX streaming observes exit without
reaping in the output loop and retains the collected status. Callback exceptions
propagate after owned cleanup. Windows collection requires a completed wait and
preserves signed exit codes. These are direct-child contracts. The controlled CLI
fixtures do not qualify an NPU, real FLM downloads, descendants or model quality.

Local acceptance includes real direct children, non-consuming exit observation,
unowned PID rejection, controlled failed stop/replacement, exact-generation
reconciliation and actual neutral CPU serving/cache. This does not establish
process-tree, helper descendant, device-kernel or remote termination. Destruction
can report an unknown native cleanup outcome but cannot guarantee OS cleanup.
Unknown prior-manager writers and reversible all-writer maintenance remain open.
The native contracts follow
[Windows termination](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-terminateprocess),
[process-object waiting](https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-waitforsingleobject),
and [owned-child wait observation](https://man7.org/linux/man-pages/man2/waitpid.2.html).


Startup failures also write a direct stderr diagnostic before returning nonzero.
Directory contention remains fail-closed when the logging sink cannot display the
failure. HTTP acceptance probes compiled gzip support with a bounded RPC. Supported
codecs must reject oversized decoded input with413. Unavailable and unknown codecs
must reject with415. Both paths must release ingress ownership. Unsupported codec
rejection does not qualify successful decompression on that package.

Prepared-copy inventory and reclamation use ModelManager's registration/reference
owner. Both `checkpoint` and `checkpoints` definitions protect the directory.
Cleanup retains that owner through filesystem effects, while shared model-cache
and Juiced state locks remain released. Registration after cleanup rejects a
missing file under the managed prepared root, including filesystem-equivalent
path casing. Juiced excludes new preparation work while cleanup is active and
rejects cleanup while accepted preparation jobs remain pending. Failed or partial
cleanup stays visible for a reference-checked retry. These rules cover retained
prepared files and synchronous failure, not complete P3 artifact/worker references,
conversion quality, hard filesystem quotas or host-crash durability.

### Loaded inputs and cache compatibility

The held worker lease supplies cache profile identity, address, options and
context. Cache inspection does not pair a newer registration's files with an
older worker. For local llama.cpp loads, ModelManager enumerates each declared
checkpoint/companion and complete GGUF shard family. Startup hashes their content
and the launch executable, then verifies those inputs again after readiness.
Supported custom template, LoRA and control-vector file arguments use that same
content resolver. Scaled/list arguments retain their file roles. Unknown argument
semantics disable persistent qualification with a visible `configuration_reason`.
Ordinary serving remains independent of that qualification decision.
Cancellation is checked between 64-KiB reads; identity work also has a 120-second
elapsed-time bound inside the accepted load deadline. Blocking filesystem calls
are not an end-to-end I/O deadline guarantee. Directory-backed or remotely resolved
inputs remain uncaptured and cannot qualify this persistent-cache path.

Profile schema4 excludes generation, listener port and observation timestamps so
unchanged worker reloads/restarts preserve compatibility. It includes the frozen
declared definition, artifact and supported argument-file content, resolved
options, context layout, launch executable and reported engine build. Earlier
receipts and snapshots remain retained and require requalification. This does
not qualify adapter, speculative or vision continuation merely by hashing files.
Complete loaded-runtime and physical-device identity, runtime environment
mutation and complete request/state semantics remain open P1 requirements.

A worker's declared recipe, model type, checkpoint roles and resolved paths freeze
at publication. Registration changes cannot relabel that generation. Companion-only
and path changes report `settings_pending` in native discovery and the catalog.
An explicit native load applies changed definitions through normal worker
replacement. A matching repeated load reuses its existing generation. Definition
comparison performs no filesystem work under Router's shared lock.

Linux process-image capture also records the actual child executable and regular
files backing executable mappings. It joins procfs mapping links, file metadata,
mount/root namespace and filesystem identity. Btrfs uses its governing mount device
when its subvolume `stat` device differs. Missing/deleted files, writable or unknown
executable mappings, unresolved association and bounded-query failure prevent
persistent qualification. Kernel vDSO/vsyscall mappings are outside this file-content
subset. Reads check cancellation and cap mapping data at 4 MiB, 65,536 rows and
4,096 distinct files. These bounds do not prove end-to-end filesystem latency.

Startup hashes mapped files and revalidates their association through the existing
process owner. Procfs short reads are consumed through a bounded complete walk.
Repeated inventory checks do not make the kernel mapping view atomic. Mapping addresses, inode numbers and timestamps remain transient
verification facts. They do not enter compatibility fingerprints. Cache profiles
and state exchanges revalidate the captured file association. Changed or late-loaded
modules disable qualification without relabeling the frozen generation. A rejected
pre-dispatch save releases unused credit. An unresolved dispatched operation still
requires confirmed termination.

The Linux image supplies separate CPU and Vulkan runtime trees from the same
verified published release. The CPU tree omits unselected GGML accelerator/RPC
components, retains CPU/core libraries and licenses, and has a hashed file inventory.
Its launcher preserves native arguments. Staging rejects linked directories and
external file aliases, rebases internal file aliases, and materializes generated
launchers/inventories without following copied links into source files. Image
defaults select that tree for CPU; saved executable overrides remain authoritative. The Vulkan tree is unchanged.
This prevents a CPU default from initializing unselected accelerator libraries.
An explicit override can still be unqualified if its mapped code is writable or
otherwise unverifiable. This is runtime packaging, not weight conversion or a
claim of complete runtime-bundle promotion/rollback.

The process mutex owns these checks and content reads. Router/model/configuration
locks remain released. File-backed mapping identity does not prove mapped-page
immutability or include every nonexecutable runtime asset. This remains distinct
from complete immutable runtime bundles and all-writer/root maintenance. Windows
and macOS currently report process-image association unavailable, so this new
persistent-cache qualification path remains unqualified there. Ordinary serving
continues. Their image capture and real-device acceptance remain required work.
Mapping lookup parses the address range before constructing its kernel pathname.
Linux prints low addresses with leading zeros in
[maps](https://github.com/torvalds/linux/blob/master/fs/proc/task_mmu.c), while
[map_files](https://github.com/torvalds/linux/blob/master/fs/proc/base.c) names omit
that padding. Normalizing the range supports non-PIE executable mappings without
relaxing inode, device, path, namespace or writable-code checks. The Linux native
launch fixture uses a non-PIE child to exercise this boundary.

The [kernel procfs contract](https://docs.kernel.org/filesystems/proc.html) documents
mapping-read and process-lifetime limitations. Portable metadata is not proof of
shared-library or device execution on another platform.

### Owned initial execution environment

On Linux, the process owner reads the actual child's initial exec environment
with cancellation, a 2 MiB byte ceiling, 4,096 entries and bounded names/values.
It never iterates the manager's mutable environment. Raw values stay private;
credentials and unrelated variables never enter public identity or digests.
Before/after inventories include the initial environment and current child working
directory. Missing or ambiguous observations prevent persistent qualification.

The llama.cpp owner interprets a bounded set of numeric and enum controls and
the observed library search path. An environment-selected template resolves
against the actual child directory. Its content digest must equal the template
reported by the ready engine. A file changed before capture or overridden by a
different applied template therefore cannot receive a misleading qualification.
Other relevant backend, loader and accelerator controls remain unqualified until
their semantics are supported. Diagnostics omit their supplied names and values.
Ordinary inference remains independent of opportunistic persistent saving.

Identity schema4 includes this supported environment projection. Unchanged manager
restart preserves it; changed supported values or template content require new
useful-reuse qualification. Existing snapshots remain under their original
fingerprints. This is not complete environment or device qualification:
[/proc/PID/environ](https://man7.org/linux/man-pages/man5/proc_pid_environ.5.html)
reports initial exec inputs, not subsequent environment changes inside the worker.
The [pinned argument parser](https://github.com/ggml-org/llama.cpp/blob/dde879bc4f954b809892fd0d660515f42536394a/common/arg.cpp)
defines the supported engine's environment semantics. A different engine version
needs its own acceptance; launch labels alone cannot establish behavior.

### Native listener association

A live child and a reachable URL are separate observations. Native startup now
requires the owned process to hold the IPv4 loopback/wildcard TCP listener before
and after a successful bounded health response. Active-request watchdog probes
use the same association check. Linux joins owned socket file
descriptors to procfs TCP entries in the manager's network namespace. Windows uses the TCP listener owner table.
macOS inspects the child's socket descriptors through libproc. Query failure or
an unconfirmed association cannot publish readiness. Controlled process adapters
must explicitly declare their simulated listener ownership.

Startup health polling uses an elapsed deadline, inherited cancellation and a
64-KiB response ceiling. Stop/collection time remains separate. The listener query
is serialized by the process owner, without a shared Router/configuration lock
across the network exchange. Enumeration is bounded, but kernel/filesystem query
latency still needs the end-to-end termination/interference gates.

Persistent-cache inspection and save/restore/erase exchanges recheck that same
held worker before dispatch and after response. A missing association returns
`backend_listener_unverified`; an unused save reservation remains distinguishable
from an unresolved dispatched exchange. This covers direct-child IPv4 listeners,
not general descendant listeners, shared-port authentication, remote computation,
all protocol forwarding paths or complete runtime/device identity.
