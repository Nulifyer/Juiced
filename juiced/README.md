# Juiced

A local model appliance derived from Lemonade, with a T3-derived model library
and direct multimodal playground. The model runtime owns weights, backend
processes and serving policy. Agent tools/workspaces remain in a separate stack.

## Development

- Manager: `cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DBUILD_WEB_APP=OFF -DBUILD_TAURI_APP=OFF`, then `cmake --build build -j4 --target lemond`.
- UI: `cd juiced/ui && bun install --frozen-lockfile && bun run build`.
- Native API tests: `python3 juiced/tests/test_api.py build/lemond`.
- Container: `docker build -f juiced/Dockerfile -t juiced:development .`.

The UI build produces root `web/`; the image copies it into native resources.
The C++ server serves the UI and APIs on port8080. Models are downloaded to a
persistent HF cache, not embedded into the image. No Docker socket is required.

## Initial scope

Models shows actual Loaded/Unloaded state and operation status. Loading policy
separates Hot/On demand/Disabled intent. Native C++ reconciliation recovers hot
models with bounded retries. Existing native registries and downloads supply the
catalog; the product does not duplicate model registrations.

Library provides native Hugging Face search/variant review and transfers, plus
chunked single-file GGUF uploads and approved-directory server import. Split
weights and vision companions use managed HF downloads in this release. The
transfer journal retains interrupted work across restart and retries through
the native downloader. Downloading does not imply loading.

Playground uses eligible loaded deployments for chat, speech and images. Chat
streams, retains ordinary test threads and supports images when declared by the
model. English audio is converted in-browser to16kHz WAV for the CPU recognizers.
There are no shell, browser, MCP or autonomous agent tools in this application.

Cache capability is reported explicitly. Memory reuse is engine-owned. Persistent
resume must pass save/restart/restore useful-reuse checks for the exact engine and
model profile before being enabled; a saved file alone does not qualify it.

## Provenance

Upstream Lemonade base: v2026.44.1, commit69948a7d40afbf91f6ad990d8b863c83ce5cc71d.
The root LICENSE and existing upstream history are retained. T3 UI extraction is
pinned with its MIT notice in `juiced/ui/LICENSE.t3code` and `T3-SOURCES.json`.
FUTO and Parakeet run as native subprocesses with their real backend identities;
each runtime bundle includes its source-license notices. Weights retain their
publisher licenses and are not included in the image.

## Release and deployment

The release workflow builds and tests the image before publishing to
`ghcr.io/nulifyer/juiced`. Versioned tags identify releases. Deploy with a retained
/data volume, /dev/dri for Vulkan, and a private network. An optional read-only
/library mount exposes existing GGUFs for explicit import. Keep administration
on the trusted internal route; inference and management keys use native Lemonade
key handling when configured. Do not mount another manager's writable config.

Homelab deployment/evidence is owned by the separate Homelab repository. Existing
HA and other clients are deliberately not migrated during initial user testing.
