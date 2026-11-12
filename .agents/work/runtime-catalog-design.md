# Runtime catalog design for the Rust rewrite

Status: complete
Owner: runtime catalog design session
Repository: /home/nulifyer/Projects/Juiced-rust-rewrite
Base: explore/rust-bun-rewrite at 62e82570d2d5625addff0da636420f68d27b80d7, with concurrent rewrite work preserved
Project: [rewrite plan](../../juiced/rewrite/PLAN.md)
Request source: November 10, 2026 user asked to flesh out the researched runtime/backend breakdown and the proper addition list for inclusion in the Rust rewrite.

## Goal

Deliver a concrete runtime catalog design, a prioritized machine-readable inventory,
and an explicit migration/acceptance handoff that fits existing rewrite owners.
This task delivers design inputs. Implementing and qualifying runtime adapters
belongs to subsequent selected implementation batches.

## Acceptance checks

- [x] Inspect the rewrite's current plan, config/profile/supervisor ownership and active task records.
- [x] Preserve all 18 native recipe identities, all 44 declared support rows and the hybrid fork distinction.
- [x] Separate model families, implementations, bundles, compute paths, hardware, execution profiles and qualification.
- [x] Prioritize new integrations without expanding existing release promises or weakening required parity.
- [x] Define package/artifact lifecycle, resource/cache identity, authority and subprocess boundaries.
- [x] Map design work into existing R1 through R9 milestones and native Rust acceptance lanes.
- [x] Validate inventory references, generated list, local documentation links and exact native source rows/hashes.
- [x] Keep writes confined to new catalog-design files and this ledger, preserving active implementation/shared records.

## Scope

In: juiced/rewrite/catalog-design/, this ledger.
Out: runtime implementation/configuration, existing shared rewrite plans and ledgers, native descriptors, main-checkout edits, commits, external messages, installation or deployment.

## Current state

Phase: design and catalog deliverables verified, local-only and uncommitted.
Dependencies: source snapshot at main HEAD 8e4b338415d0ebad9aad11307815867e24462959 plus four exact source file hashes; Bun for inventory checks.
Integration: new files reside in the rewrite worktree. Shared PLAN.md and FEATURE-COVERAGE.json remain unchanged by this task. Incorporation and runtime implementation remain with their existing owners.
Next action: incorporate [DESIGN.md](../../juiced/rewrite/catalog-design/DESIGN.md) as the R1 catalog/profile identity input, then implement the existing llama.cpp plus one selected speech worker against current artifact/cache/admission owners.

## Decisions

| Decision | Reason | Evidence |
| --- | --- | --- |
| Maintain inventory.json and generate the readable list | Keep one owner for the catalog list and prevent divergent tables | catalog-design/inventory.ts --check |
| Preserve 18 native mappings with a separate hybrid distribution | Existing native metadata and pinned packages have more detail than generic runtime names | BACKENDS.md; backend_versions.json; RUNTIME-PROFILES.md |
| Use attach-first treatment for independently owned services | API compatibility does not grant process, storage or resource authority | DESIGN.md external service contract |
| Keep proposed additions separate from required migration scope | Conditional or researched entries cannot silently change product disposition | rewrite/PLAN.md; PLATFORM-FEATURES.json |
| Keep FUTO and Handy transcribe.cpp identities separate | Different source origins and selected worker/model contracts | juiced/Dockerfile; juiced/runtime/futo_server.cpp |
| Preserve subprocess isolation for every implementation language | Runtime library language does not change the appliance boundary | AGENTS.md; rewrite/PLAN.md |

## Findings

| Finding | Evidence | Status |
| --- | --- | --- |
| Inspected Rust config has ExternalHttp and LlamaCpp providers, with common worker ownership in the supervisor | rewrite/service/src/config.rs; profiles.rs; supervisor.rs | Design fits current owners |
| Native metadata declares 18 recipes and 44 support rows | Exact table comparison and four source SHA-256 checks | Verified observation |
| llamacpp/hybrid-rocm needs a distinct pinned implementation identity | Nulifyer/llama-xdna-hybrid pin and native runtime-profile contract | Preserved as profile override |
| Cactus tiny-model runtime and general Cactus Engine are distinct current packages | Upstream cactus and engine repositories | Separate inventory entries |
| Native Kokoro/Parakeet recipe names do not identify every implementation of their model families | Native Kokoros package and pinned mudler/parakeet.cpp source | Explicit migration mappings |

## Changes

| Path or artifact | Change | Verification |
| --- | --- | --- |
| catalog-design/DESIGN.md | Catalog identities, compute paths, qualification dimensions, worker ownership, examples, lifecycle, resources/cache, API behavior, R1-R9 mapping and acceptance gates | Local links and whitespace checked; reviewed against inspected contracts |
| catalog-design/inventory.json | 69 entries, 28 compute-path IDs, 21 discovery workload tags, 18 migrations, 44 native support rows and 9 model-family examples | Bun structural/reference and exact native source validation |
| catalog-design/INTEGRATIONS.md | Generated readable preservation/addition/service/legacy list | Generator --check passes |
| catalog-design/inventory.ts | Dependency-free Bun validator and list renderer | Executed generation and --check with explicit native reference |

## Risks and unknowns

- Candidate compute paths and tasks are planning leads, not supported matrix cells.
- No runtime adapters were implemented or hardware/model inference qualification executed by this task.
- Native reference hashes identify a bounded observation. The explicit reference check fails if those source files change.
- Native recipe presence does not override retained/conditional/excluded product disposition.
- The shared rewrite plan and implementation remain active under another owner. These isolated design inputs do not establish release readiness or overall rewrite completion.

Verification command: bun inventory.ts --check --native-root /home/nulifyer/Projects/Juiced from juiced/rewrite/catalog-design/.
Result: 69 entries; parity 19, first expansion 12, conditional 27, attach 6, reference only 3, legacy 2; 18 native recipes; 44 native support rows; local source references valid; generated list current; exact source hashes and rows match; execution remains disabled.
