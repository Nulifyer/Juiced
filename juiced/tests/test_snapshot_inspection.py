#!/usr/bin/env python3
"""Read-only native snapshot inspection before startup and recovery."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def fingerprint(root):
    result = {}
    for path in sorted(root.rglob("*")):
        status = path.lstat()
        result[str(path.relative_to(root))] = (
            status.st_mode,
            status.st_ino,
            status.st_size,
            hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None,
        )
    return result


def acceptance(binary, output):
    checks = []
    with tempfile.TemporaryDirectory(prefix="juiced-snapshot-inspection-") as directory:
        root = Path(directory)
        cache = root / "uncreated-cache"
        config = root / "config"
        state_directory = config / "juiced"
        snapshots = state_directory / "snapshots"
        snapshots.mkdir(parents=True)
        state_file = state_directory / "state.json"
        state = {"version": 1, "revision": 1, "private_field": "do-not-export-fixture"}
        (config / "config.json").write_text("invalid runtime configuration")

        def run(label, expected, args=(), override=None, structured=True):
            state_file.write_text(json.dumps(state))
            env = dict(os.environ)
            env.pop("JUICED_SNAPSHOT_DIR", None)
            if override is not None:
                env["JUICED_SNAPSHOT_DIR"] = override
            before = fingerprint(root)
            result = subprocess.run(
                [
                    str(binary),
                    str(cache),
                    str(config),
                    "--inspect-snapshot-store",
                    *args,
                ],
                env=env,
                capture_output=True,
                text=True,
                encoding="utf-8",
                timeout=15,
            )
            assert result.returncode == expected, (
                label,
                result.returncode,
                result.stderr,
            )
            assert fingerprint(root) == before, (label, "inspection changed inputs")
            assert not cache.exists(), (label, "inspection created a cache directory")
            assert "do-not-export-fixture" not in result.stdout, label
            if structured:
                report = json.loads(result.stdout)
                assert report["read_only"] is True, (label, report)
                assert report["application_authorized"] is False, (label, report)
                assert report["writer_termination_verified"] is False, (label, report)
                assert report["validation_complete"] is (expected == 0), (label, report)
                assert not result.stderr, (label, result.stderr)
            checks.append({"check": label, "passed": True})
            return report if structured else None

        run("empty unbound inventory is observed without startup", 0)
        binding = {
            "version": 1,
            "id": "inspection-fixture",
            "state_directory": str(state_directory.resolve()),
            "directory": str(snapshots.resolve()),
            "ready": True,
        }
        state["snapshot_store"] = binding
        marker = snapshots / ".juiced-snapshot-store.json"
        marker.write_text(
            json.dumps({k: v for k, v in binding.items() if k != "ready"})
        )
        run("bound state is inspectable despite invalid runtime configuration", 0)
        key = "a" * 64
        payload = b"synthetic snapshot"
        (snapshots / (key + ".bin")).write_bytes(payload)
        state["snapshots"] = {
            key: {
                "model": "fixture-model",
                "profile": "b" * 64,
                "state": "saved",
                "bytes": len(payload),
                "checksum": hashlib.sha256(payload).hexdigest(),
            }
        }
        run("saved snapshot content is verified without a worker", 0)
        unicode_root = root / "snapshots-é-模型"
        unicode_root.mkdir()
        (unicode_root / (key + ".bin")).write_bytes(payload)
        binding["directory"] = str(unicode_root.resolve())
        (unicode_root / marker.name).write_text(
            json.dumps({k: v for k, v in binding.items() if k != "ready"})
        )
        run(
            "Unicode external snapshot roots use native path decoding",
            0,
            override=str(unicode_root),
        )
        binding["directory"] = str(snapshots.resolve())
        state["voice_writer"] = {
            "owner": "previous-fixture",
            "pending": True,
            "state": "dispatching",
        }
        report = run(
            "structurally valid writer metadata never certifies termination", 0
        )
        assert report["unverified_writers"] == 1, report
        state.pop("voice_writer")
        state.pop("snapshot_store")
        marker.unlink()
        run("legacy snapshots remain inspectable with explicit missing binding", 2)
        state["snapshot_store"] = binding
        run("missing committed marker remains an unsuccessful inspection", 2)
        marker.write_text(
            json.dumps({k: v for k, v in binding.items() if k != "ready"})
        )
        run("explicit empty snapshot root never selects the default", 2, override="")
        run(
            "missing external root is not created",
            2,
            override=str(root / "missing-root"),
        )
        for args in (
            ("--port", "54321"),
            ("--host", "127.0.0.1"),
            ("--no-broadcast",),
            ("--log-file", str(root / "must-not-exist.log")),
            ("--log-max-size-mb", "1"),
            ("--log-max-files", "0"),
            ("--watchdog-fd", "-1"),
        ):
            run(
                "inspection rejects serving option " + args[0],
                108,
                args,
                structured=False,
            )
        run(
            "inspection rejects extra positional arguments",
            109,
            ("extra",),
            structured=False,
        )
    output.write_text(
        json.dumps(
            {
                "scope": "Native read-only snapshot CLI; no migration or writer-quiescence authorization",
                "checks": checks,
            },
            indent=2,
        )
        + "\n"
    )
    print("Snapshot inspection CLI:", len(checks), "checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    acceptance(args.binary.resolve(), args.out)
