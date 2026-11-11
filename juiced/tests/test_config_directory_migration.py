#!/usr/bin/env python3
"""Model-free native startup acceptance for configuration-generation migration."""

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

MODEL = "Migration-Cohort"
CANONICAL = "user." + MODEL
MANIFEST = ".juiced-configuration-migration.json"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def seed(root):
    for name in ("cache", "config", "run"):
        (root / name).mkdir(parents=True, exist_ok=True)
    documents = {
        "config.json": {
            "config_version": 2,
            "offline": True,
            "broadcast": False,
            "websocket_port": free_port(),
            "auto_check_model_updates": False,
            "no_fetch_executables": True,
            "disable_model_filtering": True,
            "log_file": str(root / "manager.log"),
            "future": "preserved",
        },
        "user_models.json": {
            MODEL: {"recipe": "llamacpp", "checkpoint": "fixture/repo:Q4_K_M"}
        },
        "recipe_options.json": {CANONICAL: {"ctx_size": 2048, "future": "kept"}},
        "execution_profiles.json": {
            CANONICAL: {
                "selected": "alternate",
                "profiles": {
                    "registered": {
                        "id": "registered",
                        "recipe": "llamacpp",
                        "options": {"ctx_size": 2048},
                        "saved_options": {"ctx_size": 2048},
                    },
                    "alternate": {
                        "id": "alternate",
                        "recipe": "vllm",
                        "options": {"ctx_size": 4096},
                    },
                },
                "settings": {"version": 1, "saved_options": {"ctx_size": 4096}},
            }
        },
        "native_helpers.json": {
            "version": 1,
            "flm": {
                "model": MODEL,
                "operation": "remove",
                "fingerprint": "0" * 64,
                "pending": True,
                "state": "dispatching",
            },
        },
        "jobs.json": {
            "version": 1,
            "jobs": [{"id": "retained", "status": "running", "future": "kept"}],
        },
    }
    for name, data in documents.items():
        (root / "cache" / name).write_text(json.dumps(data, indent=2))
    return {name: (root / "cache" / name).read_bytes() for name in documents}


def acceptance(args):
    checks, logs, children = [], [], []
    completed = False

    def check(condition, label):
        checks.append({"check": label, "passed": bool(condition)})
        if not condition:
            raise AssertionError(label)

    def same(directory, expected):
        return all(
            (directory / name).is_file() and (directory / name).read_bytes() == data
            for name, data in expected.items()
        )

    with tempfile.TemporaryDirectory(prefix="juiced-config-generation-") as temporary:
        base = Path(temporary)

        def launch(root):
            port = free_port()
            log_path = base / f"process-{len(logs)}.log"
            log = log_path.open("w")
            logs.append(log_path)
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
            try:
                process = subprocess.Popen(
                    [
                        str(args.binary),
                        str(root / "cache"),
                        str(root / "config"),
                        "--host",
                        "127.0.0.1",
                        "--port",
                        str(port),
                    ]
                    + (["--silent"] if args.embedded else []),
                    env=env,
                    stdout=log,
                    stderr=log,
                )
            except BaseException:
                log.close()
                raise
            children.append((process, log, port))
            return process, port

        def call(port, path, body=None):
            request = urllib.request.Request(
                f"http://127.0.0.1:{port}{path}",
                data=None if body is None else json.dumps(body).encode(),
                headers={"Content-Type": "application/json"},
            )
            with urllib.request.urlopen(request, timeout=3) as response:
                return json.load(response)

        def ready(process, port):
            deadline = time.monotonic() + 25
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise AssertionError("manager exited before catalog readiness")
                try:
                    catalog = call(port, "/v1/juiced/catalog")
                    if any(row["id"] == MODEL for row in catalog["data"]):
                        return
                except OSError:
                    pass
                time.sleep(0.05)
            raise AssertionError("manager did not reach catalog readiness")

        def stop(process, port):
            call(port, "/internal/shutdown", {})
            check(process.wait(timeout=25) == 0, "owned manager shutdown completes")

        try:
            root = base / "resume"
            original = seed(root)
            child = subprocess.run(
                [
                    str(args.fixture),
                    "--interrupt",
                    str(root),
                    "published:config.json",
                ],
                capture_output=True,
                timeout=20,
            )
            (base / "interruption.log").write_bytes(child.stdout + child.stderr)
            logs.append(base / "interruption.log")
            check(
                child.returncode == 91,
                "actual fixture exit interrupts a real partial publication",
            )
            intent = json.loads((root / "config" / MANIFEST).read_text())
            check(
                intent["state"] == "prepared",
                "partial target remains a prepared generation",
            )
            check(
                same(root / "cache", original),
                "source rollback files remain intact after interruption",
            )
            process, port = launch(root)
            ready(process, port)
            snapshot = call(port, f"/v1/models/{MODEL}/options")
            check(
                snapshot["execution_profile"] == "alternate"
                and snapshot["recipe"] == "vllm"
                and snapshot["saved"]["ctx_size"] == 4096
                and snapshot["effective"]["ctx_size"] == 4096,
                "native startup finishes the complete generation before serving model options",
            )
            check(
                json.loads((root / "config" / MANIFEST).read_text())["state"]
                == "active"
                and same(root / "config", original),
                "active files match the staged source before normal mutations",
            )
            check(
                call(port, "/v1/health")["native_helpers"]["flm"]["pending"],
                "native inspection retains unknown prior helper ownership",
            )
            call(port, f"/v1/models/{MODEL}/options", {"ctx_size": 8192})
            stop(process, port)
            process, port = launch(root)
            ready(process, port)
            snapshot = call(port, f"/v1/models/{MODEL}/options")
            check(
                snapshot["saved"]["ctx_size"] == 8192
                and snapshot["effective"]["ctx_size"] == 8192,
                "restart keeps later native edits instead of replaying the migration backup",
            )
            check(
                same(root / "cache", original),
                "normal service preserves the original source cohort",
            )
            stop(process, port)

            for case in ("owned-state", "conflict"):
                root = base / case
                original = seed(root)
                retained = None
                if case == "owned-state":
                    retained = root / "cache/juiced/state.json"
                    retained.parent.mkdir()
                    retained.write_text('{"snapshot_store":"retained path binding"}')
                else:
                    retained = root / "config/user_models.json"
                    retained.write_text('{"Different":{"recipe":"llamacpp"}}')
                before = retained.read_bytes()
                process, _ = launch(root)
                check(
                    process.wait(timeout=20) != 0,
                    case + " rejects startup before serving",
                )
                check(
                    same(root / "cache", original)
                    and retained.read_bytes() == before
                    and not (root / "config" / MANIFEST).exists()
                    and not (root / "config/juiced").exists(),
                    case
                    + " preserves both generations without creating replacement state",
                )
            completed = True
        finally:
            for process, log, port in children:
                if process.poll() is None:
                    try:
                        call(port, "/internal/shutdown", {})
                        process.wait(timeout=25)
                    except (OSError, subprocess.TimeoutExpired):
                        process.kill()
                        process.wait(timeout=10)
                log.close()
            args.out.parent.mkdir(parents=True, exist_ok=True)
            args.out.with_suffix(".server.log").write_bytes(
                b"\n".join(path.read_bytes() for path in logs if path.is_file())
            )
            args.out.write_text(
                json.dumps(
                    {
                        "scope": "Model-free native startup, pending-FLM containment and metadata readback; no model execution, destructive helper command or all-writer quiescence proof",
                        "binary_sha256": hashlib.sha256(
                            args.binary.read_bytes()
                        ).hexdigest(),
                        "fixture_sha256": hashlib.sha256(
                            args.fixture.read_bytes()
                        ).hexdigest(),
                        "embedded": args.embedded,
                        "checks": checks,
                        "passed": completed and all(row["passed"] for row in checks),
                    },
                    indent=2,
                )
                + "\n"
            )
    print(f"PASS: {len(checks)} configuration migration startup checks")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--fixture", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--embedded", action="store_true")
    acceptance(parser.parse_args())
