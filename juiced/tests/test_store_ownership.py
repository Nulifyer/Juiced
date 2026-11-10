#!/usr/bin/env python3
"""Model-free process acceptance for exclusive native cache/config/model/snapshot ownership."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.request


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def run(binary, output, binding_only=False):
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
        for name in ("config", "other-config", "independent-config"):
            (root / name).mkdir(exist_ok=True)
            (root / name / "config.json").write_text(json.dumps(settings))
        children = []
        logs = []
        completed = False
        recovery_attempts = None

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

        def binding_contract():
            config = root / "config"
            snapshots = root / "snapshots"
            state_file = config / "juiced/state.json"
            marker = snapshots / ".juiced-snapshot-store.json"

            def boot(config=config, snapshots=snapshots):
                port = free_port()
                process, _ = start(
                    root / "cache", config, port, snapshot_root=snapshots
                )
                ready(process, port)
                check(
                    stop(process, port) == 0, "bound snapshot manager drains normally"
                )

            def rejected(label, config=config, snapshots=snapshots):
                state_path = config / "juiced/state.json"
                before = state_path.read_bytes() if state_path.exists() else None
                process, log = start(
                    root / "cache", config, free_port(), snapshot_root=snapshots
                )
                check(process.wait(timeout=20) == 1, label)
                log.flush()
                check(
                    "Snapshot storage binding requires recovery"
                    in Path(log.name).read_text(errors="replace"),
                    label + " reports the recovery boundary",
                )
                check(
                    (state_path.read_bytes() if state_path.exists() else None)
                    == before,
                    label + " preserves state bytes",
                )

            boot()
            initial = json.loads(state_file.read_text())
            binding = initial["snapshot_store"]
            marker_bytes = marker.read_bytes()
            check(
                binding["ready"]
                and {k: v for k, v in binding.items() if k != "ready"}
                == json.loads(marker_bytes)
                and binding["directory"] == str(snapshots.resolve())
                and binding["state_directory"] == str(state_file.parent.resolve()),
                "committed state and canonical root retain the same store identity",
            )
            boot()
            check(
                json.loads(state_file.read_text())["snapshot_store"] == binding,
                "normal restart retains the existing store identity",
            )
            moved = root / "temporarily-moved-snapshots"
            snapshots.rename(moved)
            rejected("a missing committed root is never reinitialized")
            check(not snapshots.exists(), "missing bound storage remains absent")
            moved.rename(snapshots)
            if os.name != "nt":
                alias = root / "snapshot-alias"
                alias.symlink_to(snapshots, target_is_directory=True)
                boot(snapshots=alias)
                check(
                    marker.read_bytes() == marker_bytes,
                    "canonical snapshot aliases retain the binding",
                )

            state = json.loads(state_file.read_text())
            state["snapshot_cleanup"] = {"retained": {"bytes": 7}}
            state["snapshot_writes"] = {
                "pending": {"state": "cleanup", "files": {"other.bin": 5}}
            }
            state_file.write_text(json.dumps(state))
            retained = snapshots / "retained.bin"
            retained.write_bytes(b"retain!")
            other = root / "changed-snapshots"
            other.mkdir()
            (other / "retained.bin").write_bytes(b"foreign")
            (other / "other.bin").write_bytes(b"other")
            rejected("changed snapshot root rejects old journals", snapshots=other)
            check(
                retained.read_bytes() == b"retain!"
                and (other / "retained.bin").read_bytes() == b"foreign"
                and (other / "other.bin").read_bytes() == b"other"
                and not (other / ".juiced-store.lock").exists(),
                "root mismatch rejects before target acquisition or journal effects",
            )
            absent = root / "absent-snapshots"
            rejected("changed absent root is not initialized", snapshots=absent)
            check(not absent.exists(), "mismatched root is never created")
            rejected(
                "another state directory cannot adopt a released snapshot store",
                config=root / "other-config",
            )
            check(
                marker.read_bytes() == marker_bytes
                and retained.read_bytes() == b"retain!",
                "foreign adoption preserves marker and private snapshot",
            )

            state_file.write_text(json.dumps(initial))
            marker.unlink()
            rejected("a committed binding cannot silently recreate a missing marker")
            marker.write_bytes(b"{malformed")
            rejected("a malformed marker requires recovery")
            marker.write_text(json.dumps({**json.loads(marker_bytes), "id": "foreign"}))
            rejected("a different store identity requires recovery")
            marker.write_bytes(marker_bytes)
            copied = root / "copied-config"
            shutil.copytree(config, copied)
            rejected(
                "copied state cannot migrate its snapshot authority", config=copied
            )

            saved_state = state_file.read_bytes()
            state_file.unlink()
            rejected("missing state cannot adopt an existing store marker")
            state_file.write_bytes(saved_state)
            retained.unlink()
            pending = {**initial, "snapshot_store": {**binding, "ready": False}}
            state_file.write_text(json.dumps(pending))
            boot()
            check(
                json.loads(state_file.read_text())["snapshot_store"] == binding,
                "interrupted final state publication completes with its matching marker",
            )
            state_file.write_text(json.dumps(pending))
            marker.unlink()
            boot()
            check(
                marker.read_bytes() == marker_bytes,
                "interrupted initial bind recreates only its recorded marker in an empty root",
            )
            state_file.write_text(json.dumps(pending))
            temporary = snapshots / ".juiced-snapshot-store.json.tmp"
            temporary.write_bytes(b"ambiguous interrupted write")
            rejected("ambiguous marker temporary remains quarantined")
            check(
                temporary.read_bytes() == b"ambiguous interrupted write",
                "recovery never overwrites an unresolved temporary",
            )
            temporary.unlink()
            state_file.write_text(
                json.dumps({**pending, "snapshot_cleanup": {"retained": {}}})
            )
            rejected("an unfinished binding cannot inherit cleanup intent")

            for field in ("snapshots", "snapshot_cleanup", "snapshot_writes"):
                legacy = {
                    "version": 1,
                    "revision": 0,
                    field: {"retained": {"unknown": "preserve"}},
                }
                state_file.write_text(json.dumps(legacy))
                rejected(
                    "unbound legacy " + field + " requires explicit migration",
                    snapshots=absent,
                )
                check(
                    not absent.exists(),
                    "legacy journals cannot initialize a replacement root",
                )
            state_file.write_text(json.dumps({"version": 1, "snapshots": []}))
            rejected(
                "malformed snapshot inventories require recovery", snapshots=absent
            )
            state_file.write_text(json.dumps({"version": 1, "revision": 0}))
            rejected("unbound nonempty root is never adopted", snapshots=other)
            check(
                (other / "other.bin").read_bytes() == b"other",
                "unowned snapshot payloads are retained",
            )

            unchanged = state_file.read_bytes()
            blocked = state_file.with_name("state.json.tmp")
            blocked.mkdir()
            process, log = start(
                root / "cache", config, free_port(), snapshot_root=absent
            )
            check(
                process.wait(timeout=20) == 1,
                "initial state commit failure rejects startup",
            )
            log.flush()
            check(
                "Could not save state" in Path(log.name).read_text(errors="replace")
                and state_file.read_bytes() == unchanged
                and not (absent / ".juiced-snapshot-store.json").exists(),
                "a failed binding intent cannot publish a marker or change state",
            )
            blocked.rmdir()
            boot(snapshots=absent)
            check(
                json.loads(state_file.read_text())["snapshot_store"]["ready"],
                "initial binding retries after state storage recovers",
            )

            state_file.write_text(json.dumps(initial))
            boot()
            check(
                marker.read_bytes() == marker_bytes,
                "restoring the original state and root restores service without reassignment",
            )

        try:
            if binding_only:
                binding_contract()
                completed = True
                print(f"Snapshot store binding: {len(checks)} checks passed")
                return
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
                root / "independent-config",
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
            (root / "independent-config/config.json").write_text(
                json.dumps({**settings, "models_dir": str(root / "hf")})
            )
            previous_owner, _ = start(
                root / "other-cache",
                root / "independent-config",
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
                        "scope": (
                            "Snapshot state/root binding and conservative startup recovery"
                            if binding_only
                            else "Model-free local cache/config/external model/snapshot root ownership; no backend termination, nested-root, foreign-writer or whole-store snapshot qualification"
                        ),
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
    parser.add_argument("--binding", action="store_true")
    args = parser.parse_args()
    run(args.binary.resolve(), args.out, args.binding)
