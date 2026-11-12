# Rust and Bun rewrite exploration

Status: complete
Owner: primary agent in explore/rust-bun-rewrite
Repository: /home/nulifyer/Projects/Juiced-rust-rewrite
Base: 62e82570d2d5625addff0da636420f68d27b80d7
Project: [rewrite plan](../../juiced/rewrite/PLAN.md)
Request source: User requested an isolated worktree, a full-project rewrite exploration, a low-dependency Bun/TypeScript/React console based on T3 Code styling and Juiced concepts, a Rust backend, more native Rust tests, planning and implementation. Another worker owns the main checkout's larger improvement plan.

## Goal

Explore the complete migration and implement a runnable, independently tested first slice. Preserve the appliance boundary and existing contracts. Do not mistake an experimental implementation for release parity.

## Acceptance checks

- [x] Separate branch and worktree based on the current committed product.
- [x] Architecture and full migration sequence map existing contracts to Rust ownership and concrete exit gates.
- [x] Rust supervises actual subprocesses, enforces authenticated quad-prefix APIs, preserves disabled defaults and revision conflicts, and forwards bounded inference.
- [x] Native Rust tests exercise lifecycle, admission, policy persistence, forwarding, failures and cancellation.
- [x] Bun builds a React console with only React and React DOM as runtime npm dependencies.
- [x] Console exercises inventory, selection, policy, lifecycle, diagnostics and temporary service tests, with accessible compact responsive controls.
- [x] Verification and outstanding parity gaps are recorded; no production state or concurrent work is modified.

## Scope

In: juiced/rewrite/, this ledger. Independent experimental service and console, architecture, fixtures, tests and migration gates.
Out: main checkout changes, production deployment, existing native registry/config mutation, deleting the current implementation, claiming full protocol/hardware parity.

## Current state

Phase: R0 exploration and working implementation verified. Full replacement remains planned.
Dependencies: Rust, Cargo and Bun are installed. Runtime engines and model weights are operator supplied.
Integration: independent experiment; current CMake, release image and console remain the production owners.
Next action: the follow-up [main-plan integration](main-plan-integration.md) has reconciled the full feature scope and added suitability. Begin its R1 identity/ownership/restore/context batch before expanding the experimental config.

## Decisions

| Decision | Reason | Evidence |
| --- | --- | --- |
| Own a separate worktree from the current committed HEAD | Preserve product context without copying the other worker's uncommitted changes | User request; git worktree list |
| Implement a working slice and enumerate remaining parity | Existing project spans many engines, protocols and platform integrations | juiced/PLATFORM-PLAN.md; juiced/RUNTIME-PROFILES.md |
| React/React DOM, native controls, plain CSS, Bun tooling | User requested low to no frontend dependencies | User request; existing juiced/ui/package.json |
| Keep subprocess engines and policy/observed-state separation | Product invariants remain valid through a language rewrite | AGENTS.md; juiced/PLATFORM-PLAN.md |
| Keep the current service as release owner through explicit parity gates | A fixture-backed experiment does not establish native platform or real-engine qualification | juiced/rewrite/PLAN.md; juiced/rewrite/VERIFICATION.md |

## Findings

| Finding | Evidence | Status |
| --- | --- | --- |
| Current console already has T3 palette, licensed primitives and bundled Inter | juiced/ui/T3-SOURCES.json; juiced/ui/src/style.css | Preserve provenance and design roles |
| Main checkout has active uncommitted C++ and resource-plan edits | Initial git status | Excluded from this worktree |

## Changes

| Path or artifact | Change | Verification |
| --- | --- | --- |
| juiced/rewrite/service/ | Subprocess supervision, admission, persisted policies, authenticated APIs and bounded diagnostics | 20 native Rust contract tests; Clippy with warnings denied |
| juiced/rewrite/ui/ | Bun/React console with local controls, responsive inspector and draft protection | 9 Bun tests, strict build, 12 browser journeys |
| juiced/rewrite/PLAN.md and README.md | Complete migration sequence and runnable preview/engine procedures | Built fixture preview and Bun development proxy smoke check |
| juiced/rewrite/VERIFICATION.md | Results, source/test links and unverified gates | Current verified worktree state |

## Risks and unknowns

- Rust does not by itself establish faster inference or lower latency. Real engine benchmarks must compare identical artifacts, launch options and workload.
- Native Windows/macOS subprocess teardown and packaging need platform runs.
- Complete runtime and protocol parity remains a multi-phase migration, governed by the rewrite plan.
- Main checkout continued receiving concurrent source/contract edits during this work. This worktree intentionally retains its committed base and needs a contract review before R1.
- One process per experimental config is required. Cross-process ownership, process-tree teardown, private-port ownership and crash-durable migration remain open gates.
- Worktree implementation and this ledger are uncommitted. No release, deployment or main-checkout integration was attempted.
