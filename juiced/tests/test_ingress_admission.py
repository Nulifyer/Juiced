#!/usr/bin/env python3
"""Real HTTP acceptance for buffered body bounds and import containment."""

import argparse
import gzip
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

if sys.platform == "linux":
    import resource

LIMIT = 8 * 1024 * 1024
PREFIXES = ("/api/v0/", "/api/v1/", "/v0/", "/v1/")


def run(binary, output):
    assertions = []
    observations = {}
    completed = False

    def check(condition, label):
        assertions.append({"check": label, "passed": bool(condition)})
        if not condition:
            raise AssertionError(label)

    with tempfile.TemporaryDirectory(prefix="juiced-ingress-") as temporary:
        root = Path(temporary)
        for name in ("cache", "config", "run"):
            (root / name).mkdir()
        config = {
            "offline": True,
            "broadcast": False,
            "auto_check_model_updates": False,
            "no_fetch_executables": True,
            "admission": {
                "ingress": {
                    "request_bytes": LIMIT,
                    "total_bytes": LIMIT + 65536,
                    "active_limit": 2,
                }
            },
        }
        (root / "config/config.json").write_text(json.dumps(config))
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        env = {
            **os.environ,
            "HOME": str(root),
            "XDG_RUNTIME_DIR": str(root / "run"),
            "HF_HUB_CACHE": str(root / "hf"),
            "JUICED_SNAPSHOT_DIR": str(root / "config/juiced/snapshots"),
            "LEMONADE_API_KEY": "ingress-fixture",
            "LEMONADE_ADMIN_API_KEY": "",
        }
        log = (root / "server.log").open("w")
        process = subprocess.Popen(
            [
                str(binary),
                str(root / "cache"),
                str(root / "config"),
                "--host",
                "127.0.0.1",
                "--port",
                str(port),
            ],
            stdout=log,
            stderr=log,
            env=env,
        )
        connections = []

        def request(path, body=None, headers=None, raw=False):
            data = body if raw else None if body is None else json.dumps(body).encode()
            query = urllib.request.Request(
                f"http://127.0.0.1:{port}{path}",
                data=data,
                headers={
                    "Authorization": "Bearer ingress-fixture",
                    "Content-Type": "application/json",
                    **(headers or {}),
                },
            )
            try:
                response = urllib.request.urlopen(query, timeout=5)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                payload = response.read()
                try:
                    result = json.loads(payload) if payload else {}
                except json.JSONDecodeError:
                    result = {"raw": payload.decode(errors="replace")}
                return response.status, result

        def state():
            return request("/v1/juiced/admission")[1]["ingress"]

        def wait_empty():
            until = time.monotonic() + 5
            while time.monotonic() < until:
                if state()["active"] == 0:
                    return True
                time.sleep(0.01)
            return False

        def pending(path, length, extra="", expect=True, authenticated=True):
            connection = socket.create_connection(("127.0.0.1", port), timeout=3)
            connections.append(connection)
            credential = (
                "Authorization: Bearer ingress-fixture\r\n" if authenticated else ""
            )
            expectation = "Expect: 100-continue\r\n" if expect else ""
            connection.sendall(
                (
                    f"POST {path} HTTP/1.1\r\nHost: localhost\r\n"
                    f"{credential}Content-Length: {length}\r\n{expectation}"
                    f"{extra}\r\n"
                ).encode()
            )
            received = b""
            while b"\r\n\r\n" not in received:
                fragment = connection.recv(8192)
                if not fragment:
                    raise RuntimeError("HTTP response ended before its headers")
                received += fragment
            return connection, received

        try:
            until = time.monotonic() + 15
            while True:
                try:
                    if request("/v1/health")[0] == 200:
                        break
                except (OSError, ValueError):
                    pass
                if process.poll() is not None or time.monotonic() > until:
                    raise RuntimeError((root / "server.log").read_text())
                time.sleep(0.05)
            for prefix in PREFIXES:
                connection, response = pending(prefix + "juiced/admission", LIMIT + 1)
                check(
                    b"413" in response.split(b"\r\n")[0],
                    prefix + "oversize rejects before body/100 continue",
                )
                connection.close()
                check(wait_empty(), prefix + "oversize leaves no ingress charge")
            connection, response = pending(
                "/v1/juiced/admission", LIMIT + 1, expect=False
            )
            check(
                b"413" in response.split(b"\r\n")[0],
                "ordinary headers reject oversize before any body arrives",
            )
            connection.close()
            connection, response = pending(
                "/v1/juiced/admission", LIMIT + 1, authenticated=False
            )
            check(
                b"401" in response.split(b"\r\n")[0],
                "authentication precedes body admission and 100 continue",
            )
            connection.close()
            first, response = pending("/v1/chat/completions", LIMIT - 1)
            check(
                response.startswith(b"HTTP/1.1 100"),
                "accepted header reserves before body transmission",
            )
            check(
                state()["reserved_body_bytes"] == LIMIT - 1,
                "incomplete body retains aggregate credit",
            )
            check(
                state()["reserved_host_bytes"] > LIMIT - 1
                and state()["physical_pool"] == "host"
                and state()["memory_estimate"]["peak_qualified"] is False,
                "input memory estimate is physical, visible and explicitly unqualified",
            )
            second, response = pending("/v1/chat/completions", 2)
            check(
                b"429" in response.split(b"\r\n")[0],
                "aggregate exhaustion rejects without reading a second body",
            )
            second.close()
            code, observed = request("/v1/juiced/admission", {"paused": False})
            check(
                code == 200
                and observed["ingress"]["reading"] == 1
                and observed["ingress"]["control_reading"] == 0,
                "operator control completes while ordinary body capacity is exhausted",
            )
            check(
                observed["ingress"]["reserved_body_bytes"] > LIMIT - 1,
                "completed control reads retain byte ownership through their handler",
            )
            first.close()
            check(wait_empty(), "disconnect releases proven local body ownership")
            first, _ = pending("/v1/chat/completions", 1)
            second, _ = pending("/v1/chat/completions", 1)
            third, response = pending("/v1/chat/completions", 1)
            check(
                b"429" in response.split(b"\r\n")[0],
                "small incomplete bodies obey concurrency bound",
            )
            third.close()
            first.close()
            second.close()
            check(wait_empty(), "all failed body reads release credit")
            for path in ("/api/chat", "/v1/messages", "/mcp", "/sdapi/v1/txt2img"):
                connection, response = pending(path, LIMIT + 1)
                check(
                    b"413" in response.split(b"\r\n")[0],
                    path + "fixed protocol shares ingress ceiling",
                )
                connection.close()
            code, job = request(
                "/v1/juiced/import/start", {"filename": "bounded.gguf", "size": LIMIT}
            )
            check(code == 201, "legacy chunk protocol remains available")
            code, progress = request(
                "/v1/juiced/import/chunk?id=" + job["id"] + "&offset=0",
                b"x" * LIMIT,
                raw=True,
            )
            check(
                code == 200 and progress["received"] == LIMIT,
                "exact 8 MiB import chunk remains supported",
            )
            check(wait_empty(), "completed maximum chunk releases ingress charge")
            native_memory = request("/v1/system-stats")[1]["memory_budget"]
            observations["native_memory"] = native_memory
            host_qualified = native_memory["available_bytes"] is not None
            if not host_qualified:
                check(
                    native_memory["containment"]
                    in ("windows-job-unqualified", "macos-process-limit-unqualified")
                    and native_memory["visible_constraints_complete"] is False,
                    "unqualified host has an explicit native containment reason",
                )
            observations["protected_positive_qualified"] = host_qualified
            observations["before_protected_activation"] = request(
                "/v1/juiced/admission"
            )[1]
            code, activation = request(
                "/internal/set",
                {
                    "admission": {
                        "mode": "protected",
                        "ingress": config["admission"]["ingress"],
                    }
                },
            )
            observations["protected_activation"] = {
                "status": code,
                "response": activation,
            }
            if host_qualified:
                check(
                    code == 200,
                    "bounded settings request can publish compatible admission configuration",
                )
                for prefix in PREFIXES:
                    for operation, body in (
                        ("start", {"filename": "denied.gguf", "size": 24}),
                        ("finish", {"id": job["id"]}),
                        ("server", {"path": "/library/denied.gguf"}),
                    ):
                        code, result = request(
                            prefix + "juiced/import/" + operation, body
                        )
                        check(
                            code == 503
                            and result["error"]["code"] == "operation_unqualified",
                            prefix + "import " + operation + " rejects before mutation",
                        )
                code, result = request(
                    "/v1/juiced/import/chunk?id=" + job["id"] + "&offset=0",
                    b"x",
                    raw=True,
                )
                check(
                    code == 503 and result["error"]["code"] == "operation_unqualified",
                    "protected chunk rejection preserves upload",
                )
            else:
                check(
                    code == 503
                    and activation["error"]["details"]["code"]
                    == "ingress_memory_unknown",
                    "unknown host rejects activation with its typed memory error",
                )
                check(
                    json.loads((root / "config/config.json").read_text())[
                        "admission"
                    ].get("mode", "legacy")
                    == "legacy",
                    "failed memory qualification preserves committed legacy configuration",
                )
            request(
                "/internal/set",
                {"admission": {"ingress": config["admission"]["ingress"]}},
            )
            request("/v1/juiced/admission", {"paused": True})
            for prefix in PREFIXES:
                code, result = request(
                    prefix + "juiced/import/start",
                    {"filename": "paused.gguf", "size": 24},
                )
                check(
                    code == 503 and result["error"]["code"] == "admission_paused",
                    prefix + "pause rejects import mutation",
                )
            request("/v1/juiced/admission", {"paused": False})
            request(
                "/internal/set",
                {
                    "admission": {
                        "mode": "protected" if host_qualified else "legacy",
                        "ingress": config["admission"]["ingress"],
                    }
                },
            )
            code, _ = request("/v1/juiced/import/cancel", {"id": job["id"]})
            check(
                code == 200,
                "cleanup cancellation remains available in the accepted mode",
            )
            check(
                not (root / "config/juiced/imports" / (job["id"] + ".part")).exists(),
                "cancellation reclaims the owned upload",
            )
            code, codec = request(
                "/mcp",
                gzip.compress(
                    json.dumps({"jsonrpc": "2.0", "method": "ping", "id": 33}).encode()
                ),
                {"Content-Encoding": "gzip"},
                raw=True,
            )
            observations["gzip_capability"] = {"status": code, "response": codec}
            gzip_supported = code == 200
            check(
                (gzip_supported and codec.get("id") == 33) or code == 415,
                "gzip either decodes the bounded RPC or rejects the unsupported encoding",
            )
            check(wait_empty(), "codec capability probe releases its ingress ownership")
            for prefix in PREFIXES:
                for encoding in ("x-juiced-fixture", "UTF-8", "gzip, identity"):
                    code, rejected = request(
                        prefix + "chat/completions",
                        b"x" * 65536,
                        {"Content-Encoding": encoding},
                        raw=True,
                    )
                    check(
                        code == 415
                        and rejected.get("error", {}).get("code")
                        == "unsupported_content_encoding",
                        "unknown or layered codings return a complete typed415 across every native prefix",
                    )
                    check(
                        wait_empty(),
                        "unsupported large-body rejection retains no input allocation",
                    )
            for repetition in range(3):
                started = time.monotonic()
                connection, received = pending(
                    "/mcp",
                    65536,
                    extra="Content-Encoding: x-juiced-fixture\r\n",
                    expect=False,
                )
                check(
                    b" 415 " in received.split(b"\r\n", 1)[0],
                    "an unsupported coding rejects without waiting for its declared body",
                )
                while True:
                    fragment = connection.recv(4096)
                    if not fragment:
                        break
                    received += fragment
                elapsed = time.monotonic() - started
                check(
                    elapsed < 1.0 and wait_empty(),
                    "a peer withholding its rejected body cannot hold close or input ownership indefinitely",
                )
                observations.setdefault("withheld_rejected_body", []).append(
                    {
                        "repetition": repetition,
                        "elapsed_ms": elapsed * 1000,
                        "status": 415,
                    }
                )
                connection.close()
            connection, duplicate = pending(
                "/mcp",
                2,
                extra="Content-Encoding: identity\r\nContent-Encoding: gzip\r\n",
            )
            check(
                b" 415 " in duplicate.split(b"\r\n", 1)[0],
                "ambiguous repeated coding headers reject before100continue or dispatch",
            )
            connection.close()
            code, _ = request(
                "/mcp",
                b"{}",
                {"Content-Encoding": "x-juiced-fixture"},
                raw=True,
            )
            check(
                code == 415,
                "unknown content encoding rejects before native body dispatch",
            )
            check(
                wait_empty(), "unsupported codec rejection releases ingress ownership"
            )
            compressed = gzip.compress(b"x" * (LIMIT + 1))
            code, result = request(
                "/v1/chat/completions",
                compressed,
                {"Content-Encoding": "gzip"},
                raw=True,
            )
            observations["oversized_gzip"] = {"status": code, "response": result}
            check(
                code == (413 if gzip_supported else 415),
                "oversized gzip remains bounded or rejects its unavailable codec",
            )
            check(wait_empty(), "decompression failure releases ingress charge")
            deep = ("[" * 65 + "0" + "]" * 65).encode()
            wide = json.dumps({"items": [0] * 32769}).encode()
            for prefix in PREFIXES:
                for endpoint in (
                    "chat/completions",
                    "juiced/admission",
                    "juiced/import/start",
                    "models/Missing/options",
                ):
                    code, result = request(prefix + endpoint, deep, raw=True)
                    check(
                        code == 413
                        and result["error"]["code"]
                        == "ingress_structure_limit_exceeded",
                        prefix
                        + endpoint
                        + "rejects deep JSON before native allocation",
                    )
            for endpoint in ("/api/chat", "/v1/messages", "/mcp", "/sdapi/v1/txt2img"):
                code, result = request(endpoint, wide, raw=True)
                check(
                    code == 413
                    and result["error"]["code"] == "ingress_structure_limit_exceeded",
                    endpoint
                    + "rejects excessive JSON values through the shared handler",
                )
            code, result = request(
                "/mcp", {"jsonrpc": "2.0", "method": "ping", "id": 1}
            )
            check(
                code == 200 and result.get("id") == 1,
                "bounded ordinary gateway JSON remains accepted",
            )
            request(
                "/internal/set",
                {"admission": {"ingress": config["admission"]["ingress"]}},
            )
            code, upload = request(
                "/v1/juiced/import/start", {"filename": "raw.gguf", "size": len(deep)}
            )
            check(code == 201, "raw chunk structural fixture can create an upload")
            code, result = request(
                "/v1/juiced/import/chunk?id=" + upload["id"] + "&offset=0",
                deep,
                raw=True,
            )
            check(
                code == 200 and result["received"] == len(deep),
                "raw import bytes bypass JSON structure interpretation",
            )
            request("/v1/juiced/import/cancel", {"id": upload["id"]})
            check(
                wait_empty(),
                "structural rejection and raw chunk completion release byte ownership",
            )
            if host_qualified:
                memory_settings = {
                    "mode": "protected",
                    "pools": {
                        "host": {
                            "capacity_bytes": 4 * 1024 * 1024,
                            "essential_headroom_bytes": 1024 * 1024,
                            "source": "controlled HTTP input-memory fixture",
                        }
                    },
                }
                code, _ = request("/internal/set", {"admission": memory_settings})
                check(
                    code == 200,
                    "protected host ceiling activates with compatible input ownership",
                )
                connection, response = pending("/v1/chat/completions", 10000)
                check(
                    b"429" in response.split(b"\r\n")[0],
                    "physical host exhaustion rejects before body transmission",
                )
                connection.close()
                check(wait_empty(), "physical rejection leaves no partial claim")
                connection, response = pending("/v1/chat/completions", 2000)
                check(
                    response.startswith(b"HTTP/1.1 100"),
                    "smaller input fits the host pool",
                )
                observed = request("/v1/juiced/admission")[1]
                check(
                    observed["pools"]["host"]["reserved_bytes"]
                    == observed["ingress"]["reserved_host_bytes"]
                    and observed["ingress"]["reserved_host_bytes"] > 2000,
                    "HTTP input and model accounting expose one shared host charge",
                )
                code, result = request(
                    "/internal/set",
                    {
                        "admission": {
                            **memory_settings,
                            "pools": {
                                "host": {
                                    "capacity_bytes": 500000,
                                    "source": "too small for the live request",
                                }
                            },
                        }
                    },
                )
                check(
                    code == 409
                    and result["error"]["details"]["code"]
                    == "admission_configuration_busy",
                    "configuration cannot drop a live physical input charge",
                )
                check(
                    request("/v1/juiced/admission", {"paused": True})[0] == 200,
                    "bounded controls remain usable beside a charged ordinary input",
                )
                connection.close()
                check(wait_empty(), "failed local read releases physical host memory")
                request("/v1/juiced/admission", {"paused": False})
            if sys.platform == "linux" and host_qualified:
                for allocation_limit in (resource.RLIMIT_AS, resource.RLIMIT_DATA):
                    check(
                        request("/internal/set", {"admission": {"mode": "legacy"}})[0]
                        == 200,
                        "legacy mode activates before isolated process-limit transition",
                    )
                    before = request("/v1/juiced/admission")[1]
                    original = resource.prlimit(process.pid, allocation_limit)
                    finite = (
                        (1 << 62)
                        if original[1] == resource.RLIM_INFINITY
                        else original[1]
                    )
                    try:
                        resource.prlimit(
                            process.pid, allocation_limit, (finite, original[1])
                        )
                        check(
                            request(
                                "/mcp", {"jsonrpc": "2.0", "method": "ping", "id": 91}
                            )[0]
                            == 200,
                            "finite process limit preserves legacy request behavior",
                        )
                        code, result = request(
                            "/internal/set", {"admission": {"mode": "protected"}}
                        )
                        check(
                            code == 503
                            and result["error"]["details"]["code"]
                            == "ingress_memory_unknown",
                            "finite process limit rejects protected activation with typed unknown capacity",
                        )
                        after = request("/v1/juiced/admission")[1]
                        check(
                            after["mode"] == "legacy"
                            and after["configuration_revision"]
                            == before["configuration_revision"]
                            and json.loads((root / "config/config.json").read_text())[
                                "admission"
                            ]["mode"]
                            == "legacy",
                            "failed process-limit activation preserves committed mode and revision",
                        )
                    finally:
                        resource.prlimit(process.pid, allocation_limit, original)
                    check(
                        request("/internal/set", {"admission": {"mode": "protected"}})[
                            0
                        ]
                        == 200,
                        "restored unlimited process constraints permit protected activation",
                    )
                    try:
                        resource.prlimit(
                            process.pid, allocation_limit, (finite, original[1])
                        )
                        for path in [
                            prefix + "chat/completions" for prefix in PREFIXES
                        ] + ["/api/chat", "/v1/messages", "/mcp", "/sdapi/v1/txt2img"]:
                            connection, response = pending(path, 2)
                            check(
                                response.startswith(b"HTTP/1.1 503"),
                                path
                                + " rejects finite process capacity before body or100 continue",
                            )
                            connection.close()
                            code, result = request(path, {})
                            check(
                                code == 503
                                and result["error"]["code"] == "ingress_memory_unknown",
                                path
                                + " rejects finite process capacity before handler dispatch",
                            )
                        check(
                            wait_empty(),
                            "finite process-limit rejection retains no input claims",
                        )
                    finally:
                        resource.prlimit(process.pid, allocation_limit, original)
            request("/internal/shutdown", {})
            check(
                process.wait(timeout=20) == 0,
                "manager drains before unknown-memory containment test",
            )
            config["admission"]["mode"] = "protected"
            if host_qualified:
                config["admission"]["pools"] = {
                    "host": {
                        "capacity_bytes": None,
                        "source": "explicit unknown HTTP fixture",
                    }
                }
            observations["unknown_memory_cause"] = (
                "configured-host-capacity" if host_qualified else "native-containment"
            )
            (root / "config/config.json").write_text(json.dumps(config))
            process = subprocess.Popen(
                [
                    str(binary),
                    str(root / "cache"),
                    str(root / "config"),
                    "--host",
                    "127.0.0.1",
                    "--port",
                    str(port),
                ],
                stdout=log,
                stderr=log,
                env=env,
            )
            until = time.monotonic() + 15
            while True:
                try:
                    if request("/v1/health")[0] == 200:
                        break
                except OSError:
                    pass
                if process.poll() is not None or time.monotonic() >= until:
                    raise AssertionError(
                        "protected unknown-memory manager did not start"
                    )
                time.sleep(0.05)
            for path in [prefix + "chat/completions" for prefix in PREFIXES] + [
                "/api/chat",
                "/v1/messages",
                "/mcp",
                "/sdapi/v1/txt2img",
                "/internal/set",
            ]:
                connection, response = pending(path, 2)
                check(
                    b"503" in response.split(b"\r\n")[0],
                    path + "rejects unknown memory before body or100 continue",
                )
                connection.close()
                code, result = request(path, {})
                check(
                    code == 503 and result["error"]["code"] == "ingress_memory_unknown",
                    path
                    + "rejects body dispatch when native host constraints are unqualified",
                )
            check(
                state()["active"] == 0,
                "unknown-memory rejection retains no input claims",
            )
            check(
                json.loads((root / "config/config.json").read_text())["admission"][
                    "mode"
                ]
                == "protected",
                "rejected settings body cannot bypass protected unknown-memory policy",
            )
            completed = True

        finally:
            for connection in connections:
                connection.close()
            process.terminate()
            try:
                process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            log.close()
            output.write_text(
                json.dumps(
                    {
                        "checks": assertions,
                        "observations": observations,
                        "completed": completed,
                        "scope": "Model-free local HTTP body/import acceptance",
                        "server_exit": process.returncode,
                    },
                    indent=2,
                )
                + "\n"
            )
            if not completed:
                output.with_suffix(".server.log").write_text(
                    (root / "server.log").read_text()
                )
    print(f"Ingress admission: {len(assertions)} checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    arguments = parser.parse_args()
    run(arguments.binary.resolve(), arguments.out)
