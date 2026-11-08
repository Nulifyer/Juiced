#!/usr/bin/env python3
"""Actual CPU serving while failed snapshot cleanup retains its disk charge."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import time

from test_cache_admission import Fixture, MODEL


def acceptance(args):
    proof = {
        "scope": "isolated Linux CPU and filesystem failure; no hard filesystem quota, quality or accelerator qualification"
    }
    with tempfile.TemporaryDirectory(prefix="juiced-cache-disk-") as directory:
        root = Path(directory)
        fixture = Fixture(args, root)
        try:
            fixture.start()
            subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).with_name("test_engine_cache.py")),
                    "--url",
                    fixture.url,
                    "--model-file",
                    str(args.model_file),
                    "--snapshots",
                    str(root / "snapshots"),
                    "--out",
                    str(root / "initial-engine-proof.json"),
                ],
                check=True,
            )
            fixture.stop()
            state_path = root / "config/juiced/state.json"
            state = json.loads(state_path.read_text())
            state["cache"].update({"mode": "persistent", "disk_budget_gib": 1})
            state["snapshots"] = {}
            for name in ("first", "second"):
                file = root / "snapshots" / (name + ".bin")
                with file.open("wb") as stream:
                    stream.truncate(512 * 1024 * 1024)
                state["snapshots"][name] = {
                    "session": "seed-budget-only",
                    "retained": False,
                    "bytes": file.stat().st_size,
                    "last_used": 4102444800,
                }
            state_path.write_text(json.dumps(state))
            snapshots = root / "snapshots"
            permissions = stat.S_IMODE(snapshots.stat().st_mode)
            os.chmod(snapshots, stat.S_IRUSR | stat.S_IXUSR)
            try:
                fixture.start()
                status, _, _ = fixture.ask("cleanup-blocked", cache_prompt=True)
                assert status == 200, (
                    status,
                    (
                        fixture.log_path
                        if hasattr(fixture, "log_path")
                        else "inspect server.log"
                    ),
                )
                cache = fixture.ok("/v1/juiced/cache")
                assert cache["disk"]["used_bytes"] == 1024**3, cache
                assert cache["disk"]["pending_cleanup_bytes"] == 1024**3, cache
                assert not cache["pending_writes"], cache
                assert not any(
                    row.get("session") == "cleanup-blocked"
                    for row in cache["snapshots"]
                ), cache
                assert all(
                    (snapshots / (name + ".bin")).exists()
                    for name in ("first", "second")
                )
                proof["serving_continues_without_over_budget_save"] = True
                proof["undeleted_bytes_remain_charged"] = cache["disk"]
            finally:
                fixture.stop()
                os.chmod(snapshots, permissions)
            fixture.start()
            for _ in range(100):
                cache = fixture.ok("/v1/juiced/cache")
                if cache["disk"]["pending_cleanup_bytes"] == 0:
                    break
                time.sleep(0.1)
            assert cache["disk"]["used_bytes"] == 0, cache
            assert fixture.ask("after-cleanup")[0] == 200
            cache = fixture.ok("/v1/juiced/cache")
            assert any(
                row.get("session") == "after-cleanup" for row in cache["snapshots"]
            ), cache
            assert (
                not cache["pending_writes"] and cache["disk"]["used_bytes"] <= 1024**3
            ), cache
            proof["restart_cleanup_reopens_snapshot_admission"] = True
            profile = fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})
            qualification = {
                "model": MODEL,
                "fingerprint": profile["fingerprint"],
                "evidence": "Controlled real Stories15M CPU write-bound rejection fixture",
                "max_snapshot_bytes": 1,
            }
            fixture.ok("/v1/juiced/cache/qualify", qualification)
            assert fixture.ask("over-declared-bound")[0] == 200
            cache = fixture.ok("/v1/juiced/cache")
            assert cache["qualified_profiles"][profile["fingerprint"]][
                "write_bound_invalid"
            ], cache
            assert not cache["pending_writes"], cache
            assert not any(
                row.get("session") == "over-declared-bound"
                for row in cache["snapshots"]
            ), cache
            proof["bad_write_envelope_invalidates_saving_without_failing_inference"] = (
                True
            )
            qualification["max_snapshot_bytes"] = 64 * 1024 * 1024
            qualification["evidence"] = (
                "Matched full-context Stories15M CPU fixture; conservative 64 MiB write ceiling"
            )
            fixture.ok("/v1/juiced/cache/qualify", qualification)
            assert fixture.ask("after-requalification")[0] == 200
            cache = fixture.ok("/v1/juiced/cache")
            assert any(
                row.get("session") == "after-requalification"
                for row in cache["snapshots"]
            ), cache
            assert not cache["pending_writes"], cache
            proof["qualified_write_ceiling_can_be_repaired"] = True
            recovered = []
            for renamed in (False, True):
                session = "verified-previous-" + str(renamed)
                assert fixture.ask(session)[0] == 200
                fixture.stop()
                state = json.loads(state_path.read_text())
                key, previous = next(
                    (key, entry)
                    for key, entry in state["snapshots"].items()
                    if entry.get("session") == session
                )
                destination = snapshots / (key + ".bin")
                backup = snapshots / (key + ".previous.bin")
                pending = snapshots / (key + ".pending")
                candidate = {
                    **previous,
                    "checksum": hashlib.sha256(b"unavailable new snapshot").hexdigest(),
                    "bytes": len(b"unavailable new snapshot"),
                }
                if renamed:
                    destination.rename(backup)
                    destination.write_bytes(b"corrupt")
                else:
                    pending.write_bytes(b"corrupt")
                state["snapshots"].pop(key)
                state["snapshot_writes"][pending.name] = {
                    "bytes": 64 * 1024 * 1024,
                    "model": MODEL,
                    "generation": 0,
                    "owner": "controlled-sealed-publication",
                    "profile": profile["fingerprint"],
                    "state": "publishing",
                    "destination": destination.name,
                    "backup": backup.name,
                    "candidate": candidate,
                    "previous": previous,
                }
                state_path.write_text(json.dumps(state))
                fixture.start()
                fixture.ok("/v1/juiced/cache/recover", {})
                for _ in range(200):
                    cache = fixture.ok("/v1/juiced/cache")
                    if not cache["pending_writes"]:
                        break
                    time.sleep(0.05)
                assert not cache["pending_writes"], cache
                restored = next(
                    entry for entry in cache["snapshots"] if entry["id"] == key
                )
                assert restored["checksum"] == previous["checksum"], restored
                status, headers, response = fixture.ask(session)
                assert (
                    status == 200
                    and headers.get("X-Juiced-Cache") == "restored"
                    and response["timings"]["cache_n"] > 30
                ), (status, headers, response)
                recovered.append(
                    {
                        "after_rename": renamed,
                        "verified_previous_restored": True,
                        "useful_reuse": True,
                    }
                )
            proof["terminal_recovery_of_real_cpu_snapshots"] = recovered
        finally:
            fixture.stop()
            fixture.log.close()
    args.out.write_text(json.dumps(proof, indent=2) + "\n")
    print(json.dumps(proof, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "engine", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    for name in ("binary", "engine", "model_file", "out"):
        setattr(args, name, getattr(args, name).resolve())
    acceptance(args)
