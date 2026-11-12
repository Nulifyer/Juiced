# Native API parity inventory

The Rust daemon publishes 59 logical HTTP routes. Those routes do not cover the
native API. The frozen C++ source declares 135 application HTTP routes and four
WebSocket paths. Only 38 Rust rows match a native method and path. None has a
native wire-parity receipt in this inventory.

[inventory.json](inventory.json) owns the maintained crosswalk. Each native row
records its method, canonical path, prefix scheme, source anchors, authority,
approved feature references, current Rust equivalent, scenario files and semantic
boundary. A scenario file documents current Rust behavior. Its presence does not
prove native request or response compatibility.

| Native application HTTP status | Rows |
| --- | ---: |
| Implemented with native parity evidence | 0 |
| Partial Rust path or framework behavior | 39 |
| Planned retained path or rejection contract | 87 |
| Excluded arbitrary workflows or stored conversations | 7 |
| Conditional music/sound or 3D provider | 2 |

The 39 partial rows include 38 matching paths and the framework rejection for
`GET /mcp`. Twelve Rust routes add runtime definitions, context, endpoint metadata,
artifact management or explicit cache actions. They do not erase missing native
routes. Native registry search, model files and option reset, installation,
historical jobs, internal config, A1111 and WebSocket operations remain visible.

Native `POST /delete` deletes model weights. Rust `POST /delete` currently removes
a registration with an `expected_revision` check and retains the weights. Native
registration uses `model_name` and recipe/checkpoint definitions. Rust accepts its
own `ModelSpec`. Native recipe options and optional policy revisions also differ
from Rust's typed options and model revision checks. Matching URLs do not make
these JSON or lifecycle contracts interchangeable. Authority is recorded
separately. Operational reads require operator credentials; public model metadata
and immutable runtime definitions have narrower read contracts.

Native preparation controls combine retained history, cancellation and artifact
cleanup with explicit HTTP410 rejection of new producers. They remain planned
as mixed endpoints. Caller-authored workflow controls return501; conversation
storage routes return410. Their prohibited behavior stays excluded, while
historical workflow reads/deletion and runtime cache state remain retained.
Unsupported Ollama and A1111 stubs stay visible as rejection contracts.

The HTTP denominator counts method/path declarations once per prefix scheme.
Eighty-nine native rows use `/api/v0`, `/api/v1`, `/v0` and `/v1`. Forty-six use
fixed protocol or operational paths. Together they describe 402 concrete path
patterns. Four WebSocket paths accept both bare and quad-prefix forms, producing
20 path patterns. IPv4 and IPv6 registration does not duplicate these counts.
Forty-one auxiliary HTTP families cover static assets, SPA fallbacks, CORS and
generated GET405 guards. They are listed separately. Static mounts and wildcard
routes describe resource families, not an enumerable file list. Library-derived
HEAD handling and unmatched-path behavior need separate conformance checks.

The frozen reference is this rewrite worktree at base
`62e82570d2d5625addff0da636420f68d27b80d7`. File digests identify the inspected
bytes. The closure covers 143 registration sites in the reachable owners,
including finite operation loops, helpers and the WebSocket classifier. It does
not claim a live native server test or hardware qualification.

[main-comparison.json](main-comparison.json) captures a separate comparison with
pinned committed main `aaed79e8d84d201406a87ed1151e5ec297a200ad`. Seventeen of 35
observed paths differ from the frozen source; the four extracted registration
scopes and feature IDs remain unchanged. Working-tree bytes are excluded. The
latest refresh requires missing-versus-corrupt model-settings recovery and
redaction, fixed-cohort legacy configuration transport, and intentional Windows
shutdown semantics alongside earlier snapshot/root/startup contracts. These are
acceptance obligations, not imported implementation. Modern-store migration,
complete backup and power-loss recovery remain unqualified. No route status is
promoted by source consistency; CI does not access main or require its new files.

Run these checks from any directory:

```bash
bun /path/to/Juiced-rust-rewrite/juiced/rewrite/api-parity/inventory.ts --check
bun test /path/to/Juiced-rust-rewrite/juiced/rewrite/api-parity/inventory.test.ts
```

The validator rejects native source drift, missing registration mappings,
unknown references, changed Rust declarations or authorities, and unsupported
completion claims. Rust and documentation hashes retain their observation
snapshot; changed bytes produce a warning, while current route declarations and
referenced source anchors must still validate. Reviewed feature dispositions
must match. Update the inventory and its evidence deliberately when an owner
changes. Passing this check establishes inventory consistency, not API parity or
release readiness.
