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
apply settings, then resume. Hot policies remain saved. Runtime pause is temporary
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
must be explicit. Loading reserves its full peak, then retains only resident weights
and workspace for that worker generation. Requests add their incremental ceilings.
Concurrent reservations update one ledger atomically. Sampled headroom also accounts
for outstanding requests whose allocations may not yet appear in operating-system
readings. This is deliberately conservative.

Protected mode rejects missing profiles, changed execution options, unknown capacity,
and operations without explicit ceilings. Request JSON and client headers cannot
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
admission until their wrapper reports a managed lifetime. The loopback speech tests
verify HTTP/cancellation behavior, not transcription quality or accelerator performance.

Current limits remain explicit: realtime termination, downsized restore peaks,
provider installation and preparation scratch, early HTTP status
for deferred streaming admission, compute interference, and a complete shared runtime
state projection still require integration or qualification. Protected cloud passthrough
and realtime execution are rejected until their upstream lifetime is qualified. New protected providers
must not bypass those owners. No GPU/NPU latency or HA service protection is implied
by the CPU fixtures. The milestone requires the frozen interference suites and council
review before promotion.
