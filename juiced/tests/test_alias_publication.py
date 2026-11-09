#!/usr/bin/env python3
"""Model-free HTTP acceptance for alias publication and restart agreement."""

import argparse
import json
from pathlib import Path
import shutil
import tempfile
import time
import urllib.error
import urllib.request

from test_model_shutdown import free_port, launch


def run(binary, output):
    checks = []

    def check(condition, label):
        checks.append({"check": label, "passed": bool(condition)})
        if not condition:
            raise AssertionError(label)

    with tempfile.TemporaryDirectory(prefix="juiced-alias-publication-") as directory:
        root = Path(directory)
        port = free_port()
        process, log = launch(binary, root, port)

        def call(path="", body=None, method=None):
            request = urllib.request.Request(
                f"http://127.0.0.1:{port}/internal/aliases{path}",
                data=None if body is None else json.dumps(body).encode(),
                headers={"Content-Type": "application/json"},
                method=method,
            )
            try:
                response = urllib.request.urlopen(request, timeout=5)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                return response.status, json.load(response)

        def ready():
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise AssertionError("alias fixture exited before readiness")
                try:
                    if call()[0] == 200:
                        return
                except OSError:
                    pass
                time.sleep(0.05)
            raise AssertionError("alias fixture did not initialize")

        def bindings():
            status, body = call()
            assert status == 200, body
            return {row["alias"]: row["target"] for row in body["aliases"]}

        def stop():
            request = urllib.request.Request(
                f"http://127.0.0.1:{port}/internal/shutdown",
                data=b"{}",
                headers={"Content-Type": "application/json"},
            )
            with urllib.request.urlopen(request, timeout=20) as response:
                assert response.status == 200
            return process.wait(timeout=25)

        target = root / "cache/aliases.json"
        retained = root / "cache/aliases.retained"
        completed = False
        try:
            ready()
            check(
                call(body={"alias": "stable", "target": "Target-A"})[0] == 200,
                "alias creation acknowledges durable publication",
            )
            previous = target.read_bytes()
            target.rename(retained)
            target.mkdir()
            (target / "keep").write_text("force replacement failure")
            for path, body, method in (
                ("", {"alias": "candidate", "target": "Target-B"}, None),
                ("", {"alias": "stable", "target": "Target-C"}, None),
                ("/stable", None, "DELETE"),
            ):
                status, result = call(path, body, method)
                check(
                    status == 500 and result["error"]["code"] == "internal_error",
                    "failed alias mutation returns a storage error",
                )
                check(
                    bindings() == {"stable": "Target-A"},
                    "failed alias mutation preserves live discovery",
                )
                check(
                    retained.read_bytes() == previous,
                    "failed replacement preserves prior committed bytes",
                )
            check(
                not list(target.parent.glob("aliases.json.tmp*")),
                "failed alias writes reclaim uncommitted staging",
            )
            shutil.rmtree(target)
            retained.rename(target)
            check(
                call(body={"alias": "candidate", "target": "Target-B"})[0] == 200,
                "failed creation retries without stale live state",
            )
            check(
                call(body={"alias": "stable", "target": "Target-C"})[0] == 200,
                "failed alias replacement remains retryable",
            )
            check(
                call("/candidate", method="DELETE")[0] == 200,
                "alias deletion acknowledges durable removal",
            )
            check(
                call(body={"alias": "bad", "target": 123})[0] == 400,
                "malformed alias input remains a client error",
            )
            before_restart = bindings()
            check(
                before_restart == {"stable": "Target-C"},
                "only successful alias changes become visible",
            )
            check(stop() == 0, "manager drains before alias restart")
            log.close()
            process, log = launch(binary, root, port)
            ready()
            check(
                bindings() == before_restart,
                "restarted alias discovery matches committed resolution",
            )
            check(
                call("/stable", method="DELETE")[0] == 200,
                "failed deletion can complete after restart",
            )
            check(
                json.loads(target.read_text()) == {},
                "successful deletion agrees with durable empty bindings",
            )
            completed = True
        finally:
            try:
                if process.poll() is None:
                    stop()
            except Exception:
                completed = False
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)
                raise
            finally:
                log.close()
                output.write_text(
                    json.dumps(
                        {
                            "scope": "Local model-free alias HTTP/filesystem faults and manager restart",
                            "completed": completed,
                            "checks": checks,
                        },
                        indent=2,
                    )
                    + "\n"
                )
                if not completed:
                    output.with_suffix(".server.log").write_text(
                        (root / "manager.log").read_text(errors="replace")
                    )
    print(f"Alias publication: {len(checks)} checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    run(args.binary.resolve(), args.out)
