# Resource admission

This native source batch adds admission under Router ownership. It is not yet a
qualified essential-service protection release. The default `legacy` mode retains
existing operator-approved profiles and reports `latency_qualified: false`.

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
`pools`, and `models`. Pending defaults are 64 globally, 16 per service and 8 per
principal. Queue wait defaults are 2, 10 and 30 seconds respectively. These bound queue
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
Startup fails immediately if another participating process owns either root.
Normal teardown retains ownership until the Server and its owned work have drained.
The OS releases ownership when the process exits. The empty `.juiced-store.lock`
control file remains in place and must not be removed while a manager is running.
Directory aliases share ownership. Legacy-cache migration protects its source
before mutation and leaves the held control file at that source.

Local process acceptance covers competing roots, independent model-free managers,
normal restart and recovery after terminating a model-free manager. Windows test
recovery allows bounded retries because OS lock release can lag process exit.
Production startup remains nonblocking. These checks do not establish network
filesystem locking, whole-store backup consistency or host-crash durability.
External model/snapshot directories, older nonparticipating binaries, and backend,
helper, device or remote writes still require their P1 maintenance/termination gates.


Router host admission and CPU/NPU automatic sizing consume the native
SystemMetricsPlatform memory budget. GPU discovery metadata no longer supplies
CPU capacity. Linux retains the existing MemAvailable and visible cgroup hierarchy
reader. Windows samples physical, current-process commit and address-space
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
