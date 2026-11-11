#!/usr/bin/env python3
"""Model-free alias recovery through native startup and active generations."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.request

from test_config_directory_migration import MANIFEST, free_port, seed


def acceptance(args):
    checks, logs, exits = [], [], []
    completed = False

    def check(condition, label):
        checks.append({"check": label, "passed": bool(condition)})
        if not condition:
            raise AssertionError(label)

    with tempfile.TemporaryDirectory(prefix="juiced-alias-recovery-") as temporary:
        base = Path(temporary)
        try:
            for case, original, expected in (
                ("nonobject", b'[{"retained":"private-original-marker"}]', {}),
                (
                    "mixed",
                    b'{"valid":"Target-A","bad":42,"self":"self",'
                    b'"cycleA":"cycleB","cycleB":"cycleA"}',
                    {"valid": "Target-A"},
                ),
                (
                    "active-generation",
                    b'{\r\n"private-original-marker"\x00incomplete',
                    {},
                ),
            ):
                root = base / case
                seed(root)
                cache = root / "cache"
                if case == "active-generation":
                    (cache / "aliases.json").write_text('{"prior":"Target-A"}')
                    result = subprocess.run(
                        [
                            str(args.fixture),
                            "--interrupt-relocated",
                            str(root),
                            "active",
                        ],
                        stdout=subprocess.PIPE,
                        stderr=subprocess.STDOUT,
                        timeout=20,
                        check=False,
                    )
                    logs.append(result.stdout.decode(errors="replace"))
                    check(
                        result.returncode == 91,
                        "fixture retains an active seven-role generation",
                    )
                    cache = root / "new-cache"
                target = cache / "aliases.json"
                target.write_bytes(original)
                manifest = root / "config" / MANIFEST
                committed_manifest = (
                    manifest.read_bytes() if manifest.is_file() else None
                )
                prior_backups = None
                helper_journal = None
                for restart in (False, True):
                    port = free_port()
                    env = {
                        **os.environ,
                        "HOME": str(root),
                        "XDG_RUNTIME_DIR": str(root / "run"),
                        "HF_HUB_CACHE": str(root / "hf"),
                        "JUICED_SNAPSHOT_DIR": str(root / "snapshots"),
                        "LEMONADE_API_KEY": "",
                        "LEMONADE_ADMIN_API_KEY": "",
                        "CACHE_DIRECTORY": "",
                        "STATE_DIRECTORY": "",
                        "LEMONADE_ALLOWED_ORIGINS": "",
                    }
                    log_path = root / f"process-{int(restart)}.log"
                    with log_path.open("w") as log:
                        process = subprocess.Popen(
                            [
                                str(args.binary),
                                str(cache),
                                str(root / "config"),
                                "--host",
                                "127.0.0.1",
                                "--port",
                                str(port),
                            ],
                            env=env,
                            stdout=log,
                            stderr=log,
                        )

                        def call(path="/internal/aliases", body=None):
                            request = urllib.request.Request(
                                f"http://127.0.0.1:{port}{path}",
                                data=(
                                    None if body is None else json.dumps(body).encode()
                                ),
                                headers={"Content-Type": "application/json"},
                            )
                            with urllib.request.urlopen(request, timeout=5) as response:
                                return response.status, json.load(response)

                        try:
                            deadline = time.monotonic() + 25
                            while True:
                                if process.poll() is not None:
                                    raise AssertionError(
                                        f"{case} exited before alias readiness"
                                    )
                                try:
                                    status, body = call()
                                    if status == 200:
                                        break
                                except OSError:
                                    pass
                                if time.monotonic() >= deadline:
                                    raise AssertionError(
                                        f"{case} alias readiness timed out"
                                    )
                                time.sleep(0.05)
                            actual = {
                                row["alias"]: row["target"] for row in body["aliases"]
                            }
                            check(
                                actual == expected,
                                "native alias discovery reports only committed recovered bindings",
                            )
                            backups = {
                                path.name: path.read_bytes()
                                for path in cache.glob("aliases.json.corrupted*")
                                if path.is_file()
                            }
                            check(
                                list(backups.values()) == [original],
                                "native startup preserves the exact original bytes once",
                            )
                            check(
                                target.is_file()
                                and json.loads(target.read_bytes()) == expected,
                                "native recovery retains and publishes the active alias role",
                            )
                            if restart:
                                check(
                                    backups == prior_backups,
                                    "unchanged restart reuses the original recovery backup",
                                )
                            else:
                                prior_backups = backups
                            if committed_manifest is not None:
                                check(
                                    manifest.read_bytes() == committed_manifest,
                                    "alias recovery leaves active generation ownership unchanged",
                                )
                            journal = (root / "config/native_helpers.json").read_bytes()
                            if helper_journal is None:
                                helper_journal = journal
                            check(
                                journal == helper_journal,
                                "alias recovery does not dispatch the prior helper intent",
                            )
                            if restart:
                                check(
                                    call(body={"alias": "later", "target": "Target-B"})[
                                        0
                                    ]
                                    == 200,
                                    "ordinary alias editing succeeds after recovery and restart",
                                )
                                check(
                                    json.loads(target.read_bytes())
                                    == {**expected, "later": "Target-B"}
                                    and all(
                                        (cache / name).read_bytes() == data
                                        for name, data in backups.items()
                                    ),
                                    "ordinary editing preserves original backup and durable recovered bindings",
                                )
                        finally:
                            try:
                                if process.poll() is None:
                                    call("/internal/shutdown", {})
                                    process.wait(timeout=25)
                                exits.append(process.returncode)
                                check(
                                    process.returncode == 0,
                                    "owned native manager shutdown completes",
                                )
                            finally:
                                if process.poll() is None:
                                    process.kill()
                                    process.wait(timeout=10)
                    text = log_path.read_text(errors="replace")
                    logs.append(text)
                    check(
                        "private-original-marker" not in text,
                        "alias recovery diagnostics omit private parser contents",
                    )
                check(
                    not list(cache.glob("aliases.json.tmp*"))
                    and not list(cache.glob("aliases.json.corrupted*.tmp*")),
                    "native recovery and editing leave no uncommitted staging",
                )
            completed = True
        finally:
            args.out.write_text(
                json.dumps(
                    {
                        "scope": "Actual model-free native startup, alias HTTP, seven-role generation recovery and manager restart. No inference, converters or helper dispatch.",
                        "binary_sha256": hashlib.sha256(
                            args.binary.read_bytes()
                        ).hexdigest(),
                        "fixture_sha256": hashlib.sha256(
                            args.fixture.read_bytes()
                        ).hexdigest(),
                        "completed": completed,
                        "process_exit_codes": exits,
                        "checks": checks,
                    },
                    indent=2,
                )
                + "\n"
            )
            if not completed:
                logs.extend(
                    path.read_text(errors="replace") for path in base.rglob("*.log")
                )
                args.out.with_suffix(".server.log").write_text("\n".join(logs))
    print(f"Alias recovery: {len(checks)} checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    args.fixture = args.fixture.resolve()
    acceptance(args)
