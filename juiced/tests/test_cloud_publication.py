#!/usr/bin/env python3
"""Cloud publication failure/retry/restart using loopback discovery only."""

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import sys
import tempfile
import threading

from test_cache_admission import Fixture, PREFIXES


def acceptance(args):
    class Discovery(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            body = json.dumps({"data": [{"id": "fixture-discovered"}]}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    discovery = ThreadingHTTPServer(("127.0.0.1", 0), Discovery)
    worker = threading.Thread(target=discovery.serve_forever)
    worker.start()
    matrix = []
    try:
        for failure in ("staging", "replacement"):
            for index, operation in enumerate(
                ("install", "reinstall", "opt-in", "uninstall")
            ):
                with tempfile.TemporaryDirectory(
                    prefix="juiced-cloud-publication-"
                ) as directory:
                    root = Path(directory)
                    fixture = Fixture(args, root)
                    config = root / "config/config.json"
                    base = f"http://127.0.0.1:{discovery.server_port}/v1"
                    try:
                        fixture.start("fixture-inference", "fixture-admin")
                        fixture.ok(
                            "/v1/install",
                            {
                                "backend": "cloud",
                                "provider": "fixture",
                                "base_url": base,
                                "allow_insecure_http": operation == "uninstall",
                                **(
                                    {"api_key": "process-fixture-key"}
                                    if operation == "uninstall"
                                    else {}
                                ),
                            },
                        )
                        before = fixture.ok("/v1/system-info")["cloud"]
                        runtime = fixture.ok("/internal/config")
                        disk = json.loads(config.read_text())
                        catalog = fixture.ok("/v1/models")
                        prefix = PREFIXES[index]
                        path, body = (
                            (
                                prefix + "install",
                                {
                                    "backend": "cloud",
                                    "provider": (
                                        "new" if operation == "install" else "fixture"
                                    ),
                                    "base_url": base + "/changed",
                                },
                            )
                            if operation in ("install", "reinstall")
                            else (
                                (
                                    prefix + "cloud/auth",
                                    {
                                        "provider": "fixture",
                                        "api_key": "new-process-key",
                                        "allow_insecure_http": True,
                                    },
                                )
                                if operation == "opt-in"
                                else (
                                    prefix + "uninstall",
                                    {"backend": "cloud", "provider": "fixture"},
                                )
                            )
                        )
                        stage = root / "config/config.json.tmp"
                        if failure == "staging":
                            stage.mkdir()
                            (stage / "block").write_text("block")
                            status, _, data = fixture.call(path, body)
                            (stage / "block").unlink()
                            stage.rmdir()
                        else:
                            stage.write_text("candidate")
                            mode = config.parent.stat().st_mode
                            config.parent.chmod(0o500)
                            try:
                                status, _, data = fixture.call(path, body)
                            finally:
                                config.parent.chmod(mode)
                        assert status == 500, (failure, operation, status, data)
                        assert fixture.ok("/v1/system-info")["cloud"] == before
                        assert fixture.ok("/internal/config") == runtime
                        assert json.loads(config.read_text()) == disk
                        assert fixture.ok("/v1/models") == catalog
                        fixture.ok(path, body)
                        committed = fixture.ok("/v1/system-info")["cloud"]
                        persisted = json.loads(config.read_text())
                        assert (
                            persisted.get("cloud_providers", [])
                            == fixture.ok("/internal/config")["cloud_providers"]
                        )
                        assert (
                            "process-fixture-key" not in config.read_text()
                            and "new-process-key" not in config.read_text()
                        )
                        fixture.stop()
                        fixture.start("fixture-inference", "fixture-admin")
                        restarted = fixture.ok("/v1/system-info")["cloud"]

                        def metadata(cloud):
                            return [
                                {
                                    key: value
                                    for key, value in provider.items()
                                    if key
                                    not in (
                                        "runtime_key_set",
                                        "models_discovered",
                                        "warnings",
                                        "warning",
                                    )
                                }
                                for provider in cloud["providers"]
                            ]

                        assert metadata(committed) == metadata(restarted), (
                            committed,
                            restarted,
                        )
                        assert all(
                            not provider["runtime_key_set"]
                            for provider in restarted["providers"]
                        )
                        matrix.append(
                            {
                                "failure": failure,
                                "operation": operation,
                                "prefix": prefix,
                                "unchanged_after_failure": True,
                                "retry_restart": True,
                            }
                        )
                    except Exception:
                        fixture.log.flush()
                        print(
                            (root / "server.log").read_text()[-12000:], file=sys.stderr
                        )
                        raise
                    finally:
                        fixture.stop()
                        fixture.log.close()
    finally:
        discovery.shutdown()
        worker.join()
        discovery.server_close()
    return {
        "scope": "isolated metadata and loopback discovery; no external services or inference",
        "matrix": matrix,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    for name in ("binary", "engine", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    for name in ("binary", "engine", "model_file"):
        setattr(args, name, getattr(args, name).resolve())
    args.out.write_text(json.dumps(acceptance(args), indent=2) + "\n")
