#!/usr/bin/env python3
"""Model-free process acceptance for exclusive native cache/config/model/snapshot ownership."""

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
        shared_source = root / "shared-source"
        shared_source.mkdir()
        (shared_source / "retained.data").write_bytes(
            b"private retained source fixture"
        )
        other_model_root = root / "model cache-é-模型"
        other_snapshot_root = root / "snapshot cache-é-模型"
        settings = {
            "offline": True,
            "broadcast": False,
            "disable_model_filtering": True,
            "auto_check_model_updates": False,
            "no_fetch_executables": True,
            "extra_models_dir": str(shared_source),
        }
        for name in ("config", "other-config"):
            (root / name / "config.json").write_text(json.dumps(settings))
        children = []
        logs = []
        completed = False

        def start(cache, config, port, model_root=None, snapshot_root=None):
            log = (root / f"process-{len(logs)}.log").open("w")
            logs.append(log)
            env = {
                **os.environ,
                "HOME": str(root),
                "XDG_RUNTIME_DIR": str(root / "run"),
                "HF_HUB_CACHE": str(model_root or root / "hf"),
                "JUICED_SNAPSHOT_DIR": str(snapshot_root or root / "snapshots"),
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
            (root / "snapshots/retained.bin").write_bytes(
                b"retained private snapshot fixture"
            )
            retained = digest(root / "snapshots/retained.bin")
            for model_root, snapshot_root, label in (
                (root / "hf", root / "separate-snapshots", "model cache"),
                (root / "separate-hf", root / "snapshots", "snapshot"),
            ):
                contender, log = start(
                    root / "other-cache",
                    root / "other-config",
                    free_port(),
                    model_root,
                    snapshot_root,
                )
                check(
                    contender.wait(timeout=15) == 1,
                    f"shared external {label} rejects a competing process",
                )
                log.flush()
                check(
                    "Managed directory is already in use"
                    in Path(log.name).read_text(errors="replace"),
                    f"shared external {label} reports native ownership",
                )
                check(
                    call(port)[0] == 200
                    and digest(root / "snapshots/retained.bin") == retained,
                    f"failed external {label} activation preserves the owning service and retained snapshot",
                )
            recovered_root_port = free_port()
            recovered_root, _ = start(
                root / "other-cache",
                root / "other-config",
                recovered_root_port,
                root / "separate-hf",
                root / "separate-snapshots",
            )
            ready(recovered_root, recovered_root_port)
            check(
                stop(recovered_root, recovered_root_port) == 0,
                "failed snapshot activation releases its partially acquired model root",
            )
            independent_port = free_port()
            independent, _ = start(
                root / "other-cache",
                root / "other-config",
                independent_port,
                other_model_root,
                other_snapshot_root,
            )
            ready(independent, independent_port)
            check(
                call(port)[0] == 200 and call(independent_port)[0] == 200,
                "independent metadata roots can serve separate model-free HTTP instances",
            )
            check(
                not (shared_source / ".juiced-store.lock").exists()
                and (shared_source / "retained.data").read_bytes()
                == b"private retained source fixture",
                "independent managers do not claim or modify the shared external source directory",
            )
            original = (root / "config/config.json").read_bytes()
            try:
                call(port, "/internal/set", {"models_dir": str(other_model_root)})
                raise AssertionError("busy external root update unexpectedly succeeded")
            except urllib.error.HTTPError as error:
                data = json.load(error)
                check(
                    error.code == 503
                    and data["error"]["details"]["code"] == "storage_root_unavailable",
                    "a directory change rejects an external owner before persistence",
                )
            check(
                (root / "config/config.json").read_bytes() == original,
                "failed external-root acquisition preserves committed settings",
            )
            check(
                stop(independent, independent_port) == 0,
                "independent owner drains normally",
            )
            check(
                call(port, "/internal/set", {"models_dir": str(other_model_root)})[0]
                == 200,
                "a released external root can be applied without restarting its new owner",
            )
            (root / "other-config/config.json").write_text(
                json.dumps({**settings, "models_dir": str(root / "hf")})
            )
            previous_owner, _ = start(
                root / "other-cache",
                root / "other-config",
                independent_port,
                root / "hf",
                other_snapshot_root,
            )
            ready(previous_owner, independent_port)
            check(
                call(port)[0] == 200 and call(independent_port)[0] == 200,
                "a successful directory change releases the old model root to another manager",
            )
            check(
                stop(previous_owner, independent_port) == 0,
                "the former-root owner shuts down normally",
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
            if os.name != "nt":
                shared_source.chmod(0o755)
            output.write_text(
                json.dumps(
                    {
                        "scope": "Model-free local cache/config/external model/snapshot root ownership; no backend termination, nested-root, foreign-writer or whole-store snapshot qualification",
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
