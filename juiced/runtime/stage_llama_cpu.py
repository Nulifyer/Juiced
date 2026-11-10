#!/usr/bin/env python3
"""Stage a CPU-only userspace runtime from an already verified llama.cpp payload."""

import argparse
import hashlib
import json
from pathlib import Path
import shlex
import shutil


def stage(source: Path, destination: Path, installed_root: Path):
    required = (source / "bin/llama-server", source / "bin/libllama-server-impl.so")
    if not source.is_dir() or not all(path.is_file() for path in required):
        raise ValueError("The verified runtime payload is incomplete")
    if destination.exists():
        raise FileExistsError("Runtime staging requires a fresh destination")
    if destination.resolve().is_relative_to(source.resolve()):
        raise ValueError("Runtime staging cannot write inside its source")
    if not installed_root.is_absolute():
        raise ValueError("The installed runtime root must be absolute")
    shutil.copytree(source, destination, symlinks=True)
    excluded = []
    for path in sorted((destination / "bin").glob("libggml-*")):
        if path.name.startswith(("libggml-cpu", "libggml-base")):
            continue
        excluded.append(path.name)
        path.unlink()
    rpc_server = destination / "bin/ggml-rpc-server"
    if rpc_server.exists():
        excluded.append(rpc_server.name)
        rpc_server.unlink()
    excluded.sort()
    wrapper = destination / "llama-server"
    wrapper.write_text(
        "#!/bin/sh\nexport LD_LIBRARY_PATH="
        + shlex.quote(str(installed_root / "bin"))
        + "\nexec "
        + shlex.quote(str(installed_root / "bin/llama-server"))
        + ' "$@"\n'
    )
    wrapper.chmod(0o755)
    manifest = {
        "schema": 1,
        "variant": "cpu",
        "excluded_runtime_files": excluded,
        "files": {
            str(path.relative_to(destination)): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in sorted(destination.rglob("*"))
            if path.is_file() and path != destination / "variant.json"
        },
    }
    (destination / "variant.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source", "destination", "installed-root"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    stage(args.source, args.destination, args.installed_root)


if __name__ == "__main__":
    main()
