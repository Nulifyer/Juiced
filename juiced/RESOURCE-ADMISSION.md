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
apply settings, then resume. Updates are staged and persisted before publication. A revision-bound transition blocks new admission during publication. Cloud registry metadata uses the same serialized publication owner; credentials remain process/environment owned. Configuration commits use atomic platform replacement, without overwrite-copy fallback or throwing post-commit cleanup. Write/replacement failure retains the old configuration and returns an error. Hot policies remain saved. Runtime pause is temporary
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
