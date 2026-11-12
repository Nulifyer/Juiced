# Rust process-level acceptance

These tests launch the built Rust service, worker fixture and CLI as subprocesses. Requests use loopback TCP and HTTP. No API test invokes Axum handlers directly.

Build the binaries once from `juiced/rewrite/`, then run the fast suite:

```bash
cargo build -p juiced-service --bins --features test-fixture --locked
cargo test -p juiced-rust-tests --locked
```

The harness reads the service's actual ephemeral listener announcement. Each test owns a temporary config directory and synthetic weight file. Startup and HTTP requests have deadlines. On Unix, each manager and its workers have an isolated process group that the harness terminates during normal cleanup or panic unwinding. Windows cleanup uses `taskkill /T`; Windows runtime acceptance still requires a native run.

`JUICED_TEST_SERVER`, `JUICED_TEST_WORKER` and `JUICED_TEST_CLI` override executable discovery. Otherwise the harness finds the binaries beside Cargo's test `deps` directory, so debug and release targets use their corresponding binaries. It never runs Cargo inside a test.

The API suite covers the endpoint inventory, all four native prefixes, read/inference/operator credentials, malformed and oversized requests, durable policy conflicts/restart, worker lifecycle, resident and slot pressure, read-only suitability, upstream failures/timeouts, stream and non-stream disconnects, crash recovery and bounded content-free request history. Its explicit route matrix must equal the service's endpoint registry. A newly registered route fails coverage until a real behavior and failure scenario is added.

Queue cases verify FIFO ordinary work, cancelled waiters, bounded pending and active limits, expiry, independent workers, essential headroom and authority, bounded essential priority, maintenance drain, stale-policy rejection and retired-pool isolation across model ID reuse. Snapshot counters and worker observations establish admission behavior over actual HTTP requests.

Startup cases hold a private fixture behind a release file. They verify distinct-model FIFO admission, shared same-model loads, queue cancellation, capacity and timeout failures, maintenance, stale policy and shutdown. Waiting models have no process, resident reservation or power scope. Library fault tests separately cover retaining the original permit until detached preparation and child cleanup settle.

Live-root cases replace claimed directories with copied foreign trees. They check cross-store mutation refusal, cheap conflict/not-found precedence, queued startup cleanup and unrelated-manager independence. A controlled HTTP source holds range transfers and final response EOF, allowing assertions that rejected resume cannot start a network transfer or truncate staging, and that failed publication retains nonterminal bytes and charges across restart. Child shutdown remains possible after root loss. These process observations cover checks at operation boundaries; native storage tests cover the seams between verification and filesystem effects.

Runtime catalog cases preserve the reviewed 69 definitions, 18 native recipe migrations and 44 native support rows over real HTTP. Every legacy recipe/backend label resolves with exact implementation identity and original device/OS restrictions. Hybrid, installation provenance and recipe-specific NPU identities retain explicit blockers; vocabulary mapping cannot create execution qualification. Successful and rejected reads preserve model, admission, history and managed-file state.

Execution-profile cases retain legacy fixture inputs, verify that reads preserve schema-1 configuration bytes, and check that explicit saves publish canonical schema 2. They exercise both registration formats, reject mixed or invalid profiles before worker startup, and compare launch fingerprints across port, process, revision and option changes. Changes to owned weight fixtures during startup refuse readiness, while changes after loading block cache publication before engine dispatch. Restoring the original bytes requires a reload before cached state can be restored.

Runtime identity cases verify process generations and redacted summaries, then change a private copy of an actually mapped library. Cache checks reject changes before dispatch and after engine save/restore acknowledgements, retain uncertain bytes and charges, and clear failed restore ownership. The original library stays unchanged. Persistent cache acceptance currently requires Linux runtime observation; other platforms must report unsupported observation and reject saves without staging while ordinary inference remains usable.

Model option cases separate saved overrides from effective legacy arguments, verify HTTP updates reach the restarted worker, preserve policy and physical slots, and reject stale writes, forbidden fields, live changes and queued references to replaced workers. Context cases keep GGUF training metadata, configured launch, spawned launch and native engine observations separate. Unloaded inspection cannot start a worker; an actual slot inventory mismatch must release its reservation.

Operational request history, suitability/context, admission/cache and artifact/job reads require operator credentials for both GET and HEAD. Model and saved-option metadata remain readable by ordinary API clients. Process cases verify the distinction under every native prefix and preserve principal-bound cache ownership checks.

Management cases exercise runtime registration/detail/removal, persistent restart, duplicate and invalid registration rejection, Disabled-by-default policy, monotonic revisions across ID reuse, retention of existing weights and maintenance drain/resume. A managed-weight tamper case changes acquired bytes after registration, rejects loading before worker spawn, and checks recovery after restoring the original bytes. Process-level store tests reject duplicate roots and aliases, preserve live ownership after control-marker unlink on POSIX, reject overlapping parent/child roots before marker or missing-descendant creation, permit independent siblings and allow reuse after release.

Model alias cases create, swap, persist and delete an alias, then route OpenAI, Anthropic, Ollama and MCP requests through it. A request admitted before a swap completes on the old target while the next request reaches the new one. Listings, request history, Disabled targets and target-removal and model-ID conflicts are checked. Option reload cases restart a loaded Hot worker under a new PID with the new arguments, keep its policy and reject a busy worker without saving.

Unix configuration-binding cases start through a parent-directory alias from a private working directory, then retarget the alias. Relative models, worker bundles, policy publication and default cache/artifact effects stay under the originally claimed canonical store. The replacement tree remains unchanged, while configuration-leaf symlinks reject startup. Linux checks the owned worker's actual model argument and executable path through bounded `/proc` reads.

Cache cases save actual fixture slot bytes, verify their checksum, restart the manager, restore and continue. They cover principal, credential epoch, protocol and session isolation; disabled policy; generation/revision conflicts; corruption; and retained, charged uncertain writes. Engine acknowledgement counters must be positive.

Session cases enable automatic conversation snapshots, which ordinary fixtures turn off. The fixture's `--emulate-prompt-cache` mode keeps per-slot word tokens, honours `id_slot`, saves and restores them through `/slots`, and reports `prompt_n`, `cache_n` and `cached_tokens`. Conversation A, then B on one slot, then A again must restore with only the new turn processed. The same must hold after a service restart. Other cases cover header precedence, unload flush, count-budget eviction, `cache_prompt: false` bypass, identity-change discard, multi-slot affinity, the session routes under all prefixes and explicit-operation refusal.

Artifact cases use a controlled loopback HTTP source with chunked delivery, byte ranges and a stable validator. They exercise download/hash publication, pause/resume across manager restart, cancellation, quarantine/retry, generation conflicts, removal and source policy rejection. A managed-artifact case acquires weights, registers a model, loads and infers, restarts, then releases registration and worker holds before deletion. Inventory paging walks each section under every prefix, keeps off-page selections, and refuses stale views and malformed cursors without changing configuration or storage.

Library cases run the service against a loopback fake Hugging Face Hub selected by `JUICED_HUGGINGFACE_URL`. They search, pull one quant under each native prefix with operator credentials, verify the completed artifact digest and job source URL, publish a two-part split as one artifact set, and check local presence after restart. A synthetic `HF_TOKEN` must reach only Hub API requests, never downloads or responses. Offline and public-HTTPS policies must serve catalog results without contacting the fake Hub. The harness defaults `JUICED_HUGGINGFACE_URL` to a closed loopback port, so no test contacts the real Hub.

Protocol cases cover Anthropic messages/token counting, Ollama chat/generate/embeddings/catalog, native embeddings, finished SSE/NDJSON, MCP discovery/inference/cancellation, authentication and malformed input.

Terminal artifact-job metadata can be forgotten with `POST /juiced/artifacts/jobs/{job}` and `{ "action": "forget", "expected_revision": N }`. Cases verify that completed-job removal preserves artifact bytes, references and registration; cancelled history retains zero charge; stale, active, paused, failed and quarantined jobs cannot release unconfirmed bytes or reservations through this action. Explicit cancellation can clean up a confirmed inactive acquisition before its terminal metadata is forgotten. Tests cover abrupt manager death during download and a publication journal fault that preserves real published bytes and charges. These fixtures establish pipeline behavior, not model quality.

Byte-exact forwarding tests compare a fixture-observed SHA-256 against the original HTTP JSON bytes, including whitespace, key order, UTF-8 and escaped model IDs. They also verify that the caller's Authorization header and manager key environment variables do not reach the worker. Credential cases cover equivalent Bearer/x-api-key transport and conflicting or duplicated headers.

CLI cases run actual commands against explicit isolated origins. They check admin/inference credential precedence, state and policy changes, options, registration, artifact acquisition, native cache save/restore, bounded JSON input, distinct exit codes and stdout/stderr separation. Controlled responses echo a synthetic credential to verify redaction in both successful and rejected responses. Credential-bearing commands cannot select an unauthenticated UDP target implicitly.

Speech cases use strict multipart transcription requests through all four native prefixes. They check exact audio bytes, explicit defaults, JSON/text/subtitle results, field and body limits, cancellation, queueing and content-free records. A slow unread multipart body proves that the ingress byte budget rejects more inference bodies while authentication, health reads and operator controls remain available. A private bundled-worker case verifies its redacted digest and rejects changed or unlisted files before loading.

Telemetry cases read `system-stats` under all four prefixes with operator credentials and wait for a newer background sample. They check that worker stdout and stderr lines reach `juiced/logs` with model, generation, level, truncation and a monotonic cursor. A fixture writes 2 MiB to each pipe before every reply; inference must still complete, and the ring must report dropped lines. Fixture `timings` and `usage` must appear in `last_inference` and request history without prompt or reply text.

On Linux, a direct-worker lifecycle case kills only the manager PID, observes worker death before any cleanup signal, adopts and reaps that exact worker, and checks an unrelated service remains usable. An opt-in case repeats this with actual llama-server. The proof covers direct child processes. A separate manager-only SIGTERM case confirms that both owned workers are reaped before successful shutdown and that another daemon remains usable. Startup cancellation checks the exact child PID, resident reservation, and logical generation before loading a replacement. SIGTERM during Hot startup also confirms child exit before successful manager shutdown.

Snapshot inspection cases execute `lemond --inspect-snapshot-store PATH` without configuration. They compare directory contents before and after inspecting missing, legacy, healthy, corrupt and unresolved stores. A live writer lease remains usable, and inspecting a healthy store without a control marker creates none. These observations grant no recovery, repair or writer-termination authority.

System-info cases set the test-fixture-only `JUICED_TEST_HARDWARE_ROOT` to a synthetic Ryzen AI 9 HX 370 tree with a gfx1150 iGPU, XDNA2 NPU and `/dev/kfd`, then read `GET /system-info` under every native prefix. They check operator authority, HEAD, ROCm/NPU recommendations, that hybrid is never recommended, the refusal when `/dev/kfd` is missing, and a timed-out `vulkaninfo`. One case observes the actual host and checks only the response structure and cache reuse.

Engine cases run on Linux x86_64 with a synthetic CPU-only hardware root and the test-fixture-only `JUICED_TEST_ENGINE_RELEASES` pointing at a loopback fake GitHub. That override drops pinned digests, so the release document's `sha256:` digest is used instead. The fake server publishes a tar.gz whose `llama-server` is the fixture worker, next to a versioned library and its symlink, and redirects each download. The case reads `GET /juiced/engines` under every prefix, refuses non-operator credentials, refuses `models/add` with `engine_not_installed` before installation, and rejects stale generations, unbuildable backends and unknown engines. It installs llama.cpp CPU, checks release provenance, the link-free payload and staging cleanup, then adds the same GGUF artifact under all four prefixes, loads one model and completes a chat. A whisper.cpp release whose published digest differs from its bytes must fail with `engine_hash_mismatch` without changing the package generation.

Media cases (`media_pipeline.rs`) run the fixture worker as an sd.cpp worker. They check image generation under every prefix with auth, the `<sd_cpp_extra_args>` translation, PNG payloads, `/sdapi/v1/txt2img` with an omitted model, bounded rejections, operation mismatches and single-slot registration guards. A dropped client connection during generation retires the exact worker generation, and the next request starts a new one. On Linux x86_64, a loopback fake GitHub serves an sd.cpp zip and a Kokoros tar.gz whose entrypoints are the fixture worker, and a fake Hugging Face Hub serves the SD Turbo and Kokoro catalog repositories. The case searches the Kokoro bundle, pulls both catalog models (Kokoro publishes a model+voice set), refuses `models/add` before installation, installs both engines and adds both models from their primary artifacts. It then generates images and WAV/MP3 speech under every prefix, checks the forwarded speech request and the short phoneme-data link, lists voices and refuses voices for the image model.

Argument guard cases use renamed private fixture copies to exercise the selected production argument grammar. User-configuration cases change only files under their temporary HOME and XDG_CONFIG_HOME.

Ordinary fixtures disable suspend inhibition. The separate Linux power case requires a positive logind `sleep:idle` block acknowledgement and checks shared leases, cancellation, idle release and manager-only shutdown:

```bash
JUICED_TEST_POWER_INHIBIT=1 cargo test -p juiced-rust-tests --test power_lifecycle --locked -- --ignored
```

The fixed-port native discovery case is separate from the quick suite. Run it only during a coordinated interval with UDP 13305 free:

```bash
cargo test -p juiced-rust-tests --test native_discovery --locked -- --ignored --test-threads=1
```

It refuses an occupied port or unrelated traffic, checks the actual daemon beacon, then runs actual CLI discovery and scan without concurrent receivers. It never stops another listener or service.

## Tiny real-engine pipeline

These separate opt-in tests load an existing GGUF through an installed CPU `llama-server`, check structural result/token data, unload and restart. A cache case saves native slot bytes, restarts, restores and continues inference. A session case switches between two chat conversations on one slot and restarts the service. Both resumptions must report `restored`, cached tokens covering the earlier prompt and fewer than 32 processed prompt tokens. It requires positive engine save/write/restore/read counters and matching byte counts. The tests do not score output wording or download weights.

```bash
JUICED_TEST_LLAMA_SERVER=/path/to/llama-server \
JUICED_TEST_GGUF=/path/to/tiny.gguf \
cargo test -p juiced-rust-tests --test real_engine --locked -- --ignored
```

To include the Linux direct-worker parent-death proof, add `--test linux_parent_death` beside `--test real_engine` in the command above.

An omitted prerequisite is an explicit test failure when the opt-in tests are requested. The normal fast suite reports these hardware/asset-dependent tests as ignored.

## Tiny real-whisper pipeline

Speech cancellation coverage uses the deterministic worker and proves HTTP transfer and lease cleanup. Real-whisper cooperative compute-stop timing remains unqualified.

This opt-in CPU case uses the checked-in German WAV and a verified whisper.cpp bundle manifest. The harness clears daemon loader variables; the selected runtime bundle supplies library directories only to its engine child. It checks JSON and UTF-8 text responses, process reuse, unload and cold restart without printing or scoring transcripts.

```bash
JUICED_TEST_WHISPER_SERVER=/path/to/whisper-server \
JUICED_TEST_WHISPER_MODEL=/path/to/ggml-tiny.bin \
JUICED_TEST_WHISPER_BUNDLE=/path/to/speech-runtime-bundle.json \
cargo test -p juiced-rust-tests --test real_whisper --locked -- --ignored
```

## Matched HTTP overhead measurements

Build the benchmark once and run the same workload against each isolated service:

```bash
cargo build --release -p juiced-rust-tests --bin bench-api --locked
./target/release/bench-api --url http://127.0.0.1:13306 \
  --path /v1/health --requests 20000 --concurrency 16 --pid SERVICE_PID
```

The runner warms eight requests per connection worker, reuses keepalive connections, drains every response body and reports JSON with successful/error/status counts, completed-response p50/p95/p99 latency and throughput. `--body-file` selects a fixed POST workload; the body is loaded once and cloned as shared bytes, and only its digest appears in the workload identity. Credentials come from the existing Lemonade environment variables and are never printed. URLs with embedded credentials or query strings are rejected. HTTP errors produce a failing exit code rather than a passing result with hidden failures. Measurement has a 120-second deadline.

On Linux, `--pid` adds the selected process's CPU-time delta, clock resolution, before/after RSS and a 5 ms sampled RSS peak. Child-worker CPU/RSS is excluded, and sampling can miss shorter peaks. Unsupported or unavailable process metrics are null. These measurements do not count allocations or establish inference-quality or production release qualification. Preserve exact engine/model/config/workload identities when making comparisons, and run allocation/flame-graph profiling separately.
