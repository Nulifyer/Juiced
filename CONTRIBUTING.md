# Contribute to Juiced

Develop Juiced's native model service and operator console in this repository.
Use the [user guides](docs/README.md) for installation and everyday operation.

## Find the implementation

| Area | Owner |
| --- | --- |
| Model API, routing, workers, and configuration | `src/cpp/` |
| Juiced management, cache, profiles, and preparation | `src/cpp/server/juiced/` |
| Current operator console | `juiced/ui/` |
| Runtime image and optional helpers | `juiced/Dockerfile`, `juiced/runtime/` |
| Native contract tests | `test/cpp/` |
| Juiced API and runtime acceptance | `juiced/tests/` |
| Product plans and implementation contracts | `juiced/*.md` |
| Current user documentation | `docs/` |

The inherited `src/app/` and `src/web-app/` applications are separate from the
Juiced console. Do not implement a Juiced screen there by mistake.
The [platform plan](juiced/PLATFORM-PLAN.md) describes staged work. Approval does
not establish implementation or device qualification.

## Build the native core

Install the platform's C++17 toolchain, CMake, native dependencies, and Git.
`setup.sh` and `setup.ps1` retain the core's platform setup paths. For a headless
Juiced build, configure explicitly from the repository root:

```bash
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_WEB_APP=OFF -DBUILD_TAURI_APP=OFF -DBUILD_TESTING=ON
cmake --build build --config Release --parallel 4 --target lemond
```

The build fetches pinned native dependencies. The Juiced Dockerfile declares the
Linux build prerequisites. A native build alone does not bundle the console,
speech helpers, or preparation environment from the release image.

Run a development server with dedicated writable cache and config directories.
Keep development work separate from production state and device owners.
The native executable remains named `lemond`; the CLI remains `lemonade`.

## Build the console

```bash
cd juiced/ui
bun install --frozen-lockfile
bun run build
```

The build writes root `web/`. The image stages it as native Juiced UI resources.
For development, `bun run dev` proxies API requests to `http://127.0.0.1:13305`.
Set `JUICED_DEV_API` to an isolated test manager when needed.

## Verify changes

Choose checks for the behavior you changed. Release CI in
`.github/workflows/juiced-release.yml` owns the portable native suite, UI workflows,
and actual-container acceptance.

```bash
python3 juiced/tests/test_api.py build/lemond
cd juiced/ui
bun test test/
bun run build
bun run test:e2e
```

Browser tests require Playwright Chromium. `JUICED_TEST_CHROMIUM` can select an
installed test browser. Native contract tests require building their targets
before running `ctest`. Use the release workflow's target list and test selection.

Real-engine, cache, concurrency, preparation, and runtime-image checks live under
`juiced/tests/`. Read each command's `--help` and prerequisites before running it.
Compilation and fixture tests do not qualify unseen models or hardware.

## Build the container

From the repository root:

```bash
docker build -f juiced/Dockerfile -t juiced:development .
```

The image uses separate build stages for native code, UI, engines, preparation,
and voice. The runtime excludes compilers and package installers. Persist weights
and state outside the image. Host drivers and firmware remain host-owned.

## Maintain documentation

Keep the README focused on what Juiced offers and where to start.
Write task instructions for end users under `docs/`. Keep native contracts and
future plans under `juiced/`. Preserve legal credits and exact compatibility
identifiers without carrying upstream branding into Juiced's product description.

```bash
python3 -m venv .venv-docs
.venv-docs/bin/pip install -r docs/assets/docs_requirements.txt
.venv-docs/bin/zensical build --strict
python3 docs/tools/check_docs.py
```

`zensical serve` can serve the documentation locally during authoring.
The site uses `docs/README.md` as its landing page. No website deployment is
required to read the guides on GitHub.

Generate descriptor-owned metadata after a backend contract change:

```bash
python3 docs/tools/gen_backend_boilerplate.py --lemond build/lemond
python3 docs/tools/gen_mcp_boilerplate.py --lemond build/lemond
```

The generators own marked regions in `juiced/BACKENDS.md` and
`juiced/MCP-TOOLS.md`, plus native `src/cpp/resources/defaults.json`.
Do not edit those generated regions manually. Run the same commands with
`--check` to detect drift. Retired upstream website, blog, demo, and community
automation files belong in Git history rather than a working-tree archive.

Backend maintenance helpers live under `tools/`. Use
`update_llamacpp_architectures.py` to regenerate the compiled architecture allowlist
from pinned sources. `update_sdcpp_versions.py` changes existing sd-cpp version
pins after runtime qualification. Each script provides `--help`.

## Submit a change

Describe the concrete problem, resulting behavior, and relevant verification.
Keep unrelated changes out of the diff. Report limits of the evidence, especially
for hardware, model quality, and live deployment claims. Include user guide
updates with changes to user-facing behavior.

Retain source licenses and notices. See [credits](docs/credits.md).
