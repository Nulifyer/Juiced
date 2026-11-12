# Selected Whisper physical settlement

Read-only review of the selected Linux CPU speech pipeline. No engine, server,
inference, build or acceptance test ran during this review. This records a gap
and a proposed correction, not implementation or transcript-quality acceptance.

GitHub's `v1.8.4` ref resolves to
`d92d51e9f02af313eaec258be905623775e169c6`. The inspected raw source is pinned
to that commit. The selected release archive remains the separately hashed asset
in `juiced/rewrite/scripts/qualify-speech.ts`; source inspection does not establish
a reproducible binary build.

The pinned server holds `whisper_mutex` throughout `/inference`, installs a
socket-closure abort callback, and produces its response after inference returns.
CPU graph execution polls that callback after a node, then synchronizes its
threads. Parallel Whisper workers are joined before return. This supports
cooperative abort, without a stated cancellation deadline. `/health` reports model
readiness and does not acquire that mutex. There is no request cancellation
acknowledgement route.

Primary evidence, using actual raw-file line numbers:

- [Server handler and abort callback](https://github.com/lemonade-sdk/whisper.cpp-rocm/blob/d92d51e9f02af313eaec258be905623775e169c6/examples/server/server.cpp#L805),
  lines 807, 971-985, 1125 and 1170.
- [Socket-liveness callback](https://github.com/lemonade-sdk/whisper.cpp-rocm/blob/d92d51e9f02af313eaec258be905623775e169c6/examples/server/httplib.h#L7320),
  with response writing after routing returns at line 7401.
- [CPU abort poll and synchronization](https://github.com/lemonade-sdk/whisper.cpp-rocm/blob/d92d51e9f02af313eaec258be905623775e169c6/ggml/src/ggml-cpu/ggml-cpu.c#L2988),
  lines 2990, 3007 and 3301.
- [Parallel-worker joins](https://github.com/lemonade-sdk/whisper.cpp-rocm/blob/d92d51e9f02af313eaec258be905623775e169c6/src/whisper.cpp#L7825),
  line 7894. The selected qualification argv uses the default single processor.

Raw source SHA256 identities:

```text
examples/server/server.cpp b9bb1d52ff645be02917edcb9d54ba744ac2fbb25022600c12d43629bedd0e00
examples/server/httplib.h f4272973fe5b117194b39df0552b87366cd9af6504b2f217f1793c76ef01f33c
src/whisper.cpp fab1d79eb7c3f8362caede7dae45a89435dbf9d3b0dc88aa3cf01a6c46009613
ggml/src/ggml-cpu/ggml-cpu.c a83c94126df48c4c2a2860f75916f7e0ea0d7b9d54db69a184af8f66d289ac49
```

The Rust handler at `service/src/http.rs:804` owns a normal `RequestLease` while
awaiting the worker. Timeout, transport failure or handler cancellation drops that
lease. `service/src/supervisor.rs:1082` immediately decrements `worker.active`;
its slot and power activity then drop. `service/src/admission.rs:652` admits another
request when the slot drops. There is no physical-completion witness on this path.
The model's resident reservation remains held by the running child, so this is a
slot, activity and busy-state gap rather than premature model-residency release.

The native mutex prevents concurrent use of the Whisper context. It does not make
the released Rust slot truthful. Further requests can accumulate inside the worker
while Rust reports spare capacity. A dropped client connection and a successful
health probe cannot establish that computation has settled.

An empty `/inference` request is also insufficient as a settlement barrier. On a
different connection it can reach the mutex before the original request finishes
multipart parsing or enters the handler. Its rejection cannot fence that earlier
request. Socket pooling, elapsed time and abort log text should not become implicit
completion protocols.

The smallest correction should remain specific to selected Whisper requests:

1. Bind the physical request claims to the existing process-registry generation
   while the running child is selected. PID or semantic fingerprint alone is
   insufficient. `ManagedChild::generation()` already exists.
2. Establish a retained settlement owner before polling upstream `send`. Mark the
   request as possibly dispatched before that first poll. Validation and queue
   cancellation before dispatch keep their existing release behavior.
3. A final response from the pinned synchronous handler is positive evidence that
   its inference returned. Result validation can still fail independently. Keep
   this witness scoped to the selected contract, without promoting every
   `whisper-cpp` definition or arbitrary executable to qualified support.
4. On cancellation or transport failure without that witness, abort the transfer
   and retire only its exact owned child generation. Retain the slot, power
   activity and active-accounting claim until confirmed child exit. Cleanup
   failure retains those claims for retry. Do not route retirement through the
   ordinary busy/Hot unload rejection or through storage-admission guards.
5. Transfer claims synchronously to retained child cleanup resources before any
   cleanup await. Drop must only signal an already established owner. Avoid
   `Worker -> Running -> resources -> Worker` or Manager reference cycles; a small
   separate accounting object can retain counters without retaining the worker.
   Failed-cleanup retry must remain possible while those physical claims are held.
6. Preserve HTTP timeout/cancellation outcomes separately from physical settlement.
   Leave chat, completion and Responses abort behavior and their existing worker
   reuse guarantees unchanged. Saved residency policy is unchanged.

Native tests should withhold child-exit acknowledgement and prove that slot,
activity and busy accounting survive caller cancellation, cleanup failure and
cleanup-task cancellation. Only the exact selected generation may retire, and a
replacement or unrelated worker must remain untouched. Normal responses, including
worker errors after computation, should not cause unnecessary retirement.

Process acceptance needs a private speech fixture whose physical work survives
HTTP-future cancellation. The present fixture's `speech-request/cancelled` event
comes from request-future Drop, so it cannot prove that distinction. Separate
physical-start/settled events should demonstrate queued admission staying blocked,
then releasing only after positive completion or owned exit acknowledgement.
Cover cancellation before headers, timeout, malformed results after a final
response, shutdown, failed stop and successful retry. Existing OpenAI disconnect
tests must continue to prove their original nonblocking behavior.

The next selected real-engine test should retain the archive/bundle/model/argv
identities, establish actual pending CPU work, cancel its client, and observe the
exact child-exit acknowledgement before a replacement request runs. Normal warm
reuse and a subsequent successful transcription remain required. Do not retain
transcript words or claim output quality. The current real-Whisper test covers
transport, load, unload and restart; it contains no real compute-settlement test.
