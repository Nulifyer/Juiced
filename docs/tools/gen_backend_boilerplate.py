#!/usr/bin/env python3
"""Generate backend boilerplate (docs + config defaults) from the descriptors.

The C++ backend descriptors (src/cpp/include/lemon/backends/<stem>/<stem>.h) are
the single source of truth for what each backend is. This script boots a `lemond`
server and regenerates the committed artifacts that would otherwise be
hand-maintained:

  * Marker-delimited regions of the backend reference docs, from
    ``/system-info`` ``recipes``.
  * The whole of ``src/cpp/resources/defaults.json``, mirrored verbatim from
    ``/internal/config/defaults`` (its per-recipe blocks come from each
    descriptor's ``config_defaults()``).

Run it with ``--check`` to fail if a committed artifact drifts.

Usage:
    python docs/tools/gen_backend_boilerplate.py [--lemond PATH] [--check]

``--check`` regenerates in memory and exits non-zero if any on-disk artifact
differs, without modifying it. For the docs, only the regions between::

    <!-- BEGIN GENERATED: <id> -->
    <!-- END GENERATED: <id> -->

are rewritten; surrounding prose is left untouched.
"""

import argparse
import json
import os
import re
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
TARGET_DOC = REPO_ROOT / "juiced" / "BACKENDS.md"


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def find_lemond(explicit: str | None) -> Path:
    if explicit:
        p = Path(explicit)
        if not p.exists():
            sys.exit(f"lemond not found at {p}")
        return p
    for candidate in [
        REPO_ROOT / "build" / "lemond",
        REPO_ROOT / "build" / "lemond.exe",
    ]:
        if candidate.exists():
            return candidate
    sys.exit("Could not find a built lemond (looked in build/). Pass --lemond PATH.")


class Lemond:
    """Boots a throwaway lemond on a free port with an isolated cache dir."""

    def __init__(self, binary: Path):
        self.binary = binary
        self.port = free_port()
        self._cache = tempfile.TemporaryDirectory(prefix="lemond-docs-")
        self._proc: subprocess.Popen | None = None

    def __enter__(self):
        config_dir = Path(self._cache.name) / "config"
        config_dir.mkdir()
        (config_dir / "config.json").write_text(
            json.dumps(
                {
                    "broadcast": False,
                    "no_fetch_executables": True,
                    "auto_check_model_updates": False,
                }
            )
        )
        self._proc = subprocess.Popen(
            [
                str(self.binary.resolve()),
                self._cache.name,
                str(config_dir),
                "--host",
                "127.0.0.1",
                "--port",
                str(self.port),
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        deadline = time.time() + 60
        while time.time() < deadline:
            try:
                self._get("/api/v1/health")
                return self
            except Exception:
                if self._proc.poll() is not None:
                    sys.exit("lemond exited before becoming ready")
                time.sleep(0.5)
        self.__exit__(None, None, None)
        sys.exit("lemond did not become ready within 60s")

    def __exit__(self, *exc):
        if self._proc and self._proc.poll() is None:
            try:
                self._get("/internal/shutdown", timeout=2)
            except Exception:
                pass
            try:
                self._proc.wait(timeout=10)
            except Exception:
                self._proc.kill()
        self._cache.cleanup()

    def _get(self, path: str, timeout: float = 5):
        url = f"http://127.0.0.1:{self.port}{path}"
        req = urllib.request.Request(url)
        api_key = os.environ.get("LEMONADE_ADMIN_API_KEY") or os.environ.get(
            "LEMONADE_API_KEY"
        )
        if api_key:
            req.add_header("Authorization", f"Bearer {api_key}")
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(req, timeout=timeout) as r:
            return r.read()

    def system_info(self) -> dict:
        return json.loads(self._get("/api/v1/system-info", timeout=30))

    def config_defaults_text(self) -> str:
        # Verbatim text of the canonical default config (the server's own
        # serialization) so the committed resources/defaults.json is byte-stable.
        text = self._get("/internal/config/defaults", timeout=10).decode("utf-8")
        return text if text.endswith("\n") else text + "\n"


def md_escape(text: str) -> str:
    return str(text).replace("|", "\\|")


def render_overview(recipes: dict) -> str:
    rows = [
        "| Recipe | Name | Selectable backend | Uses ctx_size | Backends |",
        "|--------|------|--------------------|---------------|----------|",
    ]
    for recipe in sorted(recipes):
        info = recipes[recipe]
        if "display_name" not in info:
            continue  # not a descriptor-backed recipe on this run
        backends = sorted({b["backend"] for b in info.get("support", [])}) or sorted(
            info.get("backends", {})
        )
        rows.append(
            "| `{r}` | {n} | {s} | {c} | {b} |".format(
                r=recipe,
                n=md_escape(info.get("display_name", "")),
                s="yes" if info.get("selectable_backend") else "no",
                c="yes" if info.get("uses_ctx_size") else "no",
                b=", ".join(backends) if backends else "—",
            )
        )
    return "\n".join(rows)


def render_support_matrix(recipes: dict) -> str:
    rows = [
        "| Recipe | Backend | Tier | Format | OS | Device families |",
        "|--------|---------|------|--------|----|-----------------|",
    ]
    for recipe in sorted(recipes):
        info = recipes[recipe]
        for row in info.get("support", []):
            fams = []
            for d in row.get("devices", []):
                f = d.get("families") or []
                fams.append(d["device"] + (f" ({', '.join(f)})" if f else ""))
            rows.append(
                "| `{r}` | {b} | {t} | {f} | {o} | {d} |".format(
                    r=recipe,
                    b=row.get("backend", ""),
                    t=row.get("tier", "—"),
                    f=row.get("format", "—"),
                    o=", ".join(sorted(row.get("os", []))),
                    d=md_escape("; ".join(fams)) if fams else "—",
                )
            )
    return "\n".join(rows)


def render_options(recipes: dict) -> str:
    blocks = []
    for recipe in sorted(recipes):
        info = recipes[recipe]
        opts = info.get("options")
        if not opts:
            continue
        blocks.append(f"#### `{recipe}` — {info.get('display_name', recipe)}\n")
        blocks.append("| Option | CLI flag | Type | Default | Description |")
        blocks.append("|--------|----------|------|---------|-------------|")
        if info.get("uses_ctx_size"):
            blocks.append(
                "| `ctx_size` | `--ctx-size` | SIZE | -1 | Context size for the model |"
            )
        for o in opts:
            blocks.append(
                "| `{n}` | {f} | {t} | {d} | {h} |".format(
                    n=o["name"],
                    f=f"`{o['cli_flag']}`" if o.get("cli_flag") else "—",
                    t=o.get("type_name", ""),
                    d=md_escape(
                        json.dumps(o.get("default"))
                        if not isinstance(o.get("default"), str)
                        else o.get("default") or '""'
                    ),
                    h=md_escape(o.get("help", "")),
                )
            )
        blocks.append("")
    return "\n".join(blocks).rstrip()


DEFAULT_TEMPLATE = """# Backend reference

<!-- This file is generated by docs/tools/gen_backend_boilerplate.py from the C++ backend
descriptors. Do not edit the regions between the GENERATED markers by hand; run
the generator instead. Prose outside the markers is preserved. -->

## Backends

<!-- BEGIN GENERATED: backends-overview -->
<!-- END GENERATED: backends-overview -->

## Support matrix

<!-- BEGIN GENERATED: backends-matrix -->
<!-- END GENERATED: backends-matrix -->

## Recipe options

<!-- BEGIN GENERATED: backend-options -->
<!-- END GENERATED: backend-options -->
"""


def apply_sections(text: str, sections: dict[str, str]) -> str:
    for marker_id, body in sections.items():
        mid = re.escape(marker_id)
        begin = r"(<!-- BEGIN GENERATED: " + mid + r" -->)"
        end = r"(<!-- END GENERATED: " + mid + r" -->)"
        pattern = re.compile(begin + r".*?" + end, re.DOTALL)
        m = pattern.search(text)
        if not m:
            sys.exit(f"Marker region '{marker_id}' not found in target doc")

        # Inline regions (markers mid-line, e.g. inside a table cell) get no
        # surrounding newlines; block regions are wrapped on their own lines.
        inline = m.start() > 0 and text[m.start() - 1] != "\n"
        # Escape backslashes and group-ref markers in the body for re.sub.
        safe_body = body.replace("\\", "\\\\")
        sep = "" if inline else "\n"
        replacement = r"\1" + sep + safe_body + sep + r"\2"
        text = pattern.sub(replacement, text)
    return text


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--lemond", help="Path to the built lemond binary")
    ap.add_argument(
        "--check", action="store_true", help="Fail if docs are stale; do not write"
    )
    args = ap.parse_args()

    binary = find_lemond(args.lemond)
    with Lemond(binary) as server:
        info = server.system_info()
        defaults_text = server.config_defaults_text()
    recipes = info.get("recipes", {})
    if not recipes:
        sys.exit("/system-info returned no recipes")

    # Native descriptor metadata belongs with the implementation contracts.
    targets: dict = {
        TARGET_DOC: {
            "sections": {
                "backends-overview": render_overview(recipes),
                "backends-matrix": render_support_matrix(recipes),
                "backend-options": render_options(recipes),
            },
            "template": DEFAULT_TEMPLATE,
        },
    }

    # Whole-file generated artifacts (not marker-delimited): resources/defaults.json
    # is the canonical default config, mirrored verbatim from GET
    # /internal/config/defaults (per-recipe blocks come from the descriptors).
    raw_targets: dict = {
        REPO_ROOT / "src" / "cpp" / "resources" / "defaults.json": defaults_text,
    }

    stale = []
    for path, content in raw_targets.items():
        rel = path.relative_to(REPO_ROOT)
        if args.check:
            if not path.exists() or path.read_text() != content:
                stale.append(str(rel))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
            print(f"Wrote {rel}")

    for path, spec in targets.items():
        rel = path.relative_to(REPO_ROOT)
        current = path.read_text() if path.exists() else spec.get("template", "")
        if not current:
            sys.exit(f"{rel} is missing and has no template")
        updated = apply_sections(current, spec["sections"])
        if args.check:
            if not path.exists() or path.read_text() != updated:
                stale.append(str(rel))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(updated)
            print(f"Wrote {rel}")

    if args.check:
        if stale:
            sys.exit(
                "Stale generated files: "
                + ", ".join(stale)
                + "\nRun: python docs/tools/gen_backend_boilerplate.py"
            )
        print("All generated files are up to date.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
