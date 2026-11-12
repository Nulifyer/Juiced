# Juiced Rust and Bun rewrite

This isolated rewrite runs a Rust model supervisor and a Bun/TypeScript/React console. It preserves Juiced's appliance design and T3-derived visual foundations. React and React DOM are the console's only runtime npm dependencies; the production console is static and needs no Bun process.

The [plan](PLAN.md) and [feature crosswalk](FEATURE-COVERAGE.json) retain all 211 catalogue features and six Juiced requirements. The current implementation covers selected foundation, lifecycle, artifact, cache, admission and protocol pipelines. It does not replace the release service or migrate production state. See [verification](VERIFICATION.md) for executed checks and open release gates.

## Run an isolated preview

From `juiced/rewrite/`:

```bash
cd ui
bun install --frozen-lockfile
bun run build
bun run fixture
```

Open <http://127.0.0.1:13306>. The launcher starts actual Rust daemon/worker processes with synthetic replies, temporary configuration and fake weights. Stop it with Ctrl+C. The preview supports policy changes, load/unload, temporary tests, maintenance pause, Library acquisition/registration, explicit slot snapshots and bounded diagnostics. It does not judge model output quality.

For development, `bun run dev` proxies `/v1/` to `http://127.0.0.1:13306`. Set `JUICED_DEV_API` for another isolated service; the development UI defaults to port 5178. Credentials and opaque cache sessions stay in tab memory. Only the service origin and navigation selection may persist locally.

## Use an installed engine and existing weights

Copy `config.example.json` to a separate directory and add a registration:

```json
{
  "id": "my-model",
  "model_file": "models/model.gguf",
  "execution_profile": {
    "schema_version": 1,
    "adapter": "llama-cpp",
    "implementation_id": "llama-cpp",
    "command": "llama-server",
    "args": ["--host", "127.0.0.1", "--port", "{port}", "--model", "{model}", "--slots"],
    "options": {"context_tokens":4096, "parallel_slots":1},
    "placement": "unresolved"
  },
  "resident_bytes":4294967296,
  "max_inflight":1,
  "operations":["chat/completions", "completions"],
  "policy":"disabled"
}
```

Configuration schema 2 has one versioned execution profile per registration. Schema 1 files migrate in memory without rewriting the input; an explicit policy, options or registration edit saves the canonical schema 2 format. Mixed launch fields and unknown versions reject. Back up experimental configuration before using an older binary, which cannot read schema 2. Legacy flat HTTP registrations remain accepted; the console sends the canonical nested profile.

Startup claims the configuration's parent directory and reads its regular file through that retained handle, with a 16 MiB limit. Relative model, engine and bundle paths and default stores resolve from the claimed canonical directory. Retargeting the input directory alias cannot redirect them or later policy writes. Configuration-file symlinks and observed root/file replacement reject. Saves use the same 16 MiB bound, including JSON escaping and the final newline. An oversized update returns `413 configuration_too_large` before changing saved state or registration references. The Manager claims configuration, cache and artifact roots before opening either store. Mutations and resumed store writes check the same retained root set; a lost root returns `503 storage_root_unavailable`. Stop and shutdown remain available to settle actual children and writers; failed cleanup retains ownership. These checks do not establish a writer fence or complete-generation migration.

The adapter selects compiled integration behavior; the implementation ID names the selected runtime distribution. Generic HTTP subprocesses have unknown implementation identity. Placement remains unresolved until physical device/resource bindings are implemented; legacy GPU flags do not qualify hardware.

Selected llama.cpp and whisper.cpp profiles validate argument names and arity before execution. The manager owns the model, private loopback address, port and snapshot-directory bindings. Remote downloads, credentials, presets, additional models and external public directories reject. Typed options resolve only actual option positions. A literal template value that resembles an option stays literal. Generic HTTP subprocesses retain opaque templates and cannot claim native snapshot support.

Child environments exclude manager credentials, service-manager authority, runtime argument overrides, download credentials, proxy settings and loader injection. Selected llama.cpp launches inspect its system and user `config.ini` paths without reading their contents. Only absent or empty regular files are accepted. Changed configuration observations block launch or cache operations until reload. These retained metadata observations do not fence external writers or qualify an immutable package.

`inhibit_suspend` defaults to `true`. On Linux, startup, requests and snapshot work share a logind sleep-inhibitor lease, with a short idle grace period. Acquisition and release run on a dedicated thread. Failure is best effort and appears in `resources.power`; `held` requires a positive OS acknowledgement. Setting it to `false` performs no inhibitor OS calls. Windows and macOS currently report unsupported. Request cancellation ends the HTTP activity scope, which does not prove that a real engine stopped computing.

`context_tokens` is the total engine launch budget. Model training metadata, saved settings, launched arguments and observed serving-slot context remain separate. Do not advertise a per-request capacity by dividing a budget. The explicit llama.cpp provider checks `/props` and `/slots` before readiness; observed slot count must match the declared physical profile. Other compatible workers can use the default `external-http` provider, with observation and typed-option support left unknown.

Reservations are operator declarations, not measured hardware fit. A manager-owned process registry retains each exact child generation, artifact/cache/root holds and reservation through cancellation or failed stop. `stopping` and `cleanup-pending` are unavailable for new tests or loads; retry Unload to confirm cleanup. A requested kill is not an exit acknowledgement. This covers direct children; portable descendant containment remains a release gate.

`admission.startup` separately bounds cold startup. Defaults are `concurrent: 1`, `pending: 64` and `wait_ms: 30000`. Pending zero disables waiting when capacity is occupied. These settings apply at daemon startup. Queued attempts hold no resident reservation, power activity or child process. Duplicate loads join the same attempt. Queued policy changes cancel stale intent immediately. The console shows Waiting to start and lets a tab stop waiting for its own load without changing the saved policy.

Ready releases the startup permit. Cancellation or failed cleanup retains it until detached inspection and the exact child generation settle. Expired unclaimed grants are reclaimed during gate operations; there is no background reaper. Owned permits never expire into free capacity while work remains. Queue waits have deadlines, not completion promises. Active-startup policy locking, role transitions and inference-slot ownership before cold loading remain separate resource-planner work.

Engines run as subprocesses with direct argument vectors and loopback listeners. Relative model/executable paths resolve beside configuration; bare executable names use PATH. Each worker retains its resolved launch plan, effective options and verified model/executable handles. Argument substitutions are literal and resolved once; applied observations read that retained plan. The child environment is captured before spawn, with Juiced settings, manager/provider credentials and service-manager notification/socket/credential metadata removed, then bundle loader settings applied. Its digest joins cache compatibility without exposing environment values. Only declare operations the selected engine implements.

Every explicit llama.cpp profile receives its literal public model ID through the managed `--alias` argument. The alias participates in launch and cache identity. Ambiguous comma-separated aliases reject before spawn. The selected runtime's response model field uses the public ID. Earlier direct-profile snapshots with a different launch identity remain retained and cannot restore into this changed identity.

```bash
cargo build --release --locked -p juiced-service --bin lemond
./target/release/lemond --config local/config.json --ui ui/dist
```

Windows uses `target/release/lemond.exe`. The default listener is `127.0.0.1:13306`; `--listen ADDRESS:PORT` overrides it. New models start Disabled. Hot policies load at startup; enable and load explicitly after registration. Use separate rewrite directories. POSIX directory ownership excludes participating overlapping writers; Windows hierarchy and production migration remain open gates.

## Protected cache memory

Protected mode reserves cache controller memory on the same host ledger as model work. With the default 16 MiB manifest and 16,384-entry limits, startup reserves 114.1 MiB before parsing. It then retains a 64.1 MiB metadata ceiling. This ceiling stays outstanding because metadata can grow after later host samples. It is a reservation, not an allocation or measured resident memory.

State operations reserve a separate 22 MiB controller workspace. Inventory pages of at most 128 rows reserve 8 MiB. Inspection pages of that size reserve 22 MiB. Explicit larger pages reserve 98 MiB, including bounded response storage and orphan-file observations. These are separate from a model's declared `state_bytes`. Cache replies have a 32 MiB wire limit. Serialization counts bytes before allocating the exact buffer. The buffer keeps its claim until every transport byte reference is released. Caller cancellation retains actual blocking work until completion.

Cache metadata claims remain available during an inference maintenance pause. They still obey shared host capacity, headroom, sample freshness and shutdown. Saving or restoring model state still requires unpaused worker admission.

These conservative controller bounds preserve the configured host capacity and headroom. They do not qualify total process RSS or model peaks. Diagnostics separates metadata and cache work from the shared total. Unloading a model preserves the metadata claim. Reducing this reservation requires a separately verified growth-accounting design.

If protected capacity is unknown or cannot admit startup metadata, the service leaves the cache unopened. Health and suitability remain available. Cache requests return a capacity error. Restore capacity and restart the isolated service. Retained files are not parsed, rewritten, deleted or represented as an empty inventory through this refusal path.

## Cache pages

`GET /v1/juiced/cache` and `/v1/juiced/cache/inspect` default to 128 rows. The same query contract applies under all four version prefixes. `limit` accepts positive values up to the store entry bound, normally 16,384. Inventory rows appear in `data`; inspected rows appear in `snapshots`.

To continue, copy `page.next_after` into `after`, the response `generation` into `expected_generation`, and `page.view` into `expected_view`. Both guards are required for continuation. A changed generation, retained membership or manager instance rejects the old continuation. Refresh the first page after a conflict. The opaque view token grants no recovery or mutation authority. Busy state, checksums and filesystem observations remain point observations.

Inspection returns a separate bounded `orphan_files` list and `orphan_page`. Continue with `orphan_after` plus the same generation and view guards. `observed_count` counts names seen during the bounded scan. When `scan_complete` is false, it is not a total and no orphan continuation is certified. Inspection retains selected row metadata and numeric charge projections while reading files, rather than pinning complete unselected records.

The browser provides Previous/Next controls and resets to the first page after refresh or mutation. Private test drafts remain local across page changes. Polling failures back off to 4, 8, 16 and 30 seconds; manual refresh retries immediately. The CLI accepts `cache [--limit N --after ID --generation N --view TOKEN]` and the same flags for `cache inspect`, with an additional `--orphan-after NAME`.

## Back up imported settings

Offline settings capsules preserve the exact coordinating configuration and every retained private settings file. This includes original/current metadata and pending intents. Existing descriptors preserve absences and alias ownership. Export requires a configuration already bound to an imported native settings store.

Stop the isolated daemon before exporting. The capsule's parent directory must exist. Choose a new destination:

```bash
./target/debug/lemond --config local/source/config.json \
  --backup-settings-copy local/settings-capsule
./target/debug/lemond --config local/restored/config.json \
  --restore-settings-copy local/settings-capsule
```

Restore creates a private target directory and validates the complete settings store. It verifies staged configuration bytes before an atomic rename that refuses replacement. Partial configuration bytes stay off the primary path. A matching retry before normal service use reuses checksum-verified files. After service use adds external stores, choose a fresh destination. Conflicting files, including incomplete copies, stay preserved and block publication. Keep the failed target for recovery, or restore into another new directory. Changed configuration is never overwritten by retry.

Capsules have bounded manifests, file counts, sizes and cooperative deadlines. Copies use a fixed buffer and independently hash destination bytes. Cancellation keeps actual file work owned until its bounded phase finishes. A blocked filesystem call has no hard deadline. Participating directory locks exclude a live owner. File identity and checksum checks remain point observations across outside writers.

Restore the external model artifacts, runtime packages and computation cache separately before serving configurations that reference them. This capsule covers settings transport only. Complete appliance recovery, consistent external-store recovery and older-native-executable rollback remain release gates. Native rollback requires the matching complete original native backup.

## Import a copied native settings cohort

The experimental offline command imports an explicitly supplied copy. It does not read production state automatically:

```bash
./target/debug/lemond --config local/config.json \
  --import-native-settings-copy copied-native-config \
  --native-bindings local/native-bindings.json
```

The target must be a separately owned rewrite schema-2 configuration. The optional bindings file is a strict array of Disabled model registrations with schema-2 execution profiles. Each imported profile supplies `native_model_id`, an explicit selected llama.cpp implementation, a verified runtime bundle, manager-only argument bindings and a managed artifact. It omits `options`. Imported settings own the derived launch options. Public model names must have no surrounding whitespace or commas, because the engine receives each as one literal alias.

The selected execution subset is registered `user.*` chat GGUF models using llama.cpp CPU, 1 to 4096 explicitly declared physical slots and positive integer context settings. Slot count binds the launch identity without resizing the configured total context budget; each serving capacity requires engine observation. Snapshot mutation remains restricted to one actual slot. Nonempty helper/jobs/config state, custom arguments, unsupported settings, architecture overrides and unresolved model companions block execution. Import preserves valid unknown metadata without granting it execution authority. Other native recipes and broader settings remain required future work.

The command inspects six fixed legacy documents, with aliases included only through `--include-native-aliases`. It checks presence, original bytes and permissions, duplicate keys, bounded structure and retained source observations. Limits are 16 MiB per document, 64 MiB per cohort and a 30-second cooperative phase deadline. A modern native store or migration marker rejects. The supplied copy must already be consistent. Inspection does not quiesce the source or establish a writer fence.

On Unix, owner-only private generations and a durable preparation intent precede configuration activation. Schema 3 stores the sole active descriptor, retaining original and current immutable views. Reads never select a newer intent. Restart verifies every active blob. Profile-2 context edits use the same revision guard and publication owner, preserve unrelated rows and original bytes, and support clearing an override back to the registered model setting. Configuration publication survives caller cancellation and keeps mutation guards until disk and memory agree. Uncertain completion pauses new work and retains ownership.

The private store currently limits physical retention to 256 MiB, 256 files and 32 preparation intents. Failed or uncertain candidates remain charged. There is no automatic pruning or complete-generation backup/restore yet. Windows private-store import is unsupported pending native ACL qualification. This explicit-copy subset does not qualify native transport, helper replay, modern cache relocation or previous-binary restore.

## Selected protected host admission

`admission.memory` optionally selects `mode: "protected"`, a declared host `capacity_bytes`, `essential_headroom_bytes` and `maximum_sample_age_ms`. The default remains legacy behavior. Protected mode takes budget authority from the host pool and resource profile. The retained legacy coarse budget fields do not impose a second ceiling. A null capacity is unknown. A configured ceiling does not replace an actual complete host-memory observation. Linux sampling checks MemAvailable, visible cgroup v1/v2 ancestors and process limits. Unsupported or ambiguous constraints remain unknown. Other platforms currently refuse protected work.

The initial protected execution subset is an imported registered CPU llama.cpp chat model, a managed artifact, an explicit runtime bundle and a launch slot count matching the scheduler pool. Its separate `resource_profile` binds the exact launch-plan fingerprint, server-owned priority, phase memory ceilings, a completion deadline and explicit text input/batch/output limits. Changing launch settings invalidates that binding. These are declared ceilings, not measured model-fit or quality receipts.

Load peaks, resident ceilings and incremental request claims share the existing admission owner. CPU and UMA allocations cannot create separate copies of the host capacity. A peak becomes resident only after readiness; cached pre-load observations are invalidated before another grant. Queue waits refresh observations outside the admission lock. Protected text normalizes bounded Chat, Completions and Responses envelopes. Client fields or headers cannot promote the server profile's priority.

Ingress reserves parser/transport headroom before parsing and retains it through response completion. A complete original engine reply releases request claims. Interrupted transfers keep their exact generation's slot, power and memory claims. The console exposes cleanup instead of claiming the engine stopped. Explicit cleanup confirms child exit and preserves saved policy. Client disconnect never triggers automatic text-worker reload.

Protected state save/restore and live context inspection currently refuse dispatch. Existing snapshots remain readable and retained. Legacy cache continuation remains separately tested. Accelerator pools, other providers, broader concurrency, complete state lifetimes, measured resource profiles and interference qualification remain required future work. Do not treat this selected host boundary as full R2 acceptance.

## Selected speech worker and runtime bundles

The `whisper-cpp` provider implements one physical transcription slot and only `audio/transcriptions`. Its arguments must explicitly bind `127.0.0.1`; translation launch flags are rejected. Send multipart `model` and `file`, with optional `language`, `temperature`, `prompt` and `response_format`. JSON, verbose JSON, text, SRT and VTT are supported. Duplicate and unsupported fields reject before loading. Every upstream request resets all four settings, including an empty prompt. The public model ID and caller credentials stay with the manager. A fully received original worker reply settles its physical request. Timeout, disconnect or incomplete response instead retires that exact worker generation, retaining its slot and power claim until exit is confirmed. Saved loading policy stays unchanged; a later request starts a fresh generation. Cleanup failure keeps the claims and exposes an explicit retry. This is separate from cooperative cancellation timing inside the engine.

An optional `runtime_bundle` manifest binds a selected implementation to version 1, an external bundle root, relative entrypoint, explicit library directories and every regular file’s SHA256/role. Loading checks bounded complete inventory and hashes, rejects links or unlisted files, and requires the configured command to match the entrypoint. Loader paths apply only to that child. On Linux, observed bundled executable modules must match the manifest. Package bytes join the execution fingerprint and are rechecked for cache operations. This external-bundle validation is not an atomic package installer, immutable promotion or platform qualification.

The repeatable real speech check acquires the retained pinned CPU archive and tiny model with exact hashes, keeps companion libraries together and uses the existing WAV:

```bash
bun scripts/qualify-speech.ts
```

Receipts under `local/verification/speech/` retain identities, response shapes, lifecycle and cleanup evidence. They do not retain transcripts or judge words. This lane currently selects Linux x64 with the published archive’s AVX2 requirements.

## Authentication and routes

`LEMONADE_API_KEY` and `LEMONADE_ADMIN_API_KEY` remain supported. Bearer and `x-api-key` normalize to the same credential context. Operational inventories and operator mutations require the admin key when configured; otherwise the regular key retains existing operator behavior. Without keys, trusted-LAN mode remains available. Model options and discovery definitions remain readable with an inference credential. HEAD inherits the corresponding GET authority, including parameterized job paths. Static console assets are public so Connection settings can open.

`GET /v1/juiced/endpoints` lists every implemented method, authority and prefix. The strict process-test matrix fails when a new Rust route lacks a scenario. The separate [native API inventory](api-parity/README.md) keeps missing routes and semantic differences visible; published-route coverage is not native API parity. Native routes use `/api/v0`, `/api/v1`, `/v0` and `/v1`. Protocol-mandated fixed paths are explicit exceptions.

| Family | Implemented operations |
| --- | --- |
| Native reads | Health, model list/detail/catalog, state, requests, protocols, endpoints, admission, suitability, context, model options and read-only runtime definitions/alias resolution |
| Diagnostics | Operator-only `system-stats` sampling and bounded `juiced/logs` service/worker output |
| Model operations | Disabled registration, policy CAS, load/unload, unloaded typed-option CAS and registration removal retaining weights |
| Artifact store | Inventory, bounded SHA256 pull, job inspection/pause/resume/retry/cancel, confirmed terminal-history forgetting and reference-safe weight removal |
| Slot snapshots | Explicit cache opt-in CAS, scoped inventory, save/restore/forget and bounded operator inspection |
| OpenAI | Chat completions, completions, Responses and embeddings through one forwarding owner; bounded selected whisper.cpp multipart transcription |
| Anthropic | Fixed `/v1/messages` and `/v1/messages/count_tokens`, selected text/tools/SSE and native token counts |
| Ollama | Fixed inference, embedding and discovery subset under `/api/*` |
| MCP | Stateless JSON `/mcp`, explicit-model inference tools and credential/session-scoped cancellation |

JSON bodies are bounded to 2 MiB; multipart transcription is bounded to 8 MiB including overhead. A shared inference-input budget defaults to 128 MiB and reserves each endpoint’s maximum before reading a body. Full capacity returns 503 before accepting another application body or loading a worker; disconnects release the claim. This bounds admitted wire-body capacity, not total process memory. Operator controls and health reads remain available. Original OpenAI body bytes are forwarded without a full parse/reserialization. Response-body lifetime owns upstream cancellation and the request slot. No caller credentials are forwarded. Metadata records exclude prompts, responses and keys.

`GET /v1/system-stats` keeps the native `cpu_percent`, `memory_gb`, `gpu_percent`, `vram_gb`, `primary_gpu_id`, `gpus` and `npu_percent` fields. One background sampler starts on the first read, refreshes every 2 seconds and stops after 60 seconds without reads or at shutdown. It reads `/proc/stat` deltas, `/proc/meminfo`, NVIDIA GPUs through `nvidia-smi` with a 1-second deadline and 64 KiB output limit, and AMD GPUs through amdgpu sysfs. A runtime-suspended AMD GPU is not woken. The response also reports active and queued requests and `last_inference`: token counts and decode/prompt rates from the last completed chat, completion or Responses request whose engine reported `usage` or llama.cpp `timings`. Unknown values are null. NPU utilization is not observed. Other platforms report `supported: false`.

`GET /v1/juiced/logs?after=SEQ&limit=N&source=ID` pages one in-memory ring of service lines and worker stdout/stderr. The ring keeps 4096 lines and 8 MiB; each line is limited to 4 KiB and marked `truncated` when clipped. Dedicated reader threads drain each worker pipe continuously, so a full ring drops its oldest lines instead of blocking the engine. Pass the returned `next` as `after`. `dropped` counts lines that left the ring before the cursor reached them. `source` is `service` or a model ID; `limit` defaults to 200 and allows at most 500. Request bodies are not logged, but worker output is retained as the engine printed it.

Admission queues default to 64 shared pending and 16 pending per service, with configurable 2/10/30 second essential/interactive/batch waits. Physical slots and the resident budget remain separate. No principal or conversation quota is enabled by default. Native `X-Juiced-Priority` accepts interactive or batch; essential requires operator authority. Pause rejects new inference and clears waiting requests while preserving active work and policies. Queue expiry is 504; bounded overflow is 429. Resident pressure does not evict other workers.

## Rust CLI and discovery

Build the management client with `cargo build -p juiced-service --bin lemonade --locked`. Use an explicit isolated origin:

```bash
./target/debug/lemonade --url http://127.0.0.1:13306 --json status
./target/debug/lemonade --url http://127.0.0.1:13306 list
./target/debug/lemonade --url http://127.0.0.1:13306 load my-model
./target/debug/lemonade --url http://127.0.0.1:13306 policy my-model on-demand --revision 0
```

`lemonade --help` describes typed management commands, bounded JSON file/stdin inputs, revision guards and exit codes. The client prefers the admin environment key, then the regular key. It never starts or kills the independent daemon. Registration removal retains weights.

Daemon discovery is opt-in with `lemond --broadcast` during the rewrite. It sends the retained native service/hostname/URL datagram to IPv4 loopback UDP port 13305 every two seconds. CLI scan/discovery is read-only. Beacons are unauthenticated: configured credentials require an explicit origin/host/port, and credentials are never sent to discovered candidates. Native wire compatibility does not grant another manager's lifecycle authority.

The [runtime catalog](catalog-design/DESIGN.md) is discovery and migration evidence. Its 69 definitions preserve 18 recipes/44 native rows and list proposed additions separately. It cannot install or enable an engine. `system`, recipe-scoped `npu` and hybrid placement remain unresolved until exact bundle/device bindings exist; candidate compute paths do not establish support.

## Artifacts and cache

Public HTTPS is the acquisition default. URLs with credentials, query secrets or fragments are rejected; DNS targets and redirects are checked. Explicit `artifact_source_policy` can select `offline` or `allow-loopback-http` for controlled local tests. Each pull needs an expected SHA256 and an explicit byte bound. Interrupted transfers retain their journal/charges. Generated staging names and reservations are journaled before staging creation. A root failure during transfer blocks later writes, truncation, checkpoints and publication; uncertainty retains its charge. Restart exposes unclaimed generated files as quarantined observations with conservative reservations, without assigning artifact identity or authorizing deletion. A confirmed complete or cancelled job can be forgotten without deleting artifacts; uncertain writers cannot be forgotten as cleanup.

`GET /juiced/artifacts` returns one page per section: 64 rows by default and at most 128 (`limit`). Continue a section with `jobs_after`, `artifacts_after`, `artifact_sets_after` or `quarantined_files_after`, together with the `expected_view` from the earlier page. A changed set of jobs, files or sets returns 409; restart from the first page. `section` restricts the response to one section. `artifact` and `artifact_set` return those rows under `selection` even when they are on another page. References, source URLs and errors are previews; `reference_count` and `source_url_bytes` give full sizes. A page with maximal rows serializes below the 6 MiB response bound. Protected mode reserves 12 MiB of controller memory before reading.

Managed registrations use an artifact ID and retain durable references plus validated worker holds. Removing a registration retains weights; only unreferenced weights can be removed. This single-file pipeline does not establish full repository companions, model-format validity or runtime compatibility.

Linux snapshots bind the resolved profile, model/executable content, bundle, captured child environment and observed executable/shared-library/kernel closure. Model content and runtime closure are checked again before and after save/restore. This introduces a new execution-fingerprint namespace: older snapshots remain retained and inspectable but cannot restore into a different identity. Mutations reject without automatic reload; uncertain saves retain their bytes and charges. This does not establish complete device/driver or non-executable companion identity. Retained file handles and metadata/hash checks do not freeze external bytes against other writers. These checks also add startup/cache I/O; immutable package ownership and production-scale measurements remain open.

Persistent snapshots are off by default. Enable the cache explicitly, configure one actual engine slot and a slot-save argument containing `{cache}`, and complete a `/completions` request with `X-Juiced-Session`. Saving/restoring requires the same principal, credential epoch, opaque session, protocol and execution fingerprint. Engine acknowledgements and checksums must confirm the operation. Uncertain writes remain charged; inspection never grants repair authority. Multi-slot ownership, other protocol cache publication and full engine-family qualification remain open.

## Offline snapshot inspection

Inspect an existing Rust snapshot directory without starting the service or acquiring a writer claim:

```bash
./target/debug/lemond --inspect-snapshot-store /path/to/rewrite-cache --inspect-timeout-ms 5000
```

The command prints one bounded JSON report and exits 0 only when validation completed without issues; partial, unknown or inconsistent observations exit 2. A missing directory remains missing. Inspection does not create control files, repair snapshots, remove bytes, release charges or authorize recovery. The explicit directory argument refers to the Rust cache format, not native C++ snapshot state.

Reports are read-only, non-atomic observations with record, file, issue and hash-byte limits. Cancellation and deadlines are cooperative between filesystem operations; blocked kernel I/O cannot be preempted. Concurrent writer activity can leave results unknown. Atime changes are possible on the host filesystem.

## Quick verification and profiling

```bash
bun scripts/quick-check.ts
bun scripts/quick-check.ts --full
JUICED_TEST_POWER_INHIBIT=1 bun scripts/quick-check.ts --power
JUICED_TEST_LLAMA_SERVER=/path/to/llama-server JUICED_TEST_GGUF=/path/to/tiny.gguf bun scripts/quick-check.ts --real
```

The quick lane builds current executables, runs native Rust contracts and `rust-tests` actual-process/socket pipelines, then independent Bun UI tests and plan checks. `--full` adds formatting, lint, production build and browser journeys. `--real` adds tiny-model inference, cache, metadata, embedding batches, Chat/Responses JSON and SSE, and selected Anthropic/Ollama/MCP data pipelines. Assertions concern protocol structure, finite vectors, native counters, identity and lifecycle, never prose quality. Install Chromium with `bunx playwright install chromium` in `ui/`, or select an installed executable with `JUICED_TEST_CHROMIUM`.

On Linux, `--real` also tests two actual workers sharing one installed runtime package and model file. Promoting and removing one registration must preserve the other worker and its launch identity. The survivor restores its own snapshot after restart. Cross-model snapshot restore stays refused even when session and package match. The quick lane exercises the same contract with synthetic workers.

`--speech` runs the real Rust speech tests with `JUICED_TEST_WHISPER_SERVER`, `JUICED_TEST_WHISPER_MODEL` and `JUICED_TEST_WHISPER_BUNDLE` pointing to verified assets and the bundle JSON. On Linux it also imports two exact runtime packages, removes the temporary import source, and exercises all-prefix JSON/text transcription across promotion, restart and rollback. It verifies package/launch identity and preserved inputs without judging transcription quality. The ordinary quick suite exercises the same managed speech lifecycle through a synthetic subprocess without model assets. It can be combined with `--real --full`.

`--power` runs positive OS-inhibitor and daemon-lifecycle checks. It requires Linux, accessible logind and `JUICED_TEST_POWER_INHIBIT=1`. An unavailable inhibitor fails that explicit lane. The ordinary quick lane uses controlled actor tests and verifies disabled mode without requiring host power-management services.

Each run writes a source-bound receipt to `local/verification/latest.json` and reports concurrent source changes. CI separately configures Linux, Windows and macOS process checks; configured CI is not native-platform acceptance evidence.

```bash
cargo run --release -p juiced-service --example allocations
bun scripts/profile-api.ts
JUICED_BASELINE_LEMOND=/path/to/native/lemond bun scripts/compare-api.ts
```

Profiling is optional Unix-only instrumentation at 199 Hz, enabled by `cpu-profile` and `JUICED_CPU_PROFILE`. Artifacts live under ignored `local/performance/`. Allocation measurements cover routing and successful retained-root verification, counting Rust allocator calls and requested bytes; libc allocation, live memory and syscall counts are outside that counter. Health benchmarks cover manager HTTP only. Payloads and feature scopes differ, so health results do not establish overall inference or release gains.

`scripts/profile-inference.ts` measures the selected Stories15M CPU completion pipeline and produces a manager CPU flame graph. Set `JUICED_TEST_LLAMA_SERVER` and `JUICED_TEST_GGUF` to the retained engine and Stories15M Q4_0 model. `JUICED_BASELINE_LEMOND` optionally adds a frozen native executable with its adjacent resources. `JUICED_REFERENCE_RUST_LEMOND` optionally adds a frozen Rust executable for alternating reference/current pairs. Both Rust versions use matching configuration values, runtime input checks and positive enabled-power observations. Binary hashes, response-shape checks and cleanup evidence accompany the receipts. Model text is discarded. These selected measurements do not establish significance or complete release parity.

## Open release gates

The remaining work includes portable complete runtime/device identity and immutable bundle promotion, consistent-generation backup/restore, complete packages/companions/private acquisition, physical memory pools/UMA and specialist services, routing/access policy, Barista's guarded tools and actual-model qualification, native CLI/tray/discovery, portable packaging and prior-release migration. Full final T3 browser inspection and the communicating release council follow the implementation gates.

On Linux, readiness verifies the child generation, actual executable/module hashes and ownership of the private IPv4 loopback listener. These are process observations, not hardware or model qualification. Native module/listener observation is unavailable on other platforms, which cannot publish persistent snapshots through the current identity gate. Linux tests do not qualify Windows/macOS hierarchy, network filesystems, device support or host-crash durability. Managed startup and load hashing still need production-scale I/O measurements. No deployment, production migration or release cutover has occurred.

Visual provenance remains in [T3-SOURCES.json](../ui/T3-SOURCES.json). Builds retain T3 and Inter license/source metadata. The repository's Apache-2.0 license remains in effect.


## Model revision changes

A registration bound to a primary-only managed set can select another complete set in the model inspector. Unload the worker first. Juiced preserves the public ID, policy and engine settings, and retains one previous set for rollback. Recorded content does not establish engine compatibility. No model loads as a side effect. Another client's model change keeps your selection and requires an explicit refresh before saving.

`GET /v1/models/{model}/artifacts` observes current and previous bindings. `POST` at that path takes `expected_revision` and either `change: {"action":"promote","artifact_set":"set-…"}` or `change: {"action":"rollback"}`. All four native prefixes require operator authority. These changes cannot bypass native-settings or protected resource authority. Configuration with rollback history requires this newer rewrite binary. Runtime packages, companion execution and complete previous-release restore remain separate work.


## Import a local runtime bundle

Stop the isolated daemon, then run:

```bash
./target/debug/lemond --config local/config.json --import-runtime-bundle local/runtime-import.json
```

The specification contains `schema_version: 1`, an existing complete `bundle` descriptor, and `files` entries with each relative path and exact byte count. `provenance` requires builder/source/build labels, `declared_licenses` and optional `declared_sbom`. `declared_target` records OS, architecture, ABI and compute-path labels. These declarations do not qualify the package. Relative source roots resolve beside the specification. Every source file must be listed with its role and SHA256. Only selected llama.cpp and whisper.cpp bundle identities are accepted.

The command reserves complete payload bytes before copying into `runtime-packages/package-<digest>/payload`. It streams data through a 64 KiB buffer, checks actual target hashes and seals completed files/directories. The manifest is bounded to 4 MiB, 32 packages and 4 GiB of declared payload bytes. These are storage declarations, not memory or disk-peak measurements. Existing installed versions are rechecked without modification and need no original source. Matching complete files can be reused. A partial or conflicting file stays retained and refuses overwrite. Preserve incomplete package state for inspection rather than editing its manifest to release reservations.

The JSON receipt returns the installed `package` ID and a verified `bundle`. A canonical configuration version 2 local execution profile can select `runtime_package` instead of supplying `command` or `runtime_bundle`. The package owner derives those fields from the exact installed record. Preserve the adapter, arguments, options, placement and model registration fields. Import does not alter configuration, enable/load a model or contact a network source. Network package acquisition, reference-safe package deletion, full writer fencing and Windows/private-platform acceptance remain open. Settings capsules continue to exclude runtime packages.


## Select and roll back an installed runtime

`GET /v1/juiced/runtime-packages` observes the installed/staging package records and their declared provenance/target labels. Staging packages cannot be selected. The `declared_host_match` field compares recorded OS/CPU labels against the service binary target. Mismatched declarations remain visible but cannot be selected or bound at startup. Canonical architecture names and explicit x64/amd64, arm64 and ia32/i386/i686 aliases compare without rewriting the recorded labels; darwin/win32 OS aliases are recognized. This precheck does not inspect binary ABI, drivers or hardware.

The per-package `binding_leases` observation counts distinct verification attempts and retained bindings in this manager. Current/rollback profiles and worker resources retain those bindings. Cloning one binding does not multiply the count. It changes without a saved index-generation change and is never written to the package manifest. A zero observation alone does not authorize deletion or establish that external actors have stopped.

`GET /v1/models/{model}/runtime` reports the current and previous managed package IDs, model revision, saved policy and worker-in-use guard. `POST` at that model path accepts `expected_revision` and `change: {"action":"promote","package":"package-<digest>"}` or `change: {"action":"rollback"}`. All four native prefixes require operator authority.

The worker must be unloaded with no active/preparing generation. Local llama.cpp and whisper.cpp profiles can select an installed matching adapter. Native-owned settings, generic HTTP workers and protected resource profiles require their own authority and reject this operation. First selection creates no managed rollback history. Later promotion retains one previous complete managed package; rollback swaps the two. Model identity, weights, artifact history, launch arguments/options, physical slot count and loading policy are preserved. Selection does not start a worker. Hot policy can start its worker at the next service startup.

Canonical managed profiles omit derived commands and bundle paths. Startup verifies both selected and retained packages before assembling workers. Profiles and launch plans retain package directory/manifest ownership through actual preparation and process lifetime. A package ID joins the semantic execution fingerprint, so even identical executable bytes under a different declared package cannot reuse the prior computation snapshot. Rolling back restores that exact identity if all other inputs still match. Mutations use the shared atomic configuration publication owner; caller disconnection does not release actual publication work. Unknown commit outcomes retain the saved head for recovery and pause further mutations.

The inspector exposes installed runtime selection beside model revisions. A selection stays tied to its starting model revision. Another client's change preserves the draft and requires explicit refresh before submission. Recorded labels remain unqualified; import, selection and rollback do not establish hardware, ABI, licensing or inference quality support. Package acquisition/reclamation, complete companion execution, outside-writer fencing and prior-release restore remain separate acceptance work. Configuration with managed runtime selections requires this newer rewrite binary.


## Import while serving

In Backends & hardware, use Installed runtimes to review packages and import a local bundle. Enter the absolute specification-file path on the connected service host. This form does not upload files from the browser. Import preserves model registrations and policies. Select an installed package separately in the unloaded model inspector.

The equivalent operator request is `POST /v1/juiced/runtime-packages/import` with `{"specification_file":"/host/runtime-import.json","expected_generation":2}`. Read the generation from `GET /v1/juiced/runtime-packages`; all four native prefixes are available. If another client changes that generation, review and explicitly refresh before retrying. Installed retries verify retained payloads and need no original source. Interrupted or conflicting payloads retain their reservation and require inspection.

The `installation_active` observation reports the current import owner and changes independently of the saved generation. Accepted installation continues after the requesting client disconnects. Refresh the inventory before retrying an unknown result. New imports refuse while admission is paused or the service is stopping. Protected-memory mode admits local runtime installation only when the shared host ledger has fresh capacity for runtime metadata and copy work. Diagnostics separates persistent runtime metadata, retained bindings and work/results from worker reservations. Scratch-capacity refusal precedes specification reads. Metadata capacity must be admitted before package creation or index reads. Runtime metadata remains charged after imports and worker unload; bindings release with their final actual owner. These layout ceilings do not measure process RSS or worker peak memory. Network package acquisition, reclamation, complete recovery and platform qualification remain open.
