import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

type Feature = { id: string; feature: string; phase: string; scope: string; disposition: string; baseline_status?: string };
const root = resolve(import.meta.dir, "..");
const repository = resolve(root, "../..");
const read = (path: string) => readFile(resolve(repository, path), "utf8");
const digest = (value: string) => createHash("sha256").update(value).digest("hex");
const [catalogText, planText, baristaText, approvalText, snapshotText] = await Promise.all([
  read("juiced/PLATFORM-FEATURES.json"), read("juiced/PLATFORM-PLAN.md"), read("juiced/BARISTA.md"),
  read("juiced/PLATFORM-APPROVAL.json"), read("juiced/rewrite/MAIN-SNAPSHOT.json"),
]);
const catalog = JSON.parse(catalogText) as { plan_revision: number; entries: Feature[]; user_requirements: Feature[] };
const approval = JSON.parse(approvalText);
const snapshot = JSON.parse(snapshotText) as { files: { path: string; sha256: string }[] };
const binding = { plan_sha256: digest(planText), annex_sha256: digest(catalogText), barista_contract_sha256: digest(baristaText) };
if (approval.status !== "approved" || approval.plan_revision !== catalog.plan_revision || Object.entries(binding).some(([key, hash]) => approval[key] !== hash)) {
  throw new Error("The recorded product approval does not bind this plan bundle. Review the changed sources and approval state before regenerating.");
}
for (const file of snapshot.files) {
  if (digest(await read(file.path)) !== file.sha256) throw new Error(`Imported source drift: ${file.path}. Reconcile the contract snapshot before regenerating.`);
}

const phaseMap: Record<string, string> = { P0: "R1", P1: "R2", P2: "R3", P3: "R4", P4: "R5", P5: "R6", P6: "R7", P7: "R8" };
const retained: Record<string, string> = {};
for (const [phase, ids] of Object.entries({
  R1: "F014 F064 F065 F123 F192",
  R2: "F001 F002 F004 F049 F066 F067 F068 F069 F070 F206",
  R3: "F175 F176 F177",
  R4: "F005 F008 F010 F017 F021 F030 F056 F104 F105 F106 F107 F157 F198",
  R5: "F083 F116 F117 F118 F119 F120 F133 F146 F156",
  R7: "F037 F038 F046 F054 F060",
})) for (const id of ids.split(" ")) retained[id] = phase;

const gates: Record<string, { owner: string; acceptance: string }> = {
  R1: { owner: "identity and configuration", acceptance: "Exact identity/options/unknown-state contracts, normalized principal/session context, negative Omni/graph cases and copied-state restore acceptance." },
  R2: { owner: "admission and operator workflows", acceptance: "Shared-pool and actual-slot tests; five active requests and more idle threads than slots with unchanged context; fair bounded queues, explicit caps, cancellation and unified Library journey." },
  R3: { owner: "qualification and Barista", acceptance: "Frozen paired quality/resource/interference profiles, redacted receipts, restored prior state and the exact Barista model/tool/authority suite before enabling the operator panel." },
  R4: { owner: "artifact and runtime lifecycle", acceptance: "Complete artifact/package identity, interrupted acquisition/promotion/restart/rollback, reference safety, private/offline policy and selected-family acceptance." },
  R5: { owner: "protocol and specialist services", acceptance: "Named consumer wire fixtures, policy/auth/admission parity, actual upstream cancellation and qualified continuation after worker replacement/restart." },
  R6: { owner: "routing and access", acceptance: "Routing replay versus explicit models, bounded fallback, no silent cloud egress, credential revocation and cross-protocol allowlist/authority fixtures." },
  R7: { owner: "platform delivery", acceptance: "Native OS/architecture package CI and restore tests; separate real-device support receipts and actual HA/Pi client smoke tests." },
  R8: { owner: "conditional integrations", acceptance: "A named consumer/workload and separately selected family, provider/importer, hardware/resource/cancellation/privacy acceptance; no mandatory new infrastructure." },
};
const partial: Record<string, string> = {
  F001: "Library shows a curated recommended list beside search results with task chips. Ranking and per-host recommendation tuning remain open.",
  F002: "One Library search combines the catalog, downloaded files and Hugging Face with a bounded cache and a hub-free first answer. Repository card and file-tree inspection remain open.",
  F004: "Search results group GGUF quants under one model with size and host-fit hints. Non-GGUF variant grouping remains open.",
  F008: "Browser upload and server import-folder intake (copy or in place) validate format, hash while writing and feed the same add step. Split shards and projector companions are refused.",
  F056: "The container installs and updates engine runtimes into its data volume at runtime with checksum-verified releases. Signed provenance and rollback UI remain open.",
  F060: "The rewrite container uses separate build and runtime stages with a non-root runtime user and one data volume. Docker, ROCm and NPU host builds are not qualified.",
  F062: "Adding a model picks the engine and backend from detected hardware and offers the install when missing. Workload-aware benchmarking of the choice remains open.",
  F069: "Saving launch settings on a loaded model restarts it with the new settings and rolls back the saved profile on failure. Other apply paths remain open.",
  F071: "On demand models unload after a service or per-model idle delay; loads evict the least recently used idle On demand model under budget or host-memory pressure; Ollama keep_alive is honoured. Cost-aware eviction remains open.",
  F119: "POST /sdapi/v1/txt2img through sd.cpp workers, one image per request. Other A1111 routes remain open.",
  F120: "A Wyoming TCP server answers describe, transcribe and synthesize through whisper.cpp and Kokoros, verified with the official wyoming client. Streaming events and a real Home Assistant instance remain unverified.",
  F124: "Aliases resolve for every inference route and can be swapped while requests in flight finish on the old target. Alias-scoped policies remain open.",
  F126: "A Realtime WebSocket handles the transcription intent with pcm16 24 kHz audio and energy-based server VAD. Conversation intent and delta events remain open.",
  F146: "whisper.cpp transcription is verified with real models; FUTO, Parakeet and Moonshine engines are tracked separately.",
  F147: "Kokoros voices are listed per model with language prefixes and used by the speech test and Wyoming. Speaker metadata beyond names remains open.",
  F150: "Realtime transcription has energy-based server VAD with configurable threshold and silence. Model-based VAD remains open.",
  F156: "The Images test generates through sd.cpp with size, steps, seed and negative prompt. Media job progress remains open.",
  F175: "Dashboard and footer show live CPU, RAM, GPU, VRAM and decode rates. Per-model token metrics history remains open.",
  F176: "Diagnostics streams bounded service and worker logs. Parsed log views remain open.",
  F208: "Operators can pause and resume new work; queued work drains. Scheduled maintenance windows remain open.",
  J005: "One Library journey covers search, quant choice, download progress, engine install and add with a chosen policy, plus upload and import. Repository companions remain open.",
  J006: "Library offers published GGUF variants and never converts or quantizes weights.",
  F005: "Single-file bounded SHA256 acquisition has durable progress, pause, resume, retry, cancel and restart tests. Full repository/companion/package acquisition remains open.",
  F104: "Explicit opt-in single-slot completion snapshots pass save, daemon restart, restore and continued inference with a tiny real GGUF. Automatic/session-wide and selected-provider qualification remain open. Selected Linux writer proof does not qualify macOS/Windows or cross-environment recovery.",
  F105: "Versioned profile, retained model/executable handles, captured child environment, bundle and observed Linux executable/shared-library/kernel identity join cache compatibility. Selected argument positions and external configuration metadata are bound; model/runtime freshness, principal/epoch/protocol/session and checksums are guarded. Immutable bytes, portable device/driver/companion closure and selected-family qualification remain open. Native macOS/Windows identity sources remain reference-only until their own Rust and target-native acceptance.",
  F107: "Scoped CAS forgetting, active-reference protection, corruption inspection and retained uncertain charges pass. Full TTL/reclamation/privacy policy remains open. Portable prior-writer recovery, helper EOF/exit handling and complete operator recovery require separate target-native proof.",
  F117: "Selected Messages/tool codecs, SSE and native token counts pass actual-process fixtures. Strict usage accounting and real-client/advanced tool conformance remain open.",
  F118: "Selected chat, raw generation, embeddings and discovery pass actual-process fixtures. Unsupported options reject; real-client conformance remains open.",
  F125: "Stateless MCP2025-06-18 JSON requests, explicit-model tools and credential/session-scoped actual cancellation pass process tests. Browser origin allowlisting, optional stateful transport and real-client qualification remain open.",
  J002: "Bounded FIFO/fair queues, deadline/overflow, cancellation, essential headroom and no default principal/session quotas pass native/process tests. Provider-observed slots and full physical pools remain open.",

  F064: "Disabled default and discovery/load guards cover implemented OpenAI, Anthropic, Ollama and MCP routes; full retained protocol coverage remains open.",
  F065: "Durable CAS policies exist; complete policy/application semantics across protocols remain open.",
  F066: "Hot startup load and protection exist; automatic qualified Hot recovery remains open.",
  F067: "Multiple HTTP worker subprocesses can be resident within declared reservations. Selected imported CPU profiles also share protected host accounting; complete physical-pool and resource qualification remain open.",
  F068: "Busy unload rejects; exact-generation cleanup retains resources through cancelled startup, stop failure and retry. Cancelled blocking inspection retains preparation claims; Linux work shares an acknowledged actor-owned sleep inhibitor. Worker/artifact/power shutdown reports unsettled owners. A distinct bounded startup permit admits before RAM/power, coalesces duplicate loads and retains unsettled ownership. Deferred role retirement, descendant containment and portable recovery remain open.",
  F076: "Read-only suitability reports declared reservation pressure and unknown physical fit; complete context/pool identities and measured capacity remain open.",
  F083: "Actual-process socket cancellation covers three OpenAI operations and scoped MCP; real-engine transport-loss and all retained protocol qualification remain open.",
  F116: "Chat, completion, Responses and embeddings share a cancellable forwarder; selected single-slot whisper.cpp multipart transcription has strict field/byte/response boundaries. Complete OpenAI semantics, remaining media and real consumer conformance remain open.",
  F121: "A versioned69-entry read-only catalog preserves exact18 native recipes/44 support rows, aliases and blockers. Config2/profile1 migration and selected copied-native Config3/Profile2 activation bind retained launch plans, adapters, implementation IDs, native settings authority, arguments and verified bundles without qualifying complete placement or backup. Complete immutable artifact/device/capability contracts remain open.",
  F122: "Implemented and absent protocols are explicit; complete parameter/version conformance remains open.",
  F123: "Disabled/missing models are hidden from OpenAI discovery; remaining client namespaces remain open.",
  F185: "Engine health probing precedes ready state; useful-service and quality qualification remain open.",
  F188: "Bearer and x-api-key normalize to scoped principal/role/credential epoch; credential/session cache isolation passes. Client registry, allowlists and live revocation remain open.",
  F192: "Standalone Rust service and static console run; full appliance packaging and runtime parity remain open.",
  F209: "JSON and selected speech body sizes, shared inference ingress capacity, queues and upstream timeouts are bounded. Cold startup additionally has bounded FIFO capacity and deadline/cancellation ownership. A selected protected CPU host ledger joins bounded text profiles, declared peaks/residency/request ceilings and complete Linux headroom observations. Interrupted text retains exact-generation slot/power/bytes through explicit cleanup. Protected state, accelerators, measured peaks/interference and all media backpressure remain open.",
  J003: "Sidebar reads the connected Rust service version and has an unknown state; release integration remains open.",
};
const featureAcceptance: Record<string, string> = {
  F104: "Bind the exact owned writer before snapshot dispatch. Require target-native positive live/exited and cross-process evidence for Linux, macOS or Windows support. Unsupported, legacy or changed-context writes retain their charges; unsupported-capability success is not recovery qualification.",
  F105: "Qualify each native identity method and its original execution domain. macOS process-unique ID/boot-session UUID and Windows process-start/boot/session keys remain unported references. Unknown layouts, server silos and unverified job visibility fail closed. The Windows loader boot-entry GUID is not a per-boot random nonce. Never signal or collect a process during observation.",
  F107: "Commit cleanup authority before reclaiming files and retain ownership/charges after publication or deletion failure. Output EOF is not process exit: helper timeout/cancellation continues, trailing callbacks run once and callback failure/cancellation retains exact-child cleanup ownership. Require native OS proof; cleanup can exceed the requested timeout and does not prove descendant/device/remote termination.",
};
const refinements: Record<string, string> = {
  F028: "Acquire/register existing Q4NX only. Local conversion remains excluded.",
  F031: "Legacy writer drain/quarantine and confirmed cleanup only. Never resume a producer.",
  F032: "Retain existing artifacts and unresolved references without authorizing new production.",
  F034: "Bound acquisition/import/metadata scratch and retain unresolved producer charges.",
  F078: "No default per-principal or conversation quota; bounded shared queues are a separate safety limit.",
  F084: "Qualify actual engine slots and five requests without shrinking per-request context.",
  F190: "Identity/session quotas are unset by default. Preserve actual explicit operator caps; P5 adds their optional UI.",
  F194: "Only Barista with typed Juiced tools and bounded source retrieval. No general shell, browser, filesystem or agent loop.",
  F208: "Pause/drain/resume must preserve saved policies and expose pending active work.",
  J001: "B0 in R1/R2, B1/B2 in R3, B3 in R4/R5, B4 in R6/R7. Full R2 precedes a mutating assistant release.",
  J002: "No default user/session cap. Join fair admission to real slots and preserve per-request context.",
  J004: "Keep declared, saved, applied and observed context separate with exact identity/provenance.",
  J005: "Begin the unified discovery-to-running Library in R2; complete lifecycle in R4. No fragmented Downloads destination.",
  J006: "No converter/quantizer/exporter in any phase. Preserve retained artifacts and unresolved writer ownership.",
};
const features = [...catalog.entries, ...catalog.user_requirements];
const seen = new Set<string>();
const entries = features.map((feature) => {
  if (seen.has(feature.id)) throw new Error(`Duplicate source ID: ${feature.id}`);
  seen.add(feature.id);
  const phase = feature.scope === "excluded" ? null : feature.phase === "retain" ? retained[feature.id] : phaseMap[feature.phase];
  if (feature.scope !== "excluded" && !phase) throw new Error(`No rewrite phase for ${feature.id}`);
  const gate = phase ? gates[phase] : null;
  return {
    id: feature.id, feature: feature.feature, source_phase: feature.phase, scope: feature.scope,
    disposition: feature.disposition, rewrite_phase: phase, owner: gate?.owner ?? "scope enforcement",
    rewrite_status: feature.scope === "excluded" ? "excluded" : partial[feature.id] ? "partial" : feature.scope === "conditional" ? "conditional" : "planned",
    acceptance: feature.scope === "excluded" ? "No executable/exposed path; preserve historical data and unresolved ownership during retirement." : [gate!.acceptance, featureAcceptance[feature.id]].filter(Boolean).join(" "),
    ...(partial[feature.id] ? { implementation_boundary: partial[feature.id] } : {}),
    ...(refinements[feature.id] ? { constraint: refinements[feature.id] } : {}),
  };
});
if (catalog.entries.length !== 211 || catalog.user_requirements.length !== 6) throw new Error("Source catalogue size changed. Review coverage and milestone ownership before updating expected counts.");
const output = JSON.stringify({ schema_version: 1, generated_by: "scripts/sync-plan.ts", source: "../PLATFORM-FEATURES.json", plan_revision: catalog.plan_revision, product_approval: { status: approval.status, ...binding, scope: "Exact upstream product bundle only. Not Rust architecture, implementation, hardware or deployment approval." }, entries }, null, 2) + "\n";
const path = resolve(root, "FEATURE-COVERAGE.json");
if (process.argv.includes("--check")) {
  if (await readFile(path, "utf8") !== output) throw new Error("Feature coverage drift. Run bun scripts/sync-plan.ts after reviewing source changes.");
} else await writeFile(path, output);
const statuses: Record<string, number> = {};
for (const entry of entries) statuses[entry.rewrite_status] = (statuses[entry.rewrite_status] ?? 0) + 1;
console.log(JSON.stringify({ features: catalog.entries.length, user_requirements: catalog.user_requirements.length, statuses, check: process.argv.includes("--check") }));
