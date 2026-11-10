#!/usr/bin/env python3
"""Stage a CPU-only userspace runtime from an already verified llama.cpp payload."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil


def stage(source: Path, destination: Path, installed_root: Path):
    required = (source / "bin/llama-server", source / "bin/libllama-server-impl.so")
    if not source.is_dir() or not all(path.is_file() for path in required):
        raise ValueError("The verified runtime payload is incomplete")
    if destination.exists() or destination.is_symlink():
        raise FileExistsError("Runtime staging requires a fresh destination")
    if destination.resolve().is_relative_to(source.resolve()):
        raise ValueError("Runtime staging cannot write inside its source")
    if not installed_root.is_absolute():
        raise ValueError("The installed runtime root must be absolute")
    source_root = source.resolve(strict=True)
    aliases = []
    for path in source.rglob("*"):
        if not path.is_symlink():
            continue
        try:
            target = path.resolve(strict=True)
        except (OSError, RuntimeError) as error:
            raise ValueError(
                "Runtime file aliases must resolve inside the verified payload"
            ) from error
        if path.is_dir() or not target.is_relative_to(source_root):
            raise ValueError(
                "Runtime staging rejects linked directories and external file aliases"
            )
        aliases.append((path.relative_to(source), target.relative_to(source_root)))
    if any((source / name).is_dir() for name in ("llama-server", "variant.json")):
        raise ValueError("Generated runtime paths must be file entries")
    shutil.copytree(source, destination, symlinks=True)
    for relative, target in aliases:
        alias = destination / relative
        alias.unlink()
        alias.symlink_to(os.path.relpath(destination / target, alias.parent))
    excluded = []
    for path in sorted((destination / "bin").glob("libggml-*")):
        if path.name.startswith(("libggml-cpu", "libggml-base")):
            continue
        excluded.append(path.name)
        path.unlink()
    rpc_server = destination / "bin/ggml-rpc-server"
    if rpc_server.exists() or rpc_server.is_symlink():
        excluded.append(rpc_server.name)
        rpc_server.unlink()
    excluded.sort()
    wrapper = destination / "llama-server"
    wrapper.unlink(missing_ok=True)
    wrapper.write_text(
        "#!/bin/sh\nexport LD_LIBRARY_PATH="
        + shlex.quote(str(installed_root / "bin"))
        + "\nexec "
        + shlex.quote(str(installed_root / "bin/llama-server"))
        + ' "$@"\n'
    )
    wrapper.chmod(0o755)
    if any(path.is_symlink() and not path.is_file() for path in destination.rglob("*")):
        raise ValueError("The CPU runtime contains an unresolved file alias")
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
    marker = destination / "variant.json"
    marker.unlink(missing_ok=True)
    marker.write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source", "destination", "installed-root"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    stage(args.source, args.destination, args.installed_root)


if __name__ == "__main__":
    main()
