# Main plan integration into the Rust rewrite

Status: complete
Owner: primary agent in explore/rust-bun-rewrite
Repository: /home/nulifyer/Projects/Juiced-rust-rewrite
Base: 62e82570d2d5625addff0da636420f68d27b80d7
Project: [rewrite plan](../../juiced/rewrite/PLAN.md)
Request source: User requested reviewing the main checkout's feature plans and bringing those features and plans into the isolated rewrite.

## Goal

Carry the complete main-checkout product scope into an actionable Rust rewrite roadmap, retain current contract clarifications, and implement a bounded read-only suitability operation without overstating parity.

## Acceptance checks

- [x] Review all 211 catalogue entries and six Juiced requirements, their dispositions and approval hashes.
- [x] Snapshot current planning contracts, identify pending changes, and preserve concurrent main-checkout work.
- [x] Map every feature and requirement to a rewrite owner, milestone, scope and truthful implementation state with a drift check.
- [x] Preserve early Barista B0-B4, resource-driven sessions, the unified Library, producer exclusions and recovery/qualification gates.
- [x] Add an authenticated quad-prefix read-only suitability API using the supervisor's actual reservation owner; unknown hardware/identity remains unknown.
- [x] Verify native tests, roadmap consistency, and affected existing checks; record results and remaining work.

## Scope

In: juiced/rewrite/, this ledger, and exact planning/contract updates imported from the main checkout.
Out: edits to the main checkout, merging its uncommitted C++ implementation, production state/deployment, claiming that a planning import delivers every feature.

## Current state

Phase: complete planning import and verified suitability slice
Dependencies: main plan revision 4 and its current contract snapshot
Integration: documentation imports only; main-checkout C++ changes remain independent
Next action: begin R1 identity, directory ownership, consistent restore and principal/session context against the final main-worker contracts before expanding experimental configuration.

## Decisions

| Decision | Reason | Evidence |
| --- | --- | --- |
| Source catalogue and feature dispositions remain normative | Feature titles alone can revive excluded behavior | PLATFORM-FEATURES.json F028/F031/F034/F194 |
| Align post-R0 rewrite milestones to P0-P7 | Preserve delivery ordering, especially P1 sessions and P2 Barista | PLATFORM-PLAN.md delivery sequence |
| Do not transfer platform approval to the Rust architecture | Recorded approval covers its exact bundle, not rewrite parity | PLATFORM-APPROVAL.json scope |
| Implement read-only suitability with unknown physical fit | Current Rust config lacks complete per-pool/context/artifact identities | RESOURCE-SUITABILITY.md; rewrite/service/src/config.rs |

## Findings

| Finding | Evidence | Status |
| --- | --- | --- |
| Main advanced from the rewrite base to 18db302b during integration | MAIN-SNAPSHOT.json initial_source_head/source_head | Runtime-test retry change reviewed; no C++ implementation imported |
| Revision 4 plan/catalogue/Barista hashes match recorded approval | MAIN-SNAPSHOT.json; PLATFORM-APPROVAL.json | Product plan approval only |
| Condensed rewrite roadmap omitted explicit feature-level coverage | Original rewrite/PLAN.md versus 211 features and J001-J006 | Reconcile |
| Pending docs clarify POSIX hierarchy/root aliases and assistant path handles | Imported RESOURCE-ADMISSION.md, BARISTA-OPERATIONS.md, maintenance.md | Requirements imported; Rust mechanisms remain open |
| New pending SnapshotRecovery source separates bounded observation from recovery authority | MAIN-SNAPSHOT.json implementation_leads; MAIN-REVIEW.md | Added to R1/R4 gates, not ported or accepted as Rust behavior |

## Changes

| Path or artifact | Change | Verification |
| --- | --- | --- |
| juiced/rewrite/MAIN-SNAPSHOT.json | Source hashes and committed/pending provenance for twelve documents, plus unimported recovery leads | Final refresh at 18db302b |
| juiced/rewrite/PLAN.md, MAIN-REVIEW.md, FEATURE-COVERAGE.json, scripts/ | Full 217-entry scope, owner/milestone/status mapping and exact source/approval drift checks | 4 Bun roadmap tests, 937 assertions |
| juiced/rewrite/service/ | Read-only suitability uses the reservation owner, preserves unknown identity/fit and has all prefix/auth aliases | 24 native contract tests, Clippy and formatting |
| juiced/rewrite/ui/ | Inspector check with unknown fit, retry, draft invalidation and cancellation | 9 Bun unit tests, strict production build, 15 browser journeys; 3 suitability rechecks and screenshots |
| juiced/rewrite/README.md, VERIFICATION.md | Current use and evidence boundaries | Source/local-link and whitespace checks |

## Risks and unknowns

- Main checkout is active. This snapshot records a bounded observation, not a permanent synchronization claim.
- Porting the new storage requirements needs a Rust directory-ownership implementation and native OS acceptance. Imported wording does not prove it exists.
- The Rust service cannot claim full host fit, model identity, Barista authority or cross-protocol parity from these planning updates.
- Crosswalk status remains 16 partial, 143 planned, 38 conditional and 20 excluded. This task completes the import and bounded implementation, not the full rewrite.
- All worktree changes and ledgers remain uncommitted. No main-checkout writes, deployment, production state migration or release approval transfer occurred.
