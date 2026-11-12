# Runtime catalog design for the Rust rewrite

Status: proposed architecture and integration scope, November 10, 2026.
This design is an input to the [rewrite plan](../PLAN.md). It does not change
current release support or qualify any new engine, model, device, or platform.

Juiced should describe each runnable model through an exact implementation,
runtime build, artifact set, compute path, and placement plan. The catalog should
make those relationships inspectable before installation and preserve them
through load, inference, cache restore, upgrades, and diagnostics.

The [integration list](INTEGRATIONS.md) groups the proposed catalog entries by
delivery treatment. [inventory.json](inventory.json) owns the maintained list.
It contains 18 native migration mappings and separately prioritizes new engines,
frameworks, independently managed services, browser runtimes, and legacy entries.
Its candidate backends are discovery leads, not installable support rows.

## Scope and compatibility with the rewrite

The [platform plan](../../PLATFORM-PLAN.md), [feature dispositions](../../PLATFORM-FEATURES.json),
[profiles contract](../../RUNTIME-PROFILES.md), [capabilities contract](../../CAPABILITIES.md),
and [resource contract](../../RESOURCE-ADMISSION.md) retain their authority.
A catalog entry does not promote a conditional feature into the required release
or demote an existing requirement. The product's retained operations and explicit
exclusions still determine which integrations ship.

Keep inference in subprocess workers. A Rust, C++, Python, Swift, or other
implementation can be packaged behind that boundary. Using a Rust library such
as Candle does not permit loading models inside the Rust daemon.

Keep weight conversion, quantization, fine-tuning, calibration, repacking, and
model export outside the appliance. Acquire existing compatible artifacts.
Runtime-owned compiled execution caches are permitted only under the existing
selected-provider contract. A catalog adapter must not quietly add a producer
when its upstream quickstart begins with conversion.

This work owns new files in catalog-design/ and its task record. The active
rewrite implementation, generated native descriptors, existing feature
crosswalk, release ledger, and shared migration plan remain under their current
owners. These files are ready for those owners to incorporate without changing
the current experimental configuration.

## Observed starting point

The inspected Rust config has Provider::ExternalHttp and Provider::LlamaCpp.
ModelSpec combines a model file, executable/arguments, provider, launch options,
resident reservation, slots, operations, policy, and revision.
[config.rs](../service/src/config.rs) owns those definitions.
[profiles.rs](../service/src/profiles.rs) resolves llama.cpp launch options.
[supervisor.rs](../service/src/supervisor.rs) owns workers and their leases.

This is a useful first implementation. It cannot represent every integration by
adding another provider enum variant and another unstructured argument string.
An ONNX execution provider, an ASR model family, an external service, and a GPU
API have different identities and lifecycle authority.

The inspected native [backend reference](../../BACKENDS.md) declares 18 recipes
and 44 support rows. Its declarations are migration evidence, not proof of Rust
behavior. The inventory preserves each original backend label and support row,
including tier and platform constraints. The native reference file hashes bind
this observation while the main checkout continues to change.

Two corrections to a generic runtime inventory matter for migration:

- The native asr4all recipe uses FUTO's GitLab transcribe.cpp project. Its wrapper
  forces CPU execution and the ASR4ALL architecture. It is separate from
  handy-computer/transcribe.cpp. The [container pin](../../Dockerfile) and
  [FUTO wrapper](../../runtime/futo_server.cpp) establish this relationship.
- Native kokoro means the selected Kokoros implementation. Native parakeet means
  the pinned Parakeet Redux implementation. Their model family names do not make
  other ONNX, MLX, Core ML, or NeMo implementations interchangeable.

## Catalog identities

| Record | Owns | Example |
| --- | --- | --- |
| Runtime project | Stable origin, lineage, upstream lifecycle, canonical name | ggml-org/llama.cpp versus ikawrakow/ik_llama.cpp |
| Implementation | Concrete model runner, wrapper, decoder and pipeline code | faster-whisper over CTranslate2; Kokoros versus kokoro-onnx |
| Runtime bundle | Immutable binaries, dependencies, target, adapter compatibility and provenance | Exact llama-server build plus GGML libraries; exact MLX worker environment |
| Model artifact set | Immutable weights, tokenizer, configurations and required companions | GGUF plus matching projector; Piper voice plus voice configuration |
| Execution profile | Operator-selected implementation, bundle, artifacts, devices, settings and policy | llama.cpp HIP on selected AMD GPU with explicit CPU offload |
| Resolved execution | The exact launch-applied configuration and observed worker generation | Actual engine version, device UUIDs, stage placement, slots and context |
| Capability claim | A scoped statement and its evidence source | Upstream-declared tool calling, locally probed slots, qualified cancellation |
| Qualification receipt | Functional evidence for one selected combination and test suite | Exact engine/artifact/backend/platform/operation result |
| Service connection | Endpoint, credential handle, locality and lifecycle authority | Explicitly configured Ollama or LM Studio service |

Use stable string IDs for projects and implementations. Validate them against a
versioned registry. Use closed Rust enums for settled domain concepts such as
operation, policy, lifecycle state, and evidence kind. Avoid encoding the whole
upstream project catalog as one ever-growing Rust enum.

An implementation can depend on several runtimes or frameworks. Describe those
dependencies in its bundle manifest. TEI can use Candle, and a Ryzen AI OGA
implementation can use ONNX Runtime GenAI and vendor libraries. A dependency does
not automatically create another worker, model copy, or resident reservation.

Different origins with the same display name require different IDs. Preserve
model-format generations such as EXL2 and EXL3. Renames receive explicit aliases
with a migration record. Aliases never select a new implementation on their own.

## Compute paths and hardware

Represent a compute path using ordered, typed components. The path names how the
model implementation reaches the selected hardware. It is scoped to that
implementation and bundle, not a universal list of interchangeable accelerators.

| Selection | Runtime path | Hardware selection |
| --- | --- | --- |
| llama.cpp ROCm | llama.cpp -> GGML HIP | Selected AMD GPU ISA and UUID |
| MLX-LM on Mac | MLX-LM -> MLX -> Metal | Selected Apple GPU and shared memory pool |
| ONNX on Intel NPU | Named ONNX worker -> ONNX Runtime -> OpenVINO | Selected Intel NPU |
| Snapdragon language worker | Named implementation -> QNN/Genie -> HTP | Exact supported Snapdragon NPU |
| Whisper encoder offload | whisper.cpp -> Core ML for encoder; CPU/Metal for remaining stages | Allowed and observed Apple compute units |
| FLM | FLM -> its selected XDNA2 execution stack | AMD NPU and supported prepared model |
| Ryzen AI hybrid | OGA implementation -> selected vendor stack | NPU plus iGPU with explicit stage/phase placement |

Use the inventory's backend IDs as scoped vocabulary. In particular:

- cpu is a compute choice. AVX2, AVX-512, AMX, NEON, SME and CPU architecture are
  requirements or optimizations. MKL, OpenBLAS, oneDNN and Accelerate are
  implementation dependencies/options.
- metal, mps and coreml identify different execution integrations. MLX is a
  runtime/framework, and Apple Neural Engine is hardware.
- rocm is a retained native/configuration label. A resolved HIP path records its
  actual ROCm/TheRock libraries, ISA and driver requirements.
- npu is a hardware category. Resolve legacy npu through the original recipe and
  exact vendor stack. It cannot become a universal compute implementation.
- system is installation provenance. Resolve the installed executable's actual
  build and compute path separately.
- hybrid-rocm describes an existing implementation and placement choice. Preserve
  its pinned hybrid fork and distinguish it from ordinary upstream HIP execution.
- hrx identifies the retained distribution-specific integration. Keep its exact
  implementation identity and restrictions.
- Google TPU and Coral Edge TPU require different IDs and artifact contracts.

Describe each device by class, vendor, stable instance ID, architecture, host
OS/architecture, available runtime/driver observations, and physical memory pool.
Driver version, free-memory reading and device availability have timestamps and
provenance. A failed probe leaves an unknown value, not zero or unlimited capacity.

Placement is explicit: CPU-only, accelerator-only, layer offload, expert offload,
stage split, hybrid NPU/GPU, multi-device, or multi-host. Store selected devices
and actual stage allocations. Do not call an NPU encoder profile full-model NPU
execution. Discovery of a Vulkan device does not qualify every Vulkan model.

## Support has independent dimensions

Never persist a single supported boolean.

| Dimension | States / information |
| --- | --- |
| Product disposition | Required/retained, conditional selected-provider, excluded |
| Upstream lifecycle | Active, maintenance, archived, unknown |
| Adapter implementation | Planned, partial, implemented |
| Package availability | Missing, externally installed, installed verified, invalid |
| Hardware suitability | Compatible, incompatible, unknown, with reasons |
| Artifact compatibility | Compatible, incompatible, unknown for the selected importer |
| Worker observation | Stopped, starting, ready, failed, stale, with generation |
| Operation/features | Unsupported, unknown, declared, probed, qualified, with evidence |
| Execution policy | Disabled, On demand, Hot; independent of the above |
| Qualification | Receipt and exact scope, expiry/revalidation conditions, or absent |

A probe can establish a loaded device or supported slot count without proving
schema-constrained tool calling, cancellation, or persistent cache portability.
Claims retain their source. Conflicting observations remain visible, and stale
evidence cannot authorize a new combination.

Qualification receipts identify their evidence class: functional correctness,
resource behavior, performance, or output quality. A functional receipt does not
assign a quality ranking or satisfy the comparison/assistant quality gates.

Inventory task names are discovery tags. They do not create new API operations.
Map image-understanding to an existing selected multimodal operation, and map
tool-calling/structured-extraction to features of an appropriate text operation.
Additional fixed speech or media services need an explicit operation contract.

For admission or dispatch, evaluate the operation and requested features against
the selected profile, artifact set and actual worker. Use structured reason
codes such as artifact_architecture_unknown, missing_companion,
backend_not_in_bundle, driver_requirement_unmet, worker_observation_stale,
operation_unavailable, and external_lifecycle_unmanaged.

Candidates in inventory.json start without qualification receipts. Existing
native metadata remains separately labeled. An installed bundle does not qualify
all architectures, quantizations, contexts, voices, image sizes, or media formats.

## Artifact and package contracts

A selected artifact set declares roles, content digests, source revisions,
importer identity/version and compatibility constraints. Required roles depend
on the implementation and model architecture.

| Family | Common roles requiring explicit treatment |
| --- | --- |
| GGUF text/vision | Main shards, tokenizer metadata, matching projector, optional draft model |
| Hugging Face model repository | Weight shards/index, config, tokenizer, processor, generation settings |
| ONNX/OGA | Graphs, external tensor data, tokenizer, prepared-model config, provider context where required |
| Speech recognition | Acoustic/decoder/encoder artifacts, tokenizer, vocabulary and language metadata |
| Speech synthesis | Model, voice/style data, phonemizer dictionaries, speaker/voice configuration |
| Diffusion/media | Denoiser, encoders, VAE, scheduler/config, tokenizer, selected adapters |
| Prepared mobile/vendor model | Target-specific artifact, compatible runtime/delegate, required companions |

Do not infer compatibility from a filename extension. GGUF is a container, ONNX
is a graph format, safetensors stores tensors, and an MLX model requires an
implementation-specific structure. They do not prove architecture or operation
support.

Treat LoRA/adapters, projectors, draft models and voices as role-bound artifacts
with their own digests and compatibility. Attaching existing adapters does not
authorize producing or fine-tuning weights.

A runtime bundle records upstream revision, packager origin, dependency lock,
binary digests, OS/architecture/ABI, included compute implementations, adapter
contract version, licenses and SBOM/provenance. Pin optional acceleration plugins
with the bundle. vLLM-Metal, Ascend plugins and vendor libraries are not implied
by the base project version.

Keep model revision and runtime bundle promotion independent. Stage and verify
the complete replacement before publication. A worker holds leases on all
resolved artifacts and its bundle until exit. Rollback restores a compatible
complete set and preserves public model IDs, policies and previous references.
Retain failed/partial jobs under the existing bounded recovery rules.

## Runtime options

Use common typed fields only when their semantics are shared: request deadline,
selected devices, policy and a declared operation. Context and parallelism need
explicit provider semantics.

Provider-owned option schemas carry names, types, units, bounds, defaults,
mutability, required features and the versions to which they apply. A schema
does not invent values for a runtime that lacks the corresponding concept.

Preserve declared, saved, launch-applied and observed values separately.
A llama.cpp total context allocation is not necessarily per-slot capacity.
Audio seconds, image dimensions, denoising steps and vocabulary hints do not
belong in a universal text context field.

Reserve all arguments that control model paths, bind addresses, ports, device
selection, store roots, credentials and managed lifecycle. Resolve argv without
a shell. Keep advanced provider arguments only through the existing scoped
validation policy. Inference requests, catalog records and Barista model output
cannot supply executables, arbitrary environment maps, or package commands.

## Rust ownership and adapter contract

Evolve the current concrete modules. Add a module when it owns an external
integration or an operation actually shared by callers.

| Owner | Responsibility |
| --- | --- |
| Catalog module, proposed | Definitions, aliases, schema/version validation and immutable lookups |
| Bundle/package owner | Acquisition, verification, installed-build identity, promotion/rollback and leases |
| Existing artifact owner | Artifact sets, companions, references and content identity |
| Profile resolver | Effective options, compatibility evaluation and immutable execution plan |
| Runtime adapter | Selected implementation's argument construction, protocol details and bounded observations |
| Existing supervisor | Spawn/readiness, child lifetime, cancellation wiring and worker generation |
| Existing resource/admission owner | Physical pools, incremental leases, fair queues and exclusive-device rules |
| Existing protocol/API owners | Public protocol codecs, auth, session context and operation dispatch |
| Existing cache owner | Snapshot leases, credential/session scope and execution-compatible restore |

A LaunchPlan is an immutable internal record containing verified executable and
bundle handles, argv, allowed environment handles, artifact leases, device and
pool bindings, stage placement, readiness contract, worker protocol, effective
options and a semantic execution fingerprint.

Resolve returns either that plan or structured blockers. It performs no
installation, process spawn, network handoff, enable, or policy mutation.
Installation and execution are explicit operations with their own authority.

Once a second real engine needs substitution, extract the shared plan/observation
boundary from the existing llama.cpp implementation. Keep fixture adapters tied
to meaningful worker contracts. Do not create a trait hierarchy for every entry
in the research inventory. Initial adapters can remain compiled concrete modules.

If an engine needs a shim, package that shim as a supervised worker. It owns
model-specific decoding and translates the selected private worker protocol.
The daemon continues owning auth, policy, admission, session identity and public
API compatibility. Do not embed a generic Python interpreter or dynamically
loaded inference library in the daemon.

Treat framework composition as a dependency graph within the selected worker
plan. KTransformers inside SGLang is one declared serving plan unless its actual
process topology requires more workers. Account for real processes and shared
artifacts, not the number of names in its dependency list.

## Resolve, install, load and dispatch

1. Resolve a model ID to an immutable artifact set and saved profile revision.
2. Read catalog definitions, installed bundle identity and fresh device evidence.
3. Check the exact importer, required companions, compute path, operation,
   feature requirements and product disposition.
4. Return compatibility blockers and separate resource-fit results. A planning
   check neither downloads a package nor consumes a live inference slot.
5. For an explicit install request, acquire and verify the selected bundle.
   Re-evaluate the plan after installation and detect profile/catalog revisions.
6. For load, acquire ordered artifact/bundle references and resource leases through
   their existing owners. Reject stale plans before spawn.
7. Spawn without a shell, establish readiness, and compare applied/observed engine
   and device identities with the plan. Report planned versus actual placement.
8. Publish a ready worker generation and its scoped capabilities.
9. At dispatch, recheck current authorization, policy, operation/features, model
   identity and worker generation. Hold the exact request lease through completion,
   cancellation, timeout, or transport loss.
10. Release request resources independently of worker residency. Unload only
    through the supervisor's owned lifecycle operation.

Silent engine fallback cannot change a declared accelerator-only profile into
CPU execution. Operator-approved fallback profiles must be explicit, bounded,
observable and independently admitted. Never retry after output has begun.
Planned partial offload is ordinary placement, not a failure fallback.

A failed load or request cannot trigger global model reloads. Preserve the
disconnect guarantee through public protocol codecs to the actual upstream
transfer. Distinguish transfer abort, engine acknowledgment of request abort,
and stopping a dedicated temporary worker. Advertise the precise behavior
established by that adapter's tests.

## Resources, concurrency and cache identity

Reuse the physical-pool owner for RAM, VRAM, UMA and NPU allocations. Avoid
counting the same shared-memory bytes once as CPU RAM and again as GPU capacity.
Observe weights, KV cache, temporary workspaces, compiled caches, staging buffers,
voice/media buffers and subprocess overhead separately where the engine reports
them. Unknown demand remains unknown. File size is not fit.

NPU exclusivity and hybrid NPU/GPU reservation must be coordinated with actual
worker lifetimes. Multi-stage workers reserve their real pool/device needs.
Do not add a separate catalog lock around slow install, load, or inference I/O.

A worker's batch capacity, engine slots, pending queue, conversations and
principals are different concepts. Runtime claims do not impose new per-client
quotas. Preserve the rewrite's J002 session and fairness requirements.

Bind execution identity to implementation/adapter version, bundle digests,
artifact/companion digests, semantic options, backend path, placement, relevant
hardware/driver ABI and effective model/slot configuration. Store a worker
generation separately from that stable semantic identity.

KV or other continuation state also binds to cache format/ABI, tokenizer,
attention/position settings, KV data types and effective context, plus principal,
credential epoch and session ownership. Preserve runtime-declared portability
only when evidence establishes it. A bundle or artifact change invalidates
compatibility assumptions until the adapter proves a safe match.

Keep compiled execution caches, continuation snapshots, answer caches and
download caches distinct. Runtime prefixes/caches are not automatically
exportable snapshots. A receipt for one engine's save/restore cannot qualify
another engine or backend.

## Example profile and evaluation

This proposed shape describes a saved selection. The references are illustrative
handles, not installed artifacts or an accepted experimental config schema.

```json
{
  "model_id": "example-model",
  "profile_id": "example-amd-profile",
  "revision": 1,
  "implementation_id": "llama-cpp",
  "bundle_ref": "example-verified-bundle",
  "artifact_set_ref": "example-gguf-artifact-set",
  "compute_path": [
    {"runtime": "llama-cpp"},
    {"library": "ggml"},
    {"backend": "hip"}
  ],
  "device_refs": ["example-amd-gpu"],
  "placement": {"mode": "layer-offload", "cpu_offload_allowed": true},
  "options": {"context_tokens": 8192, "parallel_slots": 1},
  "policy": "disabled"
}
```

A read-only evaluation can report installed/compatible separately from policy
and physical fit. This example intentionally has no hardware-fit receipt.

```json
{
  "profile_id": "example-amd-profile",
  "evaluated_profile_revision": 1,
  "bundle_availability": "installed-verified",
  "artifact_compatibility": "compatible",
  "resource_fit": "unknown",
  "resource_reason": "pool_observation_missing",
  "policy": "disabled",
  "worker": null,
  "qualification_receipts": [],
  "can_dispatch": false,
  "blockers": ["policy_disabled", "pool_observation_missing"]
}
```

Changing this selection to an ONNX/OpenVINO implementation requires a different
profile and compatible artifact set. Selecting another AMD GPU changes device
binding and resource evaluation. Neither change mutates the stored model ID or
silently reuses an incompatible continuation snapshot.

## External services and client-local inference

Ollama, LM Studio, LocalAI, TabbyAPI, KoboldCpp and ComfyUI should initially be
explicit service connections. Store the service's reported implementation and
model identity separately from a Juiced-managed bundle.

A connection declares authority for discovery, inference, load/unload,
installation and deletion independently. Default to discovery/inference only
unless an exact integration contract grants more. Juiced cannot guarantee
remote residency, cancellation or resource reservation from API compatibility
alone. Never terminate, upgrade, or delete state owned by another manager.

Locality describes where compute runs, independently of who manages it.
Attaching a loopback service is different from managing its worker.
Remote configuration requires the existing egress policy and credential handles.
Shared service infrastructure lives in daemon config. Client URL/API-key
preferences stay in the client, and provider secrets remain environment-backed
or ephemeral memory handles.

Managed local workers keep inference-time telemetry and automatic cloud handoff
disabled unless the existing explicit egress policy authorizes those operations.
Downloading an approved package/model is a separate acquisition operation.

ComfyUI integration, if selected, uses fixed allowlisted workflows with known
inputs/outputs, resource needs and cancellation. Keep generic node/graph
execution outside the product contract.

WebLLM and browser Transformers.js execute on the client's hardware. They are
reference entries for a separately scoped client-inference design. Their
memory and device state must never appear as daemon-owned capacity or move
per-client state onto lemond.

Needle proposes tool calls. Its adapter returns structured proposals. Barista's
typed dispatcher remains the only authorized operator-tool owner, with the
existing authority checks. Do not invoke cactus.run() behavior that executes
arbitrary Python functions inside the inference worker.

## Operator and API behavior

Build on the connected service's existing Library, model inspector, runtime
inventory, suitability and diagnostic operations. Proposed read projections are:

- Runtime definitions and implementation lineage.
- Installed bundle versions and their included compute paths.
- Host device/probe observations with timestamps and unknown reasons.
- Saved profile versus resolved/applied/observed execution.
- Capability/evidence records for the selected model and operation.
- Qualification receipts and structured compatibility blockers.

Return stable IDs, revisions and redacted evidence handles. Keep executable
paths, credentials and unbounded logs out of ordinary public projections.
If a new endpoint is required, register every native prefix and apply the
existing auth, role and principal/session checks. Reuse the endpoint registry
and actual-process Rust route coverage.

In the model inspector, select an implementation first, then a compatible
bundle/compute path and device placement. Show required companions, install
readiness, compatibility, fit and operation features separately. Retain explicit
Disabled / On demand / Hot policy and observed worker state.

Show why a profile cannot run and the next bounded action, such as installing a
specific bundle or acquiring a missing companion. Do not label a stopped worker
unavailable if it is compatible and ready to load. Keep existing temporary
service tests and client-connection workflows. Catalog qualification labels
must not claim output quality without the selected quality suite.

## Recommended integration sequence

The integration list separates preservation from expansion:

| Treatment | Delivery decision |
| --- | --- |
| Native parity | Migrate each selected existing recipe and operation with its original restrictions; retain unresolved requirements in the feature crosswalk |
| First expansion | Recommended first new adapters, described below; each selected target follows existing R4/R5/R7 gates |
| Conditional | Implement only for a named unmet workload/device and accepted selected-provider contract |
| Attach | Support independently owned services through explicit authority-limited connections |
| Reference only | Recognize packaging/client execution without scheduling a new daemon engine |
| Legacy | Recognize retained artifacts/installations, with no implicit upgrade into a different format/runtime |

Prioritize first expansion in coherent batches:

1. Text: ik_llama.cpp CPU/CUDA, MLX-LM Apple Metal, ExLlamaV3 CUDA with a selected
   TabbyAPI worker contract, and SGLang on one selected CUDA/ROCm target.
2. Voice: faster-whisper CPU/CUDA, sherpa-onnx CPU, Piper CPU, kokoro-onnx CPU,
   and MLX-Audio for selected Apple speech models.
3. Embeddings and Intel: TEI CPU/CUDA and OpenVINO GenAI on one selected Intel
   target. Other accelerators remain separate follow-up cells.
4. Tiny specialist models: current Cactus CPU runtime for Needle proposals and
   Whistle recognition, using its exact package/model revision.

Existing vLLM gains a separately qualified CUDA target without creating a second
vLLM project record. Existing llama.cpp gains SYCL/OpenVINO or other selected
paths only when its package and model acceptance exists. Current native FUTO
ASR4ALL and Parakeet Redux remain preservation work.

These batches are prioritization proposals, not a demand to postpone required
R1/R2 foundations or add all new engines to the first Rust release.

## Migration into R1 through R9

| Rewrite phase | Catalog work |
| --- | --- |
| R1 Foundation | Add identities, explicit legacy aliases, scoped capability/evidence records and compatibility reasons; extend ModelSpec through a versioned migration |
| R2 Resources/discovery | Connect profiles to physical devices/pools, actual slots, fit and Library views without preview side effects |
| R3 Evidence/assistant | Add selected model/operation qualification receipts and qualified recommendations; Needle receives no extra Barista authority |
| R4 Packages/artifacts | Verified bundles, complete artifact-role sets, immutable references, separate promotion/rollback and offline policy |
| R5 Direct services | Selected adapters and operation/protocol suites, bounded media jobs, cancellation and real-engine continuation |
| R6 Routing/access | Explicit capability/locality/affinity choices and bounded allowed fallback; independent-service authority |
| R7 Portable distribution | Exact host OS/architecture/driver/package/device qualification and provider installation acceptance |
| R8 Conditional scope | Selected vendor/mobile/specialist integrations and external formats without model producers |
| R9 Cutover | Consistent old-state migration, all required native mappings reconciled, prior-release restore and measured manager overhead |

Migration preserves public IDs, saved policies, revisions and artifact references.
Map legacy fields through the original recipe and package manifest. An ambiguous
system or npu selection becomes unresolved with a reason, never guessed.
Retain old worker references until terminal cleanup. Test migration against copied
state with interrupted generations and existing rollback dependencies.

Add a link to this design from PLAN.md when its owner incorporates the catalog
batch. Keep FEATURE-COVERAGE.json tied to its existing generator and product
dispositions. Record selected new providers explicitly rather than silently
changing imported feature status or approval hashes.

## Acceptance before a selected cell ships

A qualification cell binds runtime/implementation and adapter versions, bundle
digest, artifact-set digest, model architecture/quantization, backend path,
device/driver/OS/architecture, placement, operation and requested features.
Parameter bounds are explicit. Passing one cell does not qualify neighboring
cells in the matrix.

Use the existing native Rust unit, worker-fixture and rust-tests process lanes.
Keep Bun UI tests separate. Require evidence proportional to the selected
operation:

- Catalog validation rejects duplicate IDs, alias collisions, missing sources,
  dangling dependencies and incompatible schema versions.
- Resolution rejects wrong architectures, incomplete companions, mismatched
  quantization, absent backends, unsupported features and stale plans.
- No install, network access, policy mutation or inference lease occurs during
  read-only discovery and compatibility checks.
- Package tests cover hashes, ABI mismatch, interrupted transfer, concurrent
  install/delete, pinned worker references, offline operation and rollback.
- Worker tests cover argument/environment controls, readiness, observed-device
  mismatch, load/unload, crash, timeout, disconnect and unrelated-worker survival.
- Actual-process tests cover every exposed route/prefix, auth, session/principal
  isolation, admission, errors, cancellation and side effects.
- Tiny real models establish actual load/inference and protocol output shape,
  with cache continuation/restart where that cell advertises it.
- Speech tests include bounded audio, sample rate/channel rules, streaming
  finalization, timestamps and language handling as applicable.
- Synthesis tests include voices, valid bounded audio output and streaming
  behavior. Embedding/reranking tests verify dimensions, order, batch limits,
  finite values and selected model identity.
- Image/media tests include companions, parameter bounds, valid outputs,
  asynchronous progress and actual cancellation when advertised.
- Resource tests establish selected memory pools, UMA overlap, offload, slots,
  coexistence, NPU exclusivity and released reservations after failures.
- UI tests retain drafts/revisions and distinguish install, compatibility,
  qualification, fit, policy and actual worker observations.
- Matched performance tests use identical engine/artifact/settings and report
  daemon allocation, latency, CPU/RSS and disconnect overhead separately from
  engine inference speed. Preserve R9's broader soak and platform gates.

Upstream documentation supports a documented claim. A fixture supports the tested
adapter contract. A real engine on one target supports its receipt. None of
these alone establishes all-platform release readiness.

## Validation and handoff

From catalog-design/ run:

```sh
bun inventory.ts --check
bun inventory.ts --check --native-root /path/to/current/native/checkout
```

The first command checks inventory references and regenerable list consistency.
The second also compares all preserved native recipe/support rows and source
hashes with an explicit reference checkout. Neither executes a model or changes
runtime configuration.

The next implementation batch is R1 catalog/profile identity and exact native
alias resolution against the supervisor's current worker/artifact/cache owners.
Start with the existing llama.cpp implementation and one selected speech worker.
Keep new engine installation disabled until its own package and operation gates
pass.
