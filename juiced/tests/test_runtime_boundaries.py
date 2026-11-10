#!/usr/bin/env python3
"""Isolated lifecycle rejection, retry ownership and WebSocket containment."""

import argparse
import base64
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import sys
import tempfile
import time

from test_cache_admission import Fixture, MODEL, PREFIXES
from admission_fixture import publish_with_admission_retry


class WebSocket:
    def __init__(self, port, model, credential):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=5)
        self.buffer = b""
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET /realtime?model={model} HTTP/1.1\r\n"
            f"Host: 127.0.0.1:{port}\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
            "Sec-WebSocket-Protocol: lemonade-realtime\r\n"
            f"Authorization: Bearer {credential}\r\n\r\n"
        )
        self.socket.sendall(request.encode())
        while b"\r\n\r\n" not in self.buffer:
            self.buffer += self.socket.recv(65536)
        headers, self.buffer = self.buffer.split(b"\r\n\r\n", 1)
        assert b"101" in headers.split(b"\r\n", 1)[0], headers
        accept = base64.b64encode(
            hashlib.sha1(
                (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()
            ).digest()
        )
        assert accept in headers, headers

    def read(self, count):
        while len(self.buffer) < count:
            chunk = self.socket.recv(65536)
            assert chunk, "WebSocket closed unexpectedly"
            self.buffer += chunk
        result, self.buffer = self.buffer[:count], self.buffer[count:]
        return result

    def receive(self):
        first, second = self.read(2)
        size = second & 127
        if size == 126:
            size = struct.unpack("!H", self.read(2))[0]
        elif size == 127:
            size = struct.unpack("!Q", self.read(8))[0]
        assert size < 65536 and not second & 128, "Invalid server frame"
        payload = self.read(size)
        assert first & 15 == 1, (first, payload)
        return json.loads(payload)

    def send(self, message):
        payload = json.dumps(message).encode()
        mask = os.urandom(4)
        header = (
            bytes([0x81, 0x80 | len(payload)])
            if len(payload) < 126
            else bytes([0x81, 0xFE]) + struct.pack("!H", len(payload))
        )
        self.socket.sendall(
            header + mask + bytes(c ^ mask[i % 4] for i, c in enumerate(payload))
        )

    def close(self):
        self.socket.close()


def acceptance(args):
    proof = {
        "scope": "isolated CPU lifecycle and loopback WebSocket; no accelerator, speech quality or latency qualification"
    }

    def publication_status(response):
        status, headers, body = response
        assert status in (200, 409, 429, 503), response
        if status == 409:
            error = body.get("error", {})
            assert (
                error.get("code", error.get("details", {}).get("code"))
                == "admission_configuration_busy"
            ), response
        if status in (429, 503):
            expected = (
                "ingress_capacity_exceeded"
                if status == 429
                else "admission_configuration_pending"
            )
            assert body["error"]["code"] == expected, body
            assert headers.get("Retry-After") == "1", headers
        return status

    def resident_publication_rejection(settings, response):
        deadline = time.monotonic() + 10
        attempts = []
        while True:
            status = publication_status(response)
            attempts.append(status)
            assert status in (409, 429), response
            if status == 409:
                return attempts
            remaining = deadline - time.monotonic()
            assert remaining > 0, ("Resident rejection retry deadline", attempts)
            time.sleep(min(float(response[1]["Retry-After"]), remaining))
            remaining = deadline - time.monotonic()
            assert remaining > 0, ("Resident rejection retry deadline", attempts)
            response = fixture.call(
                "/internal/set", {"admission": settings}, timeout=remaining
            )

    with tempfile.TemporaryDirectory(prefix="juiced-runtime-boundaries-") as directory:
        root = Path(directory)
        fixture = Fixture(args, root)
        try:
            fixture.start("fixture-inference", "fixture-admin")
            transitions = []
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
                for index in range(11):
                    if index == 0:
                        fixture.ok(
                            "/v1/juiced/policy", {"model": MODEL, "policy": "on-demand"}
                        )
                        fixture.load()
                        assert fixture.ok("/v1/juiced/admission")["resident"] == 1
                    settings = [
                        {
                            "mode": "legacy",
                            "pools": {
                                "host": {
                                    "capacity_bytes": 1024 + index * 2 + offset,
                                    "source": "concurrent publication fixture",
                                }
                            },
                        }
                        for offset in (0, 1)
                    ]
                    futures = [
                        executor.submit(
                            fixture.call, "/internal/set", {"admission": value}
                        )
                        for value in settings
                    ]
                    responses = [future.result() for future in futures]
                    statuses = [publication_status(response) for response in responses]
                    if index == 0:
                        resident_rejections = [
                            resident_publication_rejection(value, response)
                            for value, response in zip(settings, responses)
                        ]
                        fixture.ok("/v1/unload", {})
                        assert fixture.ok("/v1/juiced/admission")["resident"] == 0
                        fixture.ok(
                            "/v1/juiced/policy", {"model": MODEL, "policy": "disabled"}
                        )
                    if 200 not in statuses:
                        fixture.configure_admission(settings[-1])
                    runtime = fixture.ok("/internal/config")["admission"]
                    disk = json.loads((root / "config/config.json").read_text())[
                        "admission"
                    ]
                    ledger = fixture.ok("/v1/juiced/admission")
                    assert (
                        runtime in settings
                        and runtime == disk
                        and ledger["pools"]["host"]["capacity_bytes"]
                        == disk["pools"]["host"]["capacity_bytes"]
                    )
                    transitions.append(statuses)
            cloud_races = []
            cloud_retries = []
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
                for index in range(10):
                    admission = {
                        "mode": "legacy",
                        "pools": {
                            "host": {
                                "capacity_bytes": 4096 + index,
                                "source": "cloud/admission publication fixture",
                            }
                        },
                    }
                    updated = executor.submit(
                        fixture.call, "/internal/set", {"admission": admission}
                    )
                    provider = {
                        "backend": "cloud",
                        "provider": "fixture-cloud-" + str(index),
                        "base_url": "https://127.0.0.1:9/v1",
                    }
                    installed = executor.submit(fixture.call, "/v1/install", provider)
                    statuses = [
                        publication_status(updated.result()),
                        publication_status(installed.result()),
                    ]
                    if statuses[1] != 200:
                        publish_with_admission_retry(
                            fixture.call, "/v1/install", provider, cloud_retries
                        )
                    runtime = fixture.ok("/internal/config")
                    names = {record["name"] for record in runtime["cloud_providers"]}
                    assert provider["provider"] in names, names
                    persisted = json.loads((root / "config/config.json").read_text())
                    ledger = fixture.ok("/v1/juiced/admission")
                    assert runtime["admission"] == persisted["admission"]
                    assert runtime["cloud_providers"] == persisted["cloud_providers"]
                    assert (
                        ledger["pools"]["host"]["capacity_bytes"]
                        == persisted["admission"]["pools"]["host"]["capacity_bytes"]
                    )
                    cloud_races.append(statuses)
            disk = persisted["admission"]
            proof["cloud_admission_publication"] = {
                "paired_updates": cloud_races,
                "providers_preserved": len(runtime["cloud_providers"]),
                "publication_retries": cloud_retries,
            }
            old_revision = ledger["configuration_revision"]
            (root / "config/config.json.tmp").mkdir()
            (root / "config/config.json.tmp/fixture").write_text("keep nonempty")
            try:
                status, _, _ = fixture.call(
                    "/internal/set", {"admission": {"mode": "protected"}}
                )
                assert status == 500, status
                assert fixture.ok("/internal/config")["admission"] == disk
                assert (
                    fixture.ok("/v1/juiced/admission")["configuration_revision"]
                    == old_revision
                )
            finally:
                shutil.rmtree(root / "config/config.json.tmp")
            fixture.start("fixture-inference", "fixture-admin")
            assert fixture.ok("/internal/config")["admission"] == disk
            assert (
                fixture.ok("/v1/juiced/admission")["pools"]["host"]["capacity_bytes"]
                == disk["pools"]["host"]["capacity_bytes"]
            )
            fixture.configure_admission({})
            proof["concurrent_durable_configuration"] = {
                "paired_updates": transitions,
                "resident_busy_pair_then_successful_publication": True,
                "resident_rejection_attempts": resident_rejections,
                "write_failure_preserves_old_state": True,
                "restart_readback_matches": True,
            }
            before = fixture.ok("/v1/juiced/admission")["admitted_total"]
            for policy in ("disabled", "on-demand"):
                fixture.ok("/v1/juiced/policy", {"model": MODEL, "policy": policy})
                for prefix in PREFIXES:
                    status, _, data = fixture.call(
                        prefix + "images/upscale", {"model": MODEL, "image": "fixture"}
                    )
                    assert status == (409 if policy == "disabled" else 503), (
                        status,
                        data,
                    )
            assert fixture.ok("/v1/juiced/admission")["admitted_total"] == before
            assert not fixture.ok("/v1/health")["all_models_loaded"]
            proof["upscale_rejected_before_execution"] = True

            ws_port = fixture.ok("/v1/health")["websocket_port"]
            matrix = []
            for mode in ("paused", "protected"):
                if mode == "paused":
                    fixture.ok("/v1/juiced/admission", {"paused": True})
                else:
                    fixture.configure_admission({"mode": "protected"})
                for port in (fixture.port, ws_port):
                    for model in ("Whisper-Tiny", "Moonshine-Tiny-Streaming"):
                        connection = WebSocket(port, model, fixture.api)
                        try:
                            event = connection.receive()
                            assert event["type"] == "error", event
                            assert (
                                "paused" in event["error"]["message"].lower()
                                if mode == "paused"
                                else "qualified" in event["error"]["message"].lower()
                            ), event
                            connection.send(
                                {"type": "session.update", "session": {"model": model}}
                            )
                            event = connection.receive()
                            assert event["type"] == "error", event
                            assert fixture.process.poll() is None
                            matrix.append(
                                {
                                    "mode": mode,
                                    "port_kind": (
                                        "http" if port == fixture.port else "dedicated"
                                    ),
                                    "provider": model,
                                    "rejected": True,
                                }
                            )
                        finally:
                            connection.close()
                fixture.ok("/v1/juiced/admission", {"paused": False})
            fixture.configure_admission({})
            connection = WebSocket(fixture.port, MODEL, fixture.api)
            try:
                assert connection.receive()["type"] == "session.created"
                connection.send({"type": "session.update", "session": {"model": MODEL}})
                assert connection.receive()["type"] == "session.updated"
            finally:
                connection.close()
            proof["realtime_negative_matrix"] = matrix
            proof["websocket_recovery"] = True
            fixture.stop()

            second_model = "Stories15M-Retry-Q4_0"
            second_file = root / "retry-model.gguf"
            shutil.copyfile(args.model_file, second_file)
            fault = root / "fail-once"
            starts = root / "engine-starts.jsonl"
            slow = root / "slow-load"
            wrapper = root / "engine-wrapper"
            wrapper.write_text(
                f"#!{sys.executable}\nimport json,os,sys,time,signal\nfrom pathlib import Path\n"
                "signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))\n"
                "signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM})\n"
                f"fault=Path({str(fault)!r})\n"
                f"with open({str(starts)!r},'a') as log: log.write(json.dumps({{'pid':os.getpid(),'args':sys.argv[1:]}})+'\\n')\n"
                f"if {str(second_file)!r} in sys.argv and fault.exists():\n fault.unlink()\n sys.exit(42)\n"
                f"if {str(second_file)!r} in sys.argv and Path({str(slow)!r}).exists(): time.sleep(5)\n"
                f"os.execv({str(args.engine)!r}, [{str(args.engine)!r}, *sys.argv[1:]])\n"
            )
            wrapper.chmod(0o700)
            settings = json.loads((root / "config/config.json").read_text())
            settings.update(
                {"max_loaded_models": 2, "llamacpp": {"cpu_bin": str(wrapper)}}
            )
            (root / "config/config.json").write_text(json.dumps(settings))
            registry = json.loads((root / "config/user_models.json").read_text())
            registry[second_model] = {
                **registry[MODEL],
                "checkpoints": {"main": str(second_file)},
            }
            (root / "config/user_models.json").write_text(json.dumps(registry))
            fixture.start("fixture-inference", "fixture-admin")
            fixture.ok("/v1/juiced/policy", {"model": MODEL, "policy": "hot"})
            fixture.load()
            fixture.ok(
                "/v1/juiced/policy", {"model": second_model, "policy": "on-demand"}
            )
            fixture.ok(
                "/v1/models/" + second_model + "/options",
                {"ctx_size": 2048, "llamacpp_backend": "cpu"},
            )
            first_starts = starts.read_text().splitlines()
            fault.touch()
            fixture.ok(
                "/v1/load",
                {
                    "model_name": second_model,
                    "ctx_size": 2048,
                    "llamacpp_backend": "cpu",
                },
            )
            state = fixture.ok("/v1/juiced/admission")
            assert state["resident"] == 2 and state["active"] == 0, state
            records = [json.loads(line) for line in starts.read_text().splitlines()]
            assert len(records) == len(first_starts) + 2, records
            assert (
                sum(str(args.model_file) in row["args"] for row in records) == 1
            ), records
            assert not fault.exists()
            status, _, _ = fixture.call(
                "/internal/set", {"admission": {"mode": "protected"}}
            )
            assert status == 409, status
            assert fixture.ask("hot-after-retry", cache_prompt=False)[0] == 200
            proof["retry_owns_residency_and_preserves_hot_generation"] = True
            loaded = next(
                row
                for row in fixture.ok("/v1/health")["all_models_loaded"]
                if row["model_name"] == second_model
            )
            fixture.ok("/v1/juiced/policy", {"model": MODEL, "policy": "on-demand"})
            fixture.ok("/v1/unload", {})
            options = loaded["recipe_options"]
            options.pop("pinned", None)
            profile = {
                "checkpoint": loaded["checkpoint"],
                "options": options,
                "completion_ms": 100,
                "source": "injected slow-start deadline fixture",
                "load_bytes": {"host": 512 * 1024**2},
                "resident_bytes": {"host": 128 * 1024**2},
            }
            fixture.configure_admission(
                {
                    "mode": "legacy",
                    "pools": {
                        "host": {
                            "capacity_bytes": 2 * 1024**3,
                            "source": "fixture ceiling",
                        }
                    },
                    "models": {second_model: profile},
                }
            )
            slow.touch()
            started = time.monotonic()
            status, _, data = fixture.call(
                "/v1/load",
                {
                    "model_name": second_model,
                    "ctx_size": 2048,
                    "llamacpp_backend": "cpu",
                },
            )
            elapsed = time.monotonic() - started
            assert status == 504 and elapsed < 3, (status, elapsed, data)
            assert (
                data["error"]["details"]["code"] == "completion_deadline_exceeded"
            ), data
            state = fixture.ok("/v1/juiced/admission")
            assert (
                state["active"] == 0
                and state["resident"] == 0
                and state["uncertain"] == 0
            ), state
            assert fixture.process.poll() is None
            proof["slow_load_deadline"] = {
                "configured_ms": 100,
                "elapsed_ms": elapsed * 1000,
                "worker_stopped_and_claims_released": True,
            }

            automatic = []
            for paused in (True, False):
                fixture.ok("/v1/juiced/admission", {"paused": paused})
                for prefix in PREFIXES:
                    for endpoint, payload in (
                        (
                            "chat/completions",
                            {
                                "messages": [{"role": "user", "content": "fixture"}],
                                "max_tokens": 8,
                            },
                        ),
                        ("completions", {"prompt": "fixture", "max_tokens": 8}),
                        ("responses", {"input": "fixture", "max_output_tokens": 8}),
                    ):
                        for streaming in (False, True):
                            status, headers, data = fixture.call(
                                prefix + endpoint,
                                {"model": second_model, "stream": streaming, **payload},
                            )
                            expected = (
                                "admission_paused"
                                if paused
                                else "completion_deadline_exceeded"
                            )
                            assert (
                                status == (503 if paused else 504)
                                and data["error"]["code"] == expected
                            ), (status, data)
                            if paused:
                                assert data["error"]["details"]["retry_after"] == 1
                                assert headers.get("Retry-After") == "1", headers
                            automatic.append(
                                {
                                    "prefix": prefix,
                                    "endpoint": endpoint,
                                    "stream": streaming,
                                    "code": expected,
                                    "status": status,
                                }
                            )
            proof["automatic_load_errors"] = automatic
            gateways = []
            for paused in (True, False):
                fixture.ok("/v1/juiced/admission", {"paused": paused})
                for path, payload in (
                    (
                        "/api/chat",
                        {"messages": [{"role": "user", "content": "fixture"}]},
                    ),
                    ("/api/generate", {"prompt": "fixture"}),
                    ("/api/embed", {"input": "fixture"}),
                    ("/api/embeddings", {"prompt": "fixture"}),
                    (
                        "/v1/messages",
                        {
                            "messages": [{"role": "user", "content": "fixture"}],
                            "max_tokens": 8,
                        },
                    ),
                    (
                        "/v1/messages/count_tokens",
                        {"messages": [{"role": "user", "content": "fixture"}]},
                    ),
                ):
                    for streaming in (False, True):
                        status, headers, data = fixture.call(
                            path,
                            {"model": second_model, "stream": streaming, **payload},
                        )
                        expected = (
                            "admission_paused"
                            if paused
                            else "completion_deadline_exceeded"
                        )
                        code = (
                            data.get("code")
                            if path.startswith("/api/")
                            else data["error"].get("code")
                        )
                        assert (
                            status == (503 if paused else 504) and code == expected
                        ), (path, status, data)
                        if paused:
                            assert headers.get("Retry-After") == "1", headers
                        gateways.append(
                            {
                                "path": path,
                                "stream": streaming,
                                "status": status,
                                "code": code,
                            }
                        )
            proof["gateway_automatic_load_errors"] = gateways
            proof["positive_admission_publication"] = fixture.admission_transitions

        except Exception:
            fixture.log.flush()
            print((root / "server.log").read_text()[-10000:], file=sys.stderr)
            raise
        finally:
            fixture.stop()
            fixture.log.close()
    return proof


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    for name in ("binary", "engine", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    arguments = parser.parse_args()
    arguments.binary = arguments.binary.resolve()
    arguments.engine = arguments.engine.resolve()
    arguments.model_file = arguments.model_file.resolve()
    result = acceptance(arguments)
    arguments.out.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
