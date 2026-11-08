#!/usr/bin/env python3
"""Loaded cloud retirement through native HTTP, using loopback inference only."""

import argparse
import concurrent.futures
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import sys
import tempfile
import threading

from test_cache_admission import Fixture


def acceptance(args):
    hold, entered, release = threading.Event(), threading.Event(), threading.Event()

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def send(self, body):
            data = json.dumps(body).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            self.send({"data": [{"id": "fixture-discovered"}]})

        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length", 0)))
            if hold.is_set():
                entered.set()
                release.wait(10)
            self.send(
                {"choices": [{"message": {"role": "assistant", "content": "fixture"}}]}
            )

    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    serving = threading.Thread(target=provider.serve_forever)
    serving.start()
    proof = {
        "scope": "isolated native HTTP lifecycle with loopback cloud discovery/inference; no external service or model-quality qualification"
    }
    try:
        with tempfile.TemporaryDirectory(prefix="juiced-cloud-lifecycle-") as directory:
            root = Path(directory)
            fixture = Fixture(args, root)
            try:
                fixture.start("fixture-inference", "fixture-admin")
                for name in ("fixture", "other"):
                    fixture.ok(
                        "/v1/install",
                        {
                            "backend": "cloud",
                            "provider": name,
                            "base_url": f"http://127.0.0.1:{provider.server_port}/v1",
                            "allow_insecure_http": True,
                            "api_key": "process-fixture",
                        },
                    )
                    model = name + ".fixture-discovered"
                    fixture.ok(
                        "/v1/juiced/policy", {"model": model, "policy": "on-demand"}
                    )
                    fixture.ok("/v1/load", {"model_name": model})

                def workers():
                    return {
                        row["model_name"]: row
                        for row in fixture.ok("/v1/health")["all_models_loaded"]
                    }

                cloud, other = "fixture.fixture-discovered", "other.fixture-discovered"
                assert cloud in workers() and other in workers()
                body = {
                    "model": cloud,
                    "messages": [{"role": "user", "content": "fixture"}],
                    "stream": False,
                    "max_tokens": 1,
                }
                before = fixture.ok("/v1/system-info")["cloud"]
                stage = root / "config/config.json.tmp"
                stage.mkdir()
                (stage / "block").write_text("block")
                status, _, _ = fixture.call(
                    "/v1/uninstall", {"backend": "cloud", "provider": "fixture"}
                )
                (stage / "block").unlink()
                stage.rmdir()
                assert (
                    status == 500 and fixture.ok("/v1/system-info")["cloud"] == before
                )
                assert cloud in workers() and other in workers()
                assert fixture.call("/v1/chat/completions", body)[0] == 200
                proof["failed_publication_preserves_worker_acquisition"] = True

                hold.set()
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
                    active = executor.submit(fixture.call, "/v1/chat/completions", body)
                    try:
                        assert entered.wait(
                            5
                        ), "controlled cloud request did not dispatch"
                        status, _, data = fixture.call(
                            "/v1/uninstall", {"backend": "cloud", "provider": "fixture"}
                        )
                        assert status == 409, (status, data)
                        assert cloud in workers() and other in workers()
                        assert fixture.ok("/v1/system-info")["cloud"] == before
                    finally:
                        release.set()
                    assert active.result()[0] == 200
                hold.clear()
                removed = fixture.ok(
                    "/v1/uninstall", {"backend": "cloud", "provider": "fixture"}
                )
                assert (
                    removed["workers_removed"] == 1 and not removed["cleanup_pending"]
                ), removed
                assert cloud not in workers() and other in workers()
                proof["busy_rejected_before_publication"] = True
                proof["idle_exact_provider_retirement"] = removed["workers_removed"]
                proof["unrelated_provider_preserved"] = True
                fixture.stop()
                fixture.start("fixture-inference", "fixture-admin")
                providers = fixture.ok("/v1/system-info")["cloud"]["providers"]
                assert [row["name"] for row in providers] == ["other"], providers
                assert not providers[0]["runtime_key_set"]
                assert cloud not in workers()
                proof["restart_preserves_committed_removal"] = True
            except Exception:
                fixture.log.flush()
                print((root / "server.log").read_text()[-12000:], file=sys.stderr)
                raise
            finally:
                release.set()
                fixture.stop()
                fixture.log.close()
    finally:
        provider.shutdown()
        serving.join()
        provider.server_close()
    return proof


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    for name in ("binary", "engine", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    for name in ("binary", "engine", "model_file"):
        setattr(args, name, getattr(args, name).resolve())
    args.out.write_text(json.dumps(acceptance(args), indent=2) + "\n")
