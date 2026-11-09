#!/usr/bin/env python3
"""Model-free process acceptance for exclusive cache/config ownership."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def run(binary, output):
    checks = []

    def check(condition, label):
        checks.append({"check": label, "passed": bool(condition)})
        if not condition:
            raise AssertionError(label)

    with tempfile.TemporaryDirectory(prefix="juiced-store-ownership-") as directory:
        root = Path(directory)
        for name in ("cache", "config", "other-cache", "other-config", "run"):
            (root / name).mkdir()
        settings = {
            "offline": True,
            "broadcast": False,
            "disable_model_filtering": True,
            "auto_check_model_updates": False,
            "no_fetch_executables": True,
        }
        for name in ("config", "other-config"):
            (root / name / "config.json").write_text(json.dumps(settings))
        children = []
        logs = []
        completed = False

        def start(cache, config, port):
            log = (root / f"process-{len(logs)}.log").open("w")
            logs.append(log)
            env = {
                **os.environ,
                "HOME": str(root),
                "XDG_RUNTIME_DIR": str(root / "run"),
                "HF_HUB_CACHE": str(root / "hf"),
                "LEMONADE_API_KEY": "",
                "LEMONADE_ADMIN_API_KEY": "",
            }
            process = subprocess.Popen(
                [
                    str(binary),
                    str(cache),
                    str(config),
                    "--host",
                    "127.0.0.1",
                    "--port",
                    str(port),
                ],
                env=env,
                stdout=log,
                stderr=log,
            )
            children.append((process, port))
            return process, log

        def call(port, path="/v1/health", body=None):
            request = urllib.request.Request(
                f"http://127.0.0.1:{port}{path}",
                data=None if body is None else json.dumps(body).encode(),
                headers={"Content-Type": "application/json"},
            )
            with urllib.request.urlopen(request, timeout=5) as response:
                return response.status, json.load(response)

        def ready(process, port):
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise AssertionError("manager exited before readiness")
                try:
                    if call(port)[0] == 200:
                        return
                except OSError:
                    pass
                time.sleep(0.05)
            raise AssertionError("manager did not become ready")

        def stop(process, port):
            call(port, "/internal/shutdown", {})
            return process.wait(timeout=25)

        def digest(path):
            return hashlib.sha256(path.read_bytes()).hexdigest()

        try:
            port = free_port()
            first, _ = start(root / "cache", root / "config", port)
            ready(first, port)
            check(
                call(
                    port,
                    "/internal/aliases",
                    {"alias": "lease-fixture", "target": "Target-A"},
                )[0]
                == 200,
                "owning manager can publish metadata",
            )
            before = {
                name: digest(root / name)
                for name in ("config/config.json", "cache/aliases.json")
            }
            other_before = digest(root / "other-config/config.json")
            for cache, config, label in (
                (root / "cache", root / "other-config", "cache"),
                (root / "other-cache", root / "config", "config"),
            ):
                contender, log = start(cache, config, free_port())
                check(
                    contender.wait(timeout=15) == 1,
                    f"shared {label} rejects a competing process",
                )
                log.flush()
                check(
                    "Managed directory is already in use"
                    in Path(log.name).read_text(errors="replace"),
                    f"shared {label} reports ownership instead of a port failure",
                )
                check(
                    all(digest(root / name) == value for name, value in before.items()),
                    f"rejected {label} startup preserves committed files",
                )
                check(
                    call(port)[0] == 200,
                    f"rejected {label} startup preserves the owner",
                )
            check(
                digest(root / "other-config/config.json") == other_before,
                "cache conflict rejects before unrelated configuration migration",
            )
            independent_port = free_port()
            independent, _ = start(
                root / "other-cache", root / "other-config", independent_port
            )
            ready(independent, independent_port)
            check(
                call(port)[0] == 200 and call(independent_port)[0] == 200,
                "independent metadata roots can serve separate model-free HTTP instances",
            )
            check(
                stop(independent, independent_port) == 0,
                "independent owner drains normally",
            )
            check(
                stop(first, port) == 0,
                "owning manager drains before releasing its stores",
            )
            restarted, _ = start(root / "cache", root / "config", port)
            ready(restarted, port)
            check(
                call(port, "/internal/aliases")[1]["aliases"][0]["target"]
                == "Target-A",
                "normal restart reacquires stores and retains committed aliases",
            )
            restarted.kill()
            restarted.wait(timeout=10)
            deadline = time.monotonic() + 10
            recovery_attempts = 0
            while True:
                recovery_attempts += 1
                recovered, recovery_log = start(root / "cache", root / "config", port)
                try:
                    ready(recovered, port)
                    break
                except AssertionError:
                    recovery_log.flush()
                    text = Path(recovery_log.name).read_text(errors="replace")
                    if (
                        recovered.poll() != 1
                        or "Managed directory is already in use" not in text
                        or time.monotonic() >= deadline
                    ):
                        raise
                    time.sleep(0.05)
            check(
                call(port, "/internal/aliases")[1]["aliases"][0]["target"]
                == "Target-A",
                "process termination releases model-free metadata leases",
            )
            check(
                stop(recovered, port) == 0, "recovered metadata owner drains normally"
            )
            check(
                (root / "cache/.juiced-store.lock").is_file()
                and (root / "config/.juiced-store.lock").is_file(),
                "lease control files remain after release",
            )
            completed = True
        finally:
            for process, child_port in children:
                if process.poll() is None:
                    try:
                        stop(process, child_port)
                    except (OSError, subprocess.TimeoutExpired):
                        process.kill()
                        process.wait(timeout=10)
            for log in logs:
                log.close()
            output.write_text(
                json.dumps(
                    {
                        "scope": "Model-free local process/cache/config ownership; no backend termination or whole-store snapshot qualification",
                        "completed": completed,
                        "recovery_attempts": recovery_attempts if completed else None,
                        "checks": checks,
                    },
                    indent=2,
                )
                + "\n"
            )
            if not completed:
                output.with_suffix(".server.log").write_text(
                    "\n".join(
                        Path(log.name).read_text(errors="replace") for log in logs
                    )
                )
    print(f"Store ownership: {len(checks)} checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    run(args.binary.resolve(), args.out)
