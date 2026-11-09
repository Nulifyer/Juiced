# Juiced platform plan

Revision: 4, 2026-11-09
State: proposed for renewed independent council review; revisions 2 and 3 remain historical approvals, not revision 4 or implementation acceptance
Baseline: v0.8.6, source 9cd79d01b32dfcd8b3a1a351afc68dc99c0a5266
Owner: primary Juiced maintainer agent; three independent plan reviewers

## Outcome and product boundary

Juiced is a hardware-aware model appliance. An operator downloads or imports
weights, inspects their identity and capabilities, chooses an execution profile
or a measured recommendation, and serves standard APIs directly to other tools.
The product manages artifacts, runtimes, resources, lifecycle, compatibility and
bounded operational evidence. Its UI tests services temporarily.

The latest November 9 direction excludes all Juiced-owned model-weight conversion,
quantization, re-quantization, calibration/export and repacking pipelines. Find
existing model variations and quants instead. Provide strong source/variant
search, complete-file review, runtime/host suitability and measured comparisons
so users can select and run the model they need. Preserve already acquired or
historically generated artifacts and unresolved ownership during feature retirement.
This exclusion applies to Barista and every optional phase too. Direct compatible
reuse, ordinary protocol encoding, execution arithmetic and KV-cache precision
remain separate serving behavior; they do not authorize a model-file producer.

The user's November 9 direction adds one agent exception: Barista, an optional
Juiced operator assistant whose hosted model role is `juiced-assistant`. It is off
by default, uses a user-selected normal Chat model and its existing settings,
and can inspect and control Juiced through typed native tools under operator
permissions. Its system prompt, curated skills and small-model evaluation are
required product work. `BARISTA.md` owns this bounded exception and acceptance.

General agent/interaction expansion remains excluded: no saved chat/project UI,
character histories, document/RAG workspaces, creative canvas/gallery, arbitrary
browser/filesystem/shell tools, general agent framework or workflow authoring.
Pi/T3, Home Assistant and other clients retain those responsibilities. Ordinary
model APIs parse/return tool calls and do not execute them. Barista's explicit
operator interface alone executes its allowed native Juiced tools; it does not
revive inherited Omni or arbitrary job graphs. Fixed inference/media jobs remain.

Host the same product on supported Linux systems, native macOS and Windows,
including CPU-only systems. Optional accelerator packages enable appropriate
hardware. Do not promise every artifact on every engine, every NPU as one device
class, or acceleration inside a container that cannot expose the native API.
"It works" means an exact supported combination has recorded acceptance evidence.
"Best" means a measured workload objective with correctness/quality constraints,
not the longest feature list or an unqualified fastest-model claim.

Existing deployment authority, proper model names, Disabled-by-default models,
manual runtime selections, current HA/Pi endpoints, four Hot services, volume
identities and retained weights are invariants. Release and GHCR publication and
reversible Portainer deployment remain within prior authorization. No destructive
migration or unrelated infrastructure changes are part of this plan.

## Inputs and completeness

The 77-tool survey and 211-family comparison live in the adjacent Homelab repository:
`docs/juiced-tool-survey.md`, `docs/juiced-feature-catalog.md`,
`docs/juiced-feature-gaps.md`, and `docs/evidence/juiced-capability-survey/`.
The authoritative disposition is `juiced/PLATFORM-FEATURES.json`, schema 1,
plan revision 4, SHA256 `b369ba1c3f3c8478eb3706fbafa8b226a9fb712ed51ec3ef150c620f4b01b29d`. It maps every catalogue ID to a phase, retained
behavior, conditional integration or explicit exclusion. The Homelab Markdown
projection has SHA256 `7230e46088fa9aacfd4204fc673dbdbcc5250c99830baac422c8ab13da179023`. Council approval binds this plan and both annex
hashes as one review bundle; the JSON owns phase assignments. It is a traceability aid, not
211 independent implementation tickets or a commitment to emulate every product.
J001-J006 separately trace the assistant, default-session, sidebar-version,
discovery/context, unified acquisition and no-conversion requests. Preserve the
survey's original IDs, names, baseline statuses and evidence references.
The normative Barista contract is `juiced/BARISTA.md`, SHA256
`e348076d7eeb6cfbe41d68bd4a6db6eac6d7e240037283be3f4062920c94e0f6`. Approval binds that contract too.
Revisions 2 and 3 are archived unchanged with their approvals. Their approvals
cannot be carried forward to this material scope change. Historical delivered
conversion behavior remains in baseline evidence without remaining product scope.

Current delivery owners are `juiced/RUNTIME-PROFILES.md`, `juiced/UI-DESIGN.md`,
native `src/cpp/server/juiced/`, backend descriptors, ModelManager, Router,
EvictionEngine and existing protocol handlers. Existing routing/cloud/MCP/media/
job/OTLP code must be reviewed and integrated before considering replacements.
Documentation and source presence are distinct from proven runtime behavior.

## Non-negotiable behavior

- One model/artifact identity owner, one effective option resolution path and one
  authoritative residency/admission path. UI, CLI and compatibility APIs are
  projections and callers. Do not add parallel registries or shadow schedulers.
- Backends remain supervised subprocesses with private dependency environments.
  The native host stays independent of desktop UI and agent clients. No required
  host Docker socket, nested daemon, Kubernetes, external database or gateway.
- Acquire complete existing variants and reuse weights unchanged when supported.
  Preserve numerical/format identity, source/revision, companions and licensing.
  Explain precision, modality and provenance differences between published variants.
  Never offer local weight conversion/quantization as a fallback for incompatibility.
- Distinguish unsupported, unknown, compatible, package-ready, smoke-qualified and
  performance-qualified. Unknown does not become true, false or zero. Supported
  installation is not proof of inference correctness.
- Explicit operator choices survive updates/reboots. New downloads/imported variants
  start Disabled and cannot be invoked until enabled. Experimental profiles remain
  opt-in, inspectable and reversible; benchmark absence alone does not prohibit a
  structurally compatible controlled trial.
- Standard API schemas stay compatible. Native rich metadata is separately
  versioned. Every native endpoint follows the existing four-prefix rule. Existing
  fixed-protocol exceptions and authentication rules remain enforced.
- Disconnection/cancellation does not reacquire Router locks or trigger broad
  model reloads. Slow network/probe/install/recovery work occurs outside shared
  lifecycle locks. Reconcile effects against generation/revision IDs before apply.
- API intent and active worker state remain separate. Failures are scoped to the
  affected job/profile/worker; no unrelated-worker unload as fallback.
- Preserve key-free trusted-LAN behavior for existing installations. Add optional
  client/operator controls without silently changing current authentication.
  Configure private listeners explicitly; Wyoming does not inherit HTTP auth.
- Cloud credentials remain environment-owned or process-memory-only as required
  by AGENTS.md. No prompt, credential or raw stack dump in default diagnostics.

## Operator flows and screen ownership

Keep the existing eight main screens and T3-derived compact foundations. New work
fills the right owner rather than adding pages for every engine feature.

| Screen | Main job | Added controls or information |
| --- | --- | --- |
| Models | See actual readiness and respond to failures | Fit/health, active device/profile, effective limits, pending changes; clear load/unload/reload actions |
| Loading policy | Set availability/residency | Whole-row drag and menus; optional idle/coexistence controls; protected service policy and overload behavior |
| Library | Find, acquire and run an existing variant | One search/selection/acquisition flow, clear repositories/quants, truthful host suitability, integrated transfer/downloaded state, working controls, full-source/companion review and safe delete |
| Model inspector | Configure one deployment | Typed settings, effective values, runtime/device selection, existing variant/draft/adapter links, apply/reload/rollback |
| Backends & hardware | Enable an execution path | Provider groups and variants, host prerequisites, package versions/update/rollback, exact restrictions |
| Cache | Control operational reuse and storage | Qualified modes, identity/miss reasons, retention/reclaim, references and budgets; no conversation browser |
| Service APIs | Connect external clients directly | Exact endpoint/auth/model discovery, operation support, versions, short copyable client configuration and connection probe |
| Diagnostics | Explain behavior and compare setups | Resource/request phases, raw/parsed logs, redacted export, neutral fixed-fixture benchmark and qualification receipts |
| Test services | Verify a service temporarily | Capability-led chat/transcribe/speak/image and other qualified task inputs; stop/reset; no saved interactions |

Barista is a compact top-bar panel, not a ninth destination. Its one-time
invitation is dismissible; it remains disabled until the operator enables it.
The sidebar footer shows the connected Juiced service version.

The model inspector is a pane, not an additional main destination. Test services
is the existing playground destination. Preserve accessible compact controls,
readable model names, keyboard/touch parity, dirty-draft protection and one
scroll owner. Short metadata uses title; additional explanations use the shared
Info component. Errors, losses and consequential changes stay visible. Do not
hide critical limits behind hover. No repeated accordion forests, speculative
harness categories, oversized whitespace or permanently verbose help.

## Architecture and ownership

### A. Model, artifact and deployment capability contract

Extend existing ModelManager/backend descriptor/BackendOps execution planning.
Use ordinary shared data structures at these boundaries, not another abstract
framework. A contract has a schema version and owner-produced revision identity.

Record these distinct facts without duplicating the source of truth:

- Artifact: publisher/repository/revision, files and hashes, quant scheme/precision,
  architecture, tokenizer/template, components and optional heads, license/source.
- Execution profile: provider/package/build/plugin identity, device IDs/ISA,
  resolved options and importer feature restrictions. Profiles reference artifacts.
- Service: operations and modalities, accepted inputs/output encodings, streaming,
  tools/reasoning/structured-output parsers, voices/languages, context and concurrent
  request limits. Advertised constraints must account for missing components.
- Qualification: source of each claim, exact tested identities, fixtures, results,
  expiry/invalidation and untested fields. Publisher claims remain labelled.
- State/policy: Disabled/on-demand/Hot is operator intent; worker readiness, current
  allocation, pending operations and applied options are observed, not intent.

Keep ordinary `/v1/models` valid for strict clients. Add a rich native capability
view under the existing version/prefix conventions; UI/CLI discovery, runtime
selection, router validation and client setup read that same view. Protocol-specific
model lists remain task-eligible and policy-aware. Publish JSON/OpenAPI schemas
where appropriate; do not describe a Juiced extension as an industry standard.

Discovery/context acceptance separates declared model/provider limits, configured
settings, applied launch settings and observed active-worker per-request limits.
Label total KV/context budgets separately from per-request limits and physical
slots. Preserve unknowns, provenance, selected profile and pending settings across
native/OpenAI/Ollama/MCP and UI views. Check aliases, concurrent publication,
reload and restart against exact artifact/runtime identities. A training limit,
auto sentinel or launch option is not proof of an engine's serving capacity.

Hard incompatibility has a concrete code/reason. A controlled unknown-profile
trial may be offered only if the backend can accept the artifact structurally and
startup/resource constraints are enforceable. Profile changes invalidate relevant
qualification/cache identity rather than rewriting old receipts.

### B. Hardware and runtime packages

Separate host capability, device access, user-space libraries, engine build and
model compatibility. Probes are bounded/read-only with platform guards and an
observable unknown/error state. Device IDs are stable where the platform provides
them; numeric enumeration alone is not a persistent identity.

Provider manifests declare OS/CPU ISA/GPU ISA/NPU generation, conjunction/exclusivity,
artifact/model/quant requirements, supported operations/options, package checksum,
source/license and ABI constraints. Native descriptor registration remains the
owner. Generate inventory/docs/schema from it where practical.

Runtime installation stages a versioned immutable bundle, verifies provenance,
probes it, and promotes an active reference atomically. Workers retain their exact
bundle reference. Keep a bounded known-good previous version; do not delete a
bundle or compiled cache while referenced. In-flight workers are not silently
rebound to new libraries. Updates can trial an idle model, restore the old profile
on failure and explain an interrupted update on restart. A failed promotion cannot
leave the active pointer on a partial bundle.

Do not install compilers or general package managers in the serving image to make
an unsupported target appear supported. Build environments remain separate.
Juiced does not bundle or expose a model converter/quantizer to manufacture
compatibility. Native engines use private libraries; Python engines use isolated
pinned bundles. An external backend protocol is added only for a concrete provider needing
it, with discovery, readiness, cancellation, metrics and failure conformance tests.
Remote generic OpenAI endpoints are a separate integration from local packages.

`BackendManager`, backend installation/staging helpers, CLI/API install/uninstall,
config `*_bin` changes and explicit executable overrides all participate in this
same bundle/worker-reference contract. Audit every legacy entry path before bundle
promotion ships. System-managed/external executables are explicitly external,
fingerprinted and cannot claim managed rollback. No bypass can unload healthy
workers or overwrite active bundles as a side effect of changing a path.

Host drivers, firmware, BIOS, kernel modules and Docker/Podman permissions remain
host-owned. Report prerequisites and produce platform-appropriate instructions;
never pretend a normal container can upgrade those layers. Optional user-space
bundles are updateable without replacing the core image, subject to ABI pins and
rollback. Native Windows/macOS packaging reuses current CMake/service boundaries.

### C. Resource admission and residency

Extend Router/EvictionEngine and the existing profile coordinator. One request
admission decision covers enabled policy, transition state, device exclusivity,
queue bounds and required resources. Do not introduce a second process scheduler
inside the Juiced UI/service layer.

Represent physical pools and overlap explicitly. CPU/GPU UMA mappings are not
independent RAM to add together. Account for unique weights, device copies, KV or
recurrent state, image encoders/VAE/workspaces, pinned host/XRT memory and bounded
job scratch. Estimates carry source/confidence and ranges. Unknown is not free
capacity. Use measured peaks when identities match; avoid claiming an exact
universal memory formula. Show fit, limited fit, unknown or does-not-fit.

Protect configured essential services through residency plus admission headroom
and compute/queue priority. Hot alone is not a latency reservation. Define anonymous
and optional authenticated client queues, global and per-service bounds, fairness,
deadlines and Retry-After/overload behavior. Prevent starvation; engine preemption
is only offered where implemented. Do not promise hard latency guarantees while an
uninterruptible engine kernel monopolizes a device. Report interference clearly.

Admission uses atomic incremental reservations owned by a scoped lease. Reserve
load-time weights/workspaces once per worker generation, then each request's
incremental KV/recurrent/output/workspace ceiling without counting shared weights
again. Concurrency races update the same pool ledger under Router authority.
Acquisition/import/install jobs reserve separate scratch limits before starting.
Unresolved legacy producer writes keep their original charges until confirmed cleanup.
Release/reconcile reservations on rejection, timeout, exception, cancellation,
disconnect, worker crash, unload and restart. Lease destructors/final callbacks
cannot perform blocking network calls while holding Router locks. If a backend
cannot enforce growth limits, reserve its conservative configured peak instead
of pretending tokens cost a fixed amount everywhere.

Unknown capacity blocks unattended new loads/recommendations, not existing
operator-approved profiles operating within their legacy limits. Mark those
profiles unmeasured/unprotected until qualified. An explicit trial needs declared
resource ceilings and bounded deadlines. Observed headroom shortfall stops new
background admission rather than evicting Hot workers or trusting zero estimates.

No default limit counts a user's conversations or imposes a per-model session
quota. A user with five chat threads can keep all five and submit work normally.
Idle threads do not each hold a live inference slot or duplicate model weights.
By default, per-principal pending/concurrency/rate/token caps are unset; operators
can explicitly add per-model or per-principal limits. Preserve genuinely explicit caps through migration;
do not turn a historical hardcoded default into a user choice. Distinguish unset
from explicit policy in the persisted schema and UI.

Execution capacity is physical and provider-specific. Router fairly queues active
requests when slots or compute are occupied, uses qualified continuous batching
or compatible state reuse where available, and applies backpressure when predicted
or observed RAM/VRAM/workspace headroom cannot safely fit the next operation.
Do not wait for an actual OOM. CPU saturation, nonpreemptible device work, bounded
request buffering and explicit operator limits can also require queueing. None
is a business limit on how many consumer conversations can exist.

Initial shared safety bounds remain global pending 64 and per-service pending 16,
subject to measured buffered-request memory/deadline acceptance. These are bounded
server queues, not per-user quotas. Anonymous callers share authority and the
native fairness domain; session strings do not grant more privileges. Essential
wait deadline 2 seconds, interactive 10 seconds and batch 30 seconds are configurable
queue bounds, not completion guarantees. Within each class use FIFO; after at most
eight consecutive essential admissions, attempt the oldest eligible nonessential
waiter while preserving essential headroom. Every waiter progresses or receives
a reasoned bounded overload response; no starvation or indefinite wait.

P1 must join actual backend/state-slot acquisition to this queue and prove five
simultaneous requests under measured headroom, plus idle/resumed thread counts
above slot count. The current llama.cpp single-slot default and cache busy path
are implementation audit targets, not the desired final behavior. Do not blindly
set a current-upstream auto-slot flag on an older pinned engine or reduce saved
per-request context to manufacture more slots. Qualify total versus per-request
KV/recurrent budgets, context identity and cancellation for each selected engine.
Unknown capacity still follows the explicit-trial/unprotected-profile rules above.
Preserve useful cached state where compatible; cold reconstruction is explicit.

Queue outcomes distinguish native slot occupancy, resource pressure, shared
buffer exhaustion, deadline and an optional operator quota. Test no artificial
principal/session rejection with caps unset, both default and explicit limits,
fairness across users/threads, pressure recovery and lease cleanup. Include CPU,
UMA and discrete pools without adding overlapping RAM and VRAM. If a nonpreemptible
operation prevents essential service meeting its bound, reject/defer that
background class before subsequent execution. This is not hard realtime.

Respect busy leases, bounded draining and cached-state references. Cold load,
weights-only sleep/downsize and hard eviction are different transitions. Resume
support is engine-specific. Maintenance drain/pause rejects new work and resumes
without changing saved policies. Background acquisition/import/benchmark work
has independent RAM/disk/concurrency limits and cannot silently evict Hot services.

### D. Acquisition, published variants and component library

Extend the native downloader, model registry and artifact/reference owners. Add
revision-pinned complete HF source snapshots, optional credentials, explicit gated
license handling, resumption and disk-space checks. Treat download size separately
from load memory. Reject incomplete shards/companions before claiming ready.

A model family may have multiple publisher/quant/component artifacts without every
file becoming a top-level model row. Preserve actual public model names and explicit
variant identity. Discover supports service, architecture, format, runtime, quant,
size and installed/downloaded filters as data becomes available. Do not assert
publisher equivalence from names; offer publisher search and inspected matches.

Library is one discovery-to-running task. Keep one query, source/service/runtime
facets, selected repository/variant and acquisition state. Integrate transfer
progress and downloaded readiness with results; avoid fragmented source searches
or separate Downloads/Downloaded tasks. Explain model versus repository versus
quant/companion identity, and keep that context through review, error and back
navigation. Curated picks are not hardware qualification. Expose known structural
runtime support, measured or conservatively estimated host fit and unresolved
requirements separately. Weight size alone is not a fit estimate; account for
whole MoE weights, context/KV/recurrent state, workspace and shared physical pools.

Every enabled action must produce an accepted native operation, a visible state
change or an actionable failure. Show the next step for missing runtime, unknown
fit, incompatible artifact, active/paused transfer, Disabled policy, readiness and
cleanup failure. Guide explicit On demand/Hot enablement after acquisition without
silently changing the Disabled default. End the flow with a temporary useful
service test and client endpoint. Retain first-use desktop/phone/keyboard/touch
acceptance against actual native jobs, including cancellation, retry and restart.

Model revisions have a ModelManager-owned immutable manifest binding weights,
config, tokenizer, chat-template source, projectors/encoders and every required
companion by hash/revision. Runtime promotion is a different operation from model
revision promotion. HF download refs and upstream update notifications can identify
a candidate but cannot independently change a live deployment's resolved files.

Stage and structurally validate a complete candidate, check current profile/feature
requirements, then run its predeclared selected-family acceptance before promotion.
For a loaded model, drain only the affected worker, preserve public ID/policy and
profile intent, and compare the expected current revision/generation before an
atomic active-manifest change. A quant/family change retains a distinct proper
variant ID; this is not an alias for silently changing the model behind a quant
name. Failed validation/load/promotion restores the previous complete manifest and
worker, not a mixture of old/new companions. Preserve prior revisions, binaries
and snapshots while current workers, unresolved legacy producer jobs, rollback manifests or
saved profiles reference them. Promotion invalidates incompatible cache and
qualification identities. A restart reconciles a recorded promotion intent to
the last committed complete revision before startup preload. P3 tests include
failed promotion, crash between staging/commit/reload, previous-companion restore,
policy/public-ID preservation and reference-protected reclamation.

Discover externally produced GGUF, Safetensors, AWQ/GPTQ/compressed-tensors,
EXL, OpenVINO/ONNX, MLX/UQFF/LiteRT, Q4NX and other variants only alongside a
qualified consuming provider/importer and explicit selected-family contract.
Search publisher/community provenance and revisions, format/architecture, quant,
required components, licensing and known compatibility. Do not infer equivalence
from similar names or claim a higher-bit derivative restores lost source data.
Compare selected compatible variants under P2's matched quality/resource gates.
Register existing components/adapters against a compatible base identity and
file references. Quarantine incomplete artifacts and remote-code requirements.

Retire conversion/quantization creation UI, CLI/API dispatch, recipes, bundled
producer tools and serving-image dependencies. Reject new jobs before teardown.
Keep read-only legacy records and only the native cancellation/cleanup/recovery
operations needed to protect existing data and unresolved writers. Drain accepted
operations through confirmed terminal effects before removing their executable
or staging references. Ambiguous prior-manager processes remain quarantined with
retained charges. Removing a feature or persisting a tombstone is not stop proof.
Existing prepared artifacts remain ordinary retained weights with registrations,
provenance and reference-safe cleanup. Never delete them merely because the
producer is retired. Recovery/restore acceptance covers their retained state.

Runtime-owned transient shader/graph/compile caches may remain with exact runtime,
artifact, device, resource and reference contracts. They are consumption details,
not a Juiced converter/exporter or another general model store. Global dedup/OCI
sources remain optional distribution improvements after identity/reference
accounting is reliable. No optional phase reintroduces a weight producer.

### E. Execution options, cache and comparisons

Typed options come from provider-owned schemas with defaults, bounds, requirements
and restart effects. Keep an advanced escape hatch for supported engine arguments,
with resolved effective settings and clear validation. Unknown arguments cannot
silently override typed policy or duplicate conflicting flags.

Add first-class draft pairing and acquisition, supported speculative method/heads,
acceptance metrics and memory cost. Add LoRA/adapters with base/architecture/quant
checks and per-request support only when the engine implements it. Expert placement,
NUMA/affinity, load mode, KV precision, context/RoPE and kernel choices remain
provider-specific advanced controls. Approximate attention/token pruning is labelled
separately from target-verified speculation; quality effects need evidence.

Keep operational prefix/KV snapshots distinct from model weights, retained legacy
derivatives, runtime-owned compiled caches and answer caching. Qualification and identity must cover weights/components,
tokenizer/template, engine/build/device where required, effective options and state
layout, including recurrent/MoE state. Restore tests compare real continuation after
slot replacement, unload/reload and process restart. Unsupported combinations show
a cold rebuild or explicit incompatibility, not a false cache hit.

P0 establishes a native request principal/session context before any P4 protocol
or persistent-cache expansion. The authentication owner normalizes every accepted
credential transport, including Bearer and x-api-key, into the same configured
principal ID, role and credential epoch. Cache keys consume that context, never
raw Authorization text or untrusted client identity headers. With authentication
disabled, dummy client API keys do not create trusted principals: all callers use
one explicitly documented anonymous trusted-LAN domain.

Persistent anonymous snapshots are opt-in and are shared-authority within that
anonymous domain. Session IDs partition convenience, not access control. Cross-
principal snapshot reuse is forbidden. Credential rotation/revocation or an auth-
mode change changes the credential epoch/namespace by default, producing a cold
rebuild; reuse requires an explicit validated migration. Session identity binds
principal/epoch, exact execution fingerprint, relevant request semantics and the
opaque consumer session. Protocols get separate namespaces unless an explicit
server-recognized session mapping proves equivalent requests. No generic stored-
conversation or previous_response_id service is added. Legacy snapshots without
this identity are retained but not silently reassigned/reused.

Tests precede P4: same accepted key via different headers yields the intended
principal; different principals and admin/inference credentials cannot cross-read;
unauthenticated/dummy headers cannot impersonate; rotation/mode changes invalidate;
protocol/session collisions fail safely; all error/cancel/stream-terminal paths
release cache and admission leases. Optional multi-key management UI remains P5;
this foundational request context cannot wait until P5. Snapshots can contain sensitive context.
Retention/bytes/forget controls remain explicit, with redacted diagnostics. No
persistent chat UI or transcript store is added. Broaden cache support across
protocols only with exact request/session identity and lease semantics. No universal
cross-engine KV conversion. Page-tiered/distributed KV and remote cache controllers
are optional later connectors, not core prerequisites.

Diagnostics can run neutral versioned fixtures against explicit profiles/quants
without becoming an interaction app. A receipt records full execution identity,
settings, load/cold/warm prefill, TTFT/queue/decode, per-user/aggregate throughput,
concurrency, memory peaks, cache work, errors and fixture-specific quality. Separate
streamed client timing from backend engine counters. Bounded reference evaluators
and structural/numerical checks are stronger than a vague model-judge score.

Before any comparison, freeze a versioned acceptance profile with baseline and
candidate identities/options, fixture hashes, primary objective, sample counts,
timing origin, queue/completion deadlines, percentiles, resource ceilings and
model-specific quality/numerical tolerances. A result with missing required bounds
is inconclusive, not qualified. Freeze targets before seeing candidate results.

Initial regression profiles require at least five paired cold/warm prefill runs,
ten paired generation runs and fifty short requests per essential service during
each baseline/candidate interference run. Cover long coding prefill and image
interference, plus idle control. Record every timeout/error, p50/p95 and peak
resources. Essential-service p95 may regress by no more than max(10% of matched
baseline p95, 100ms). No new OOM, mandatory schema/tool/continuation assertion
failure, lost Hot worker, or configured resource/headroom breach is allowed.
Profiles declare absolute bounded deadlines too. A failing/unusable baseline is
not a license to match failure: repair or classify inconclusive. Exact structural
assertions and numerical/quality tolerances are fixture/model/variant-specific,
concrete and versioned before testing; stochastic output is not judged by byte
identity unless deterministic equivalence is the declared contract.

A faster recommendation additionally requires >=5% improvement on its primary
metric and a 95% paired confidence interval excluding zero, while required quality
and protected-service gates pass. Tied performance can permit a predeclared
maintainability/packaging improvement under nonregression gates, without a speed
claim. Failed required gates block qualification/recommendation/promotion and
restore the previous active profile/revision. Unsupported optional capabilities
can remain disabled; a passing core release must not relabel them qualified.

Recommendations filter hard requirements first and prefer qualified stable profiles.
They optimize an explicit objective such as interactive latency, throughput or low
power. Show evidence and uncertainty. An unmeasured structurally compatible choice
can be tried but is not advertised as faster. Keep manual choices. Mage's prior
Ornith result favors ROCm over current experimental hybrid and remains the default.

### F. Service APIs, routing and specialist providers

P0 also removes inherited scope conflicts from Juiced's exposed execution paths.
`collection.omni`/`lemonade_omni` planner/tool-execution loops are excluded from
service promotion, model/MCP discovery and execution. CollectionOrchestrator's
model-selected tools and repeated planner turns are agent behavior, even when the
tools only call model services. Reject these through OpenAI dispatch, MCP and any
alias/collection gateway; do not silently redirect to a different interaction.
Preserve existing registrations as disabled/unsupported metadata and retain files.

In Juiced mode, caller-authored `definition.steps`, top-level `steps`, conditions,
branches, extraction/control flow and equivalent arbitrary graphs cannot be
created or resumed through native jobs, MCP or another ingress. Preserve legacy
records for restricted operational inspection/deletion/export; never auto-resume
them. Bounded interruption of currently active legacy work is a separate cleanup
operation. Fixed server-defined allowlisted runtime/media/test job kinds accept
only typed bounded parameters and cannot execute model-chosen tools or user graph
expressions. Keep fixed download/import jobs and retained legacy producer recovery distinct. Negative tests
cover alternate/nested payloads, discovery, aliases, resume, restart and protocol
entry paths without deleting historical records or creating a new generic runner.

Improve existing OpenAI, Anthropic, Ollama, A1111, Wyoming and native handlers in
place. Each supported operation defines request/result schema, error behavior,
streaming/cancellation, auth, capability discovery and conformance fixture. Reject
unsupported fields explicitly where silently ignoring changes output semantics.
Core OpenAI compatibility does not imply stored conversations or general agents.
Expose runtime model operations through the native APIs and optional MCP. Barista
uses its explicit operator interface for its typed tools; ordinary protocol calls
never gain that authority. No arbitrary file/shell tools.

Prioritize actual client contracts: HA core llama_cpp plus Wyoming, Pi/T3's model
provider, SillyTavern text/image/speech and standard OpenAI clients. Use public
fixtures and isolated client instances; never read private production conversations.
Maintain connections through migration rather than install custom consumer adapters.
Add other common voice/tensor protocols only with a named consumer and exact suite.
Response/semantic answer caching, OpenAI Batch and additional voice/tensor protocols
are conditional P7 integrations, not unconditional P4 deliverables. Before inclusion,
a named consumer contract must specify identity, eligibility/freshness, retention,
privacy, cancellation and resource ceilings. Personalized/tool-dependent/current-
information answers are not eligible for a generic default answer cache. Service-
side prompt rewriting/expansion pipelines are excluded; retain explicit model
chat templates and temporary test presets, with downstream clients owning expansion.

Promote inherited image edits/variations/upscale, media jobs, realtime and classification
only after reviewing handlers against policy/auth/admission/cancellation and actual
provider support. Provider presence is not a promise every model exposes the service.
Image jobs include components/adapters/conditioning, async progress and safe cancel. MCP qualification includes actual upstream termination and lease release for
notifications/cancelled and transport loss; acknowledging cancellation alone does
not satisfy it.
Speech distinguishes batch versus partial recognition, language/voice/speaker metadata,
streamed audio, VAD and duplex/barge-in. Video/music/3D/specialist encoders are provider
integrations with bounded output contracts, not new creative applications.

Integrate existing routing policies as optional virtual model services. Start with
explicit capability/task rules and measurable trace output. A classifier/LLM router
is optional, budgeted and evaluated against a static baseline. Routing chooses a
model path, not agent actions. Enforce candidate enabled policy, tools/vision/context,
locality, warm/cold cost and session affinity. Fallback is bounded, observable and
cannot switch a conversation's capability contract without an explicit policy.
Cloud stays opt-in; no silent egress. Do not automatically replay partially streamed
work or hide duplicate generations. Route evaluation must include overhead/quality.

### G. Operations, access and portability

Reuse live Prometheus/OTLP/raw-log plumbing. Add request phase/correlation, load/cache
miss reasons, device/thermal/power counters where available and redacted bounded
support bundles. Default telemetry does not export prompts or keys. Configure
sensitive tracing deliberately with retention/access limits.

Optional per-client credentials, model/service allowlists, quotas and revocation use
one native authorization owner across all HTTP/WS/MCP routes. Inference credentials
must not grant installation, deletion or policy writes. Preserve existing trusted-LAN
mode and current endpoints while making its authority explicit; client-supplied
identity headers are not authenticated identity. Avoid breaking HA's Wyoming trust
boundary. Add loopback/control-plane binding options for new deployments.

Use schema-versioned state migrations with backups, interrupted-operation reconciliation,
reference protection and explicit rollback compatibility. Publish backup/export/restore
procedures that are tested with runtime/model state, not merely a running container.
A state-changing release must define and snapshot a consistent durable set:
config/model registrations, saved recipe options, execution_profiles.json, Juiced
state.json/policies/trials/legacy producer references, package active manifests,
cache manifests/referenced snapshots, import/download manifests and retained
legacy jobs. Inventory external env secrets without printing/committing them;
ephemeral cloud keys remain nonpersistent. Immutable weights/bundles may be
referenced only if retained and hash-verified for restore.

Quiesce every state writer and new admission, finish/cancel/drain mutations, or
stop the service to take a consistent filesystem snapshot. Do not copy one JSON
file while related writers run. No two instances write one volume. Multi-file
migrations stage and validate a complete versioned generation and atomically
commit its manifest. Interruption selects the last committed complete generation,
never a mixed set. An old binary that cannot read the new schema rolls back by
restoring the old complete backup with the old image. Reference pruning protects
that restore set. Test existing native multi-file writers and reconcile them with
this contract instead of assuming their separate saves are a transaction.

Before deployment of any persisted-state/schema change, an isolated restore
rehearsal with copied state volumes and retained artifacts read-only must verify
policies, profiles/effective settings, file/package references and affected
consumer services. Exercise interrupted migration and previous-release restore.
No simultaneous hardware-exclusive worker trials on production devices. A failed
restore, unavailable rollback dependency or unsafe migration blocks deployment.
The release manifest declares whether state writes/schema changes occur and
identifies the backup/restore receipt, including a justified not-applicable result
for a genuinely read-only metadata-only change.

No raw credential-bearing Compose exports. Keep Home Assistant and Pi running while
rolling out unrelated UI/metadata work; use controlled maintenance for native restarts.

### H. Barista operator assistant

`BARISTA.md` defines the sole agent exception. Extend the native Juiced operator
service with one bounded assistant owner; its tools call existing ModelManager,
BackendOps, RuntimeConfig, Router, cache, artifact recovery, comparison and job owners.
No parallel model registry, scheduler, helper executor or required agent sidecar.
The stable `juiced-assistant` role selects a normal hosted Chat model and shares
its settings/worker. Operator-only dispatch rechecks enabled state, current
permissions and target revisions, with task-driven access to Juiced data.
Default permissions are full Juiced management after explicit enablement; optional
restrictions can narrow them. Regular inference credentials cannot gain admin
powers. Credential handles, offline/cloud consent and native resource/reference
checks remain authoritative. Missing native operations must be completed rather
than replaced by generated shell commands.

B0 defines tools/skills and the role during P0/P1. B1 evaluates exact 2B/4B and
larger model profiles in P2; B2 delivers optional onboarding/panel/control in P2
after native guards and qualification. B3 adds P3/P4's qualified lifecycle/parser
operations; B4 covers P5/P6 access and portability. Neither Barista nor adaptive
sessions is deferred to P7. Full P1 and the current foundation corrections remain
prerequisites to a mutating assistant release. The finite model/tool quality and
authority suite, normal settings and all six requested tasks are mandatory.

## Platform support and qualification

Support is a versioned matrix, not a list of logos. Compilation, package availability,
smoke tests, correctness/feature conformance and performance receipts are separate.
Cross-platform core code must retain Windows/Linux/macOS compilation guards. Never
label emulation or CI-only compilation as real accelerator qualification.

| Target | Native execution candidates | Deployment boundary and qualification |
| --- | --- | --- |
| Linux x86_64 CPU | llama.cpp, supported speech/image/ONNX providers | Minimum CPU ISA, RAM, permissions and cold/restart acceptance |
| Linux ARM64 CPU | supported native engines/task providers | Multiarch packages; no x86-only assumptions or fake accelerator claims |
| AMD gfx1150/XDNA2 Mage | current ROCm/Vulkan, scoped FLM and matched experimental hybrid | dri/kfd/accel, groups/memlock, host firmware and exact bundle; preserve measured defaults |
| AMD gfx1151 Strix Halo | ROCm/Vulkan; optional targeted guest engines | Per-model/quant kernels and UMA/firmware constraints; separate from gfx1150 |
| AMD RDNA/CDNA | qualified llama.cpp/vLLM/SGLang packages | Exact ISA, ROCm/PyTorch, quant kernels and memory/interconnect |
| NVIDIA x86/ARM | CUDA llama.cpp/vLLM/SGLang; optional TensorRT/NIM | Driver/container toolkit, compute capability and package/quant compatibility |
| Intel CPU/Arc/NPU | maintained SYCL/OpenVINO/OVMS or other reviewed provider | Intel GPU/NPU compiled paths differ; avoid archived IPEX as new default |
| Apple Silicon | native Metal/MLX | Native service/launchd; Linux Docker cannot provide native Metal; optional remote host integration |
| Windows x64/ARM64 | supported CPU/GPU/NPU vendor profiles | Native service/installer and driver matrix; WSL/container support is separately declared |
| Qualcomm | reviewed Hexagon/Adreno/QAIRT paths | Chipset/format/compiled model and companion restrictions |
| Ascend/TPU/Gaudi/other | maintained external vendor/provider plugins | Conditional integration; exact hardware access is prerequisite for qualification |

CPU is a useful first-class fallback, not a claim that large models will be fast.
Show why acceleration is unavailable and how to fix the relevant layer. Generate
Docker/Podman/rootless instructions from declarative requirements where safe. Never
run privileged host repairs automatically. Backend code can compile for hardware
we lack, but support remains unqualified until real-host evidence is attached.

Remote worker enrollment and replica pools are optional after single-host contracts
stabilize. They require authenticated/trusted transport, worker capacity/health and
model/package identity. Remote API providers and native resource-scheduled workers
are different contracts. RDMA/Thunderbolt/cache-disaggregation are conditional engine
features, not a promise to combine arbitrary devices over a VPN.

## Delivery sequence and exit gates

Each phase is a coherent verified release batch. It may contain coupled changes
across owners; do not split it into arbitrary commits per symbol. Future phases
remain planned until their own exit evidence exists. No phase is complete merely
because its plan or code is approved.

| Phase | Deliverable and existing owners | Exit evidence |
| --- | --- | --- |
| P0 Foundation | Reconcile docs; capabilities/qualification from ModelManager/BackendOps; native principal/session/operation context; exclude inherited Omni/arbitrary jobs; sidebar service version; Barista B0 contract; state-restore gate | Schema/unknown/policy discovery tests; negative graph/Omni/MCP fixtures; auth-header/session-isolation cases; backup/restore prerequisite for any changed durable state; no weight/profile change; existing HA/Pi checks |
| P1 Resources | Shared-pool estimates and scoped atomic reservations, bounded queues/aging/deadlines, Hot protection, adaptive session/slot acquisition with no default user quota, idle/maintenance controls in Router/EvictionEngine; discovery/context and unified Library flow; safe legacy producer retirement | UMA/device accounting, five concurrent/over-slot thread cases with unchanged context and optional caps; races/drain/cancel/failure tests; frozen acceptance profiles pass long-prefill/image interference and HA-like timing/resource gates; no hard realtime promise |
| P2 Evidence and tuning | Versioned comparison runner, receipt invalidation, request phases/redacted bundle, evidence-backed recommendations; Barista B1 actual-model/tool qualification and B2 optional operator assistant | Frozen acceptance profiles pass matched quant/runtime cold/warm/concurrency/quality gates; cleanup/restoration and recommendation significance; deterministic report; UI task/error states; BARISTA.md quality/authority/cancellation suite and off-by-default onboarding |
| P3 Packages and artifacts | Full-source/companion acquisition; separate complete model-revision and runtime-bundle promotion/rollback; components/adapters/drafts and existing published variants; Barista B3 lifecycle tools as their owners qualify | Interrupted download/install/import/promotion/restart and hash/disk failures; restore complete prior model plus companions/package; reference safety; concrete quality/resource gates; loss disclosure |
| P4 Direct services | Complete selected image/speech/specialist contracts and client fixtures; broaden cache qualification by exact engine/protocol; Barista B3 selected tool/parser conformance | Real native client success, failures, cancel and partial-stream checks; supported cache continuation after replacement/restart; policy/auth/admission parity |
| P5 Auto service and access | Existing router integration, capability/affinity/fallback, optional per-client/model controls and quotas (unset by default); Barista B4 multi-operator restrictions | Route replay/overhead/quality versus explicit models; no silent cloud; cross-protocol allowlist/admin/cancel tests; existing anonymous mode preserved |
| P6 Portable distribution | Linux multiarch/Windows/native Mac packaging, host diagnostics and tested deployment guides; qualified additional providers and Barista B4 | Core build/package CI per OS/arch; real-host support receipts separately named; clean install/upgrade/recovery and consumer smoke on qualified targets |
| P7 Optional scale and formats | Remote native workers, replicas, OCI/dedup, externally produced format/quant targets and runtime-owned caches, answer cache/Batch/additional protocols, tiered cache and specialty providers when justified | Named consumer/workload, matched importer/engine and hardware evidence; no mandatory new infrastructure; no additional agent features beyond Barista |

Every P3/P4/P6/P7 release has a selected-family manifest naming exact providers,
models/artifact families, operations, platforms and acceptance profiles. Unselected
families remain visibly conditional/deferred, not implied by a broad phase title.
A phase can be delivered incrementally only when its selected batch is explicit;
remaining required core behavior stays open in the ledger. No universal support
claim follows from phase completion.

P0 is the first implementation batch after council approval. It is intentionally
compatible with the current native manager and does not install engines or reassign
models. Later phases may be refined by their measurements within this contract.
A material architecture/scope change requires renewed council review, not a silent
reinterpretation. Routine implementation choices do not require user confirmation.

## Verification and release contract

Before changing source, inspect worktrees and preserve unrelated work. Existing
Homelab research is local/uncommitted and must not be swept into Juiced commits.
Use the source repo for product code/public docs and Homelab for deployment contracts,
private-host evidence and the coordinating ledger. Reuse current C++/Python/native
API/Playwright/client tests and CI packaging, adding behavior/failure tests at actual
new boundaries. Do not add tests that merely mirror low-impact layout edits.

For each relevant batch:

1. Verify required schema, lifecycle, concurrency and error outcomes through native
   interfaces. Include unsupported/unknown cases and explicit current-policy guards.
2. Build the actual runtime artifact with separated stages. Inspect exact package/
   model/build identities and required reference retention. Cross-platform core build
   checks are required when shared C++ interfaces change.
3. Exercise affected UI states and compact desktop/phone layouts with existing browser
   fixtures and actual-container tests; check all main-screen smoke paths for a UI release.
4. Require frozen acceptance profiles and pass/fail bounds before running bounded neutral client/service acceptance against a staged or scoped deployment.
   Real hardware correctness/performance cannot be inferred from mocks. Preserve four
   Hot models, actual names, old volumes and previous image for restoration.
5. Before any state-changing deployment, require a successful isolated backup/restore and previous-release compatibility rehearsal as defined above. Then publish a release and immutable GHCR image reference after required checks pass.
   Update Portainer endpoint 7/stack 83 through the existing helper when deploying Mage;
   preserve routes/networks/mounts. Do not change shared network infrastructure.
6. Record deployed image/digest/source, health plus real service acceptance and rollback
   status. A running container alone is insufficient. Stop if restoration fails and
   report that exact boundary rather than continue destructive retries.

Only inspect public upstream/isolated client data. User-owned NSFW prompts and
private chats are not needed for fixtures. Model quality tests are neutral and
bounded; user content stays user-owned.

## Council acceptance

Exactly three initial independent GPT-6.1-sol/high review seats examine this entire
plan and the survey/implementation owners. Focus areas supplement, not replace,
whole-plan review: A architecture/portability, B serving/resources/integration,
C product boundary/UX/verification/delivery. Reviewers have read-only responsibility
and cannot approve by majority over an unresolved technical objection.

Each returns APPROVE or REQUEST CHANGES, blocking findings with source/reason,
required edits and important nonblocking risks. The parent resolves objections in
one canonical revision and records an evidence-linked disposition. Rechecks are
new review tasks with the revised plan and prior findings. Whole-plan approval
requires three explicit approvals on the same plan revision/hash, pinned annex and normative Barista contract
bundle, no unaddressed blocker,
and preserved user exclusions. Approval permits implementation; it does not certify
unimplemented capabilities, all hardware, performance or deployment success.
