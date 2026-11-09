#!/usr/bin/env python3
"""Real Library jobs and retained CPU serving; Linux with bwrap, OpenSSL,
curl-config, Bun, Node and a local Playwright Chromium are required.
A private mount namespace extends TLS trust for the loopback registry only.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import shutil
import signal
import shlex
import socket
import ssl
import subprocess
import tempfile
import threading
import time
from unittest.mock import patch
from types import SimpleNamespace
from urllib.parse import urlsplit
import urllib.request

from test_cache_admission import Fixture

REPO = "Fixture/Library-Story-GGUF"
FILE = "Library-Story-Q4_0.gguf"
MODEL = "Library-Story-Q4_0"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def process_identity(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return int(fields[1]), fields[19], fields[0]
    except (OSError, ValueError, IndexError):
        return None


def descendants(pid):
    found = {}
    parents = {pid}
    while parents:
        next_parents = set()
        for path in Path("/proc").iterdir():
            if not path.name.isdigit():
                continue
            child = int(path.name)
            identity = process_identity(child)
            if identity and identity[0] in parents and child not in found:
                found[child] = identity[1]
                next_parents.add(child)
        parents = next_parents
    return found


class NamespacedFixture(Fixture):
    def stop(self):
        if self.process is None:
            return
        if self.process.poll() is None:
            try:
                self.call("/internal/shutdown", {})
                self.process.wait(timeout=20)
            except (OSError, AssertionError, subprocess.TimeoutExpired):
                super().stop()
                return
        self.process = None


class Repository(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, model_file, fixture, environment):
        self.data = model_file.read_bytes()
        self.hash = hashlib.sha256(self.data).hexdigest()
        self.revision = self.hash[:40]
        self.fixture, self.environment = fixture, environment
        self.mode = "fail"
        self.release = threading.Event()
        self.mutations = threading.Lock()
        self.manager = ThreadPoolExecutor(
            max_workers=1, thread_name_prefix="native-owner"
        )
        self.observations = []
        super().__init__(("127.0.0.1", 0), RegistryRequest)

    def restart(self):
        self.manager.submit(self._restart).result(timeout=60)

    def _restart(self):
        with self.mutations, patch.dict(os.environ, self.environment, clear=True):
            self.fixture.start()


class RegistryRequest(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def send(self, status, data, content_type="application/json", extra=None):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        for key, value in (extra or {}).items():
            self.send_header(key, value)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(data)
        self.close_connection = True

    def json(self, status, value):
        self.send(status, json.dumps(value).encode())

    def do_POST(self):
        if self.path != "/fixture/control":
            return self.json(404, {"error": "Unknown fixture control"})
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        try:
            if body.get("action") == "restart":
                self.server.restart()
            elif body.get("mode") in ("fail", "retry", "hold", "run"):
                self.server.mode = body["mode"]
                if body["mode"] == "run":
                    self.server.release.set()
                else:
                    self.server.release.clear()
            else:
                return self.json(400, {"error": "Unsupported fixture control"})
            self.json(200, {"mode": self.server.mode})
        except Exception as error:
            self.json(500, {"error": str(error)})

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        path = urlsplit(self.path).path
        self.server.observations.append(
            {"method": self.command, "path": path, "range": self.headers.get("Range")}
        )
        if path == "/api/models":
            return self.json(200, [{"id": REPO, "downloads": 1, "tags": ["gguf"]}])
        if path.startswith("/api/models/" + REPO):
            return self.json(
                200,
                {
                    "id": REPO,
                    "sha": self.server.revision,
                    "pipeline_tag": "text-generation",
                    "tags": ["gguf"],
                    "siblings": [
                        {
                            "rfilename": FILE,
                            "size": len(self.server.data),
                            "lfs": {
                                "oid": self.server.hash,
                                "size": len(self.server.data),
                            },
                        }
                    ],
                },
            )
        if not path.startswith("/" + REPO + "/resolve/") or not path.endswith(
            "/" + FILE
        ):
            return self.json(404, {"error": "No such fixture artifact"})
        if self.server.mode == "fail":
            return self.send(
                302, b"", extra={"Location": "http://127.0.0.1/blocked-downgrade"}
            )
        if self.server.mode == "retry":
            return self.json(503, {"error": "Controlled download failure"})
        start = 0
        requested = self.headers.get("Range", "")
        if requested:
            match = re.fullmatch(r"bytes=(\d+)-", requested)
            if not match or int(match[1]) >= len(self.server.data):
                return self.send(
                    416,
                    b"",
                    extra={"Content-Range": f"bytes */{len(self.server.data)}"},
                )
            start = int(match[1])
        self.send_response(206 if start else 200)
        self.send_header("Content-Length", str(len(self.server.data) - start))
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("X-Repo-Commit", self.server.revision)
        self.send_header("ETag", '"' + self.server.hash + '"')
        self.send_header("Connection", "close")
        if start:
            self.send_header(
                "Content-Range",
                f"bytes {start}-{len(self.server.data)-1}/{len(self.server.data)}",
            )
        self.end_headers()
        if self.command == "HEAD":
            return
        try:
            self.wfile.write(self.server.data[start : start + 1048576])
            self.wfile.flush()
            if self.server.mode == "hold" and not self.server.release.wait(60):
                return
            self.wfile.write(self.server.data[start + 1048576 :])
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            self.close_connection = True


def stop_owned_manager(registry):
    registry.manager.submit(registry.fixture.stop).result(timeout=30)
    registry.manager.shutdown(wait=True)


def acceptance(args):
    args.out.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="juiced-library-native-"))
    fixture = NamespacedFixture(args, root)
    config_file = root / "config/config.json"
    config = json.loads(config_file.read_text())
    config.update(
        {
            "no_fetch_executables": True,
            "default_model_source": "modelscope",
            "websocket_port": free_port(),
        }
    )
    config_file.write_text(json.dumps(config))
    environment = {
        key: os.environ[key]
        for key in (
            "PATH",
            "SystemRoot",
            "WINDIR",
            "COMSPEC",
            "PATHEXT",
            "TMP",
            "TEMP",
            "TMPDIR",
            "LANG",
            "LC_ALL",
        )
        if key in os.environ
    }
    environment["HOME"] = str(root)
    if args.chromium:
        environment["JUICED_TEST_CHROMIUM"] = str(args.chromium)
    certificate = root / "fixture-ca.pem"
    private_key = root / "fixture-key.pem"
    subprocess.run(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=Juiced Library Fixture",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-keyout",
            str(private_key),
            "-out",
            str(certificate),
        ],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    system_ca = Path(
        subprocess.check_output(["curl-config", "--ca"], text=True).strip()
    ).resolve(strict=True)
    bundle = root / "fixture-trust.pem"
    bundle.write_bytes(system_ca.read_bytes() + b"\n" + certificate.read_bytes())
    wrapper = root / "native-with-private-trust"
    command = [
        "bwrap",
        "--die-with-parent",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--bind",
        str(root),
        str(root),
        "--ro-bind",
        str(bundle),
        str(system_ca),
        "--",
        str(args.binary),
    ]
    wrapper.write_text("#!/bin/sh\nexec " + shlex.join(command) + ' "$@"\n')
    wrapper.chmod(0o755)
    fixture.args = SimpleNamespace(**{**vars(args), "binary": wrapper})
    environment["NODE_EXTRA_CA_CERTS"] = str(certificate)
    registry = Repository(args.model_file, fixture, environment)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(certificate, private_key)
    registry.socket = tls.wrap_socket(registry.socket, server_side=True)
    registry_url = f"https://127.0.0.1:{registry.server_port}"
    environment.update(
        {"HF_ENDPOINT": registry_url, "MODELSCOPE_ENDPOINT": registry_url}
    )
    thread = threading.Thread(target=registry.serve_forever, daemon=True)
    thread.start()
    vite = None
    ui_log = (args.out / "ui.log").open("w")
    proof = {
        "schema": 1,
        "passed": False,
        "scope": "Real native jobs, controlled registry faults/restart and tiny-model CPU serving through the source UI. Not publisher availability, quality, fit, concurrency, image or full P1 qualification.",
        "identities": {
            key: digest(getattr(args, key))
            for key in ("binary", "engine", "model_file")
        },
    }
    cleanup = False
    try:
        registry.restart()
        variants = fixture.ok(
            "/v1/pull/variants?checkpoint=" + REPO + "&source=huggingface"
        )
        assert len(variants["variants"]) == 1, variants
        port = free_port()
        base_url = f"http://127.0.0.1:{port}"
        ui = Path(__file__).resolve().parents[1] / "ui"
        env = {**environment, "JUICED_DEV_API": fixture.url}
        vite = subprocess.Popen(
            ["bun", "run", "dev", "--port", str(port), "--strictPort"],
            cwd=ui,
            env=env,
            stdout=ui_log,
            stderr=ui_log,
            start_new_session=True,
        )
        until = time.monotonic() + 20
        while True:
            try:
                with urllib.request.urlopen(base_url, timeout=1) as response:
                    assert response.status == 200
                break
            except OSError:
                assert vite.poll() is None and time.monotonic() < until
                time.sleep(0.05)
        manifest = {
            "url": base_url,
            "control_url": registry_url,
            "model": MODEL,
            "repo": REPO,
            "variant": variants["variants"][0]["name"],
            "out": str(args.out),
        }
        manifest_path = args.out / "browser-fixture.json"
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        with (args.out / "browser.log").open("w") as log:
            result = subprocess.run(
                ["node", "e2e/library-native.mjs", str(manifest_path)],
                cwd=ui,
                env=env,
                stdout=log,
                stderr=log,
                timeout=240,
            )
        assert (
            result.returncode == 0
        ), "Native browser journey failed; inspect browser.log and trace"
        row = next(
            m for m in fixture.ok("/v1/juiced/catalog")["data"] if m["id"] == MODEL
        )
        assert row["downloaded"] and row["loaded"] and row["policy"] == "on-demand", row
        assert row["registry_source"] == "huggingface", row
        downloaded = [p for p in root.rglob("*.gguf") if p.is_file()]
        assert any(
            digest(p) == proof["identities"]["model_file"] for p in downloaded
        ), downloaded
        proof["checks"] = {
            "normal_native_registry_registration": True,
            "downloaded_bytes_match_source": True,
            "disabled_until_explicit_setup": True,
            "real_cpu_worker_ready": True,
            "selected_source_preserved_with_other_default": True,
        }
        proof["passed"] = True
    finally:
        registry.release.set()
        cleanup_errors = []
        owned = descendants(fixture.process.pid) if fixture.process else {}
        try:
            catalog = (
                fixture.ok("/v1/juiced/catalog")["data"] if fixture.process else []
            )
            for model in catalog:
                if model.get("loaded"):
                    fixture.ok(
                        "/v1/juiced/operation",
                        {"model": model["id"], "action": "unload"},
                    )
            until = time.monotonic() + 20
            while fixture.process and any(
                m.get("loaded") or m.get("operation", {}).get("pending")
                for m in fixture.ok("/v1/juiced/catalog")["data"]
            ):
                assert time.monotonic() < until, "Native worker cleanup did not finish"
                time.sleep(0.05)
        except Exception as error:
            cleanup_errors.append(str(error))
        for action in (
            lambda: stop_owned_manager(registry),
            fixture.log.close,
            lambda: (
                (os.killpg(vite.pid, signal.SIGTERM), vite.wait(timeout=10))
                if vite and vite.poll() is None
                else None
            ),
            ui_log.close,
            registry.shutdown,
            registry.server_close,
            lambda: thread.join(timeout=2),
        ):
            try:
                action()
            except Exception as error:
                cleanup_errors.append(str(error))
        remaining = [
            pid
            for pid, start in owned.items()
            if (identity := process_identity(pid))
            and identity[1] == start
            and identity[2] != "Z"
        ]
        cleanup = not cleanup_errors and not remaining and fixture.process is None
        proof["cleanup_complete"] = cleanup
        proof["cleanup_errors"] = cleanup_errors
        proof["remaining_owned_processes"] = remaining
        proof["source_preserved"] = all(
            digest(getattr(args, key)) == value
            for key, value in proof["identities"].items()
        )
        proof["passed"] = proof["passed"] and cleanup and proof["source_preserved"]
        proof["registry_requests"] = registry.observations
        shutil.copyfile(root / "server.log", args.out / "server.log")
        if cleanup:
            shutil.rmtree(root)
        else:
            proof["retained_fixture"] = str(root)
        (args.out / "receipt.json").write_text(json.dumps(proof, indent=2) + "\n")
    assert proof["passed"], "Native acceptance or cleanup failed; inspect receipt.json"
    return proof


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "engine", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--chromium", type=Path)
    args = parser.parse_args()
    for name in ("binary", "engine", "model_file", "out", "chromium"):
        if getattr(args, name):
            setattr(args, name, getattr(args, name).resolve())
    print(json.dumps(acceptance(args), indent=2))
