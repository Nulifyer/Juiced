# Juiced implementation contracts

This directory holds the Juiced container build, operator console source,
runtime helpers, and native implementation contracts.

Use the [project README](../README.md) for the product introduction,
[user guides](../docs/README.md) for everyday operation, and
[CONTRIBUTING.md](../CONTRIBUTING.md) for builds and verification.

## Native contracts

- [Runtime profiles and published variants](RUNTIME-PROFILES.md)
- [Capability discovery and identity](CAPABILITIES.md)
- [Resource admission](RESOURCE-ADMISSION.md)
- [Resource accounting](RESOURCE-ACCOUNTING.md)
- [Operator console design](UI-DESIGN.md)
- [Generated backend metadata](BACKENDS.md)
- [Generated MCP tool metadata](MCP-TOOLS.md)

These contracts describe implementation behavior and limits. Source-level checks,
release-image acceptance, and hardware qualification are distinct evidence.
The container build lives in `Dockerfile`; portable release gates live in
`../.github/workflows/juiced-release.yml`.

## Staged product work

The [platform plan](PLATFORM-PLAN.md), [feature disposition](PLATFORM-FEATURES.json),
and [approval record](PLATFORM-APPROVAL.json) own the staged direction.
The optional [Barista operator assistant](BARISTA.md) remains planned work.
Plan approval does not establish implementation or hardware qualification.

## Documentation ownership

Keep user how-to instructions under `docs/` and native contracts here.
Keep build and contributor procedures in the contributor guide.
[Credits](../docs/credits.md) owns provenance links. Git retains retired upstream
material; keep the working tree focused on current Juiced behavior and tooling.
