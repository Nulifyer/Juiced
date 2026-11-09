#!/usr/bin/env python3
"""Persistent-cache exchange cancellation and failed-write ownership."""

import argparse
import json
from pathlib import Path
import socket
import sys
import tempfile
import time

from test_cache_admission import BODY, Fixture, MODEL


def acceptance(args):
    proof = {
        "scope": "mock engine inspection HTTP only; no model, hardware or quality qualification"
    }
    with tempfile.TemporaryDirectory(prefix="juiced-cache-inspection-") as directory:
        root = Path(directory)
        fixture = Fixture(args, root)
        calls = root / "engine-calls.jsonl"
        delayed = root / "delayed-path"
        delayed.write_text("")
        engine = root / "inspection-engine"
        engine.write_text(
            f"#!{sys.executable}\n"
            "import json,sys,time,signal\n"
            "from pathlib import Path\n"
            "from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer\n"
            "signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))\n"
            "signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM})\n"
            "class Handler(BaseHTTPRequestHandler):\n"
            " def log_message(self,*_): pass\n"
            " def record(self):\n"
            f"  with open({str(calls)!r},'a') as log: log.write(json.dumps(self.path)+'\\n')\n"
            " def do_GET(self):\n"
            "  self.record()\n"
            f"  if self.path == Path({str(delayed)!r}).read_text(): time.sleep(10)\n"
            "  body=json.dumps({'status':'ok','build_info':'dde879b-fixture-inspection'}).encode()\n"
            "  try:\n"
            "   self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)\n"
            "  except (BrokenPipeError,ConnectionResetError): pass\n"
            " def do_POST(self):\n"
            "  self.record();self.send_response(500);self.send_header('Content-Length','2');self.end_headers();self.wfile.write(b'{}')\n"
            "port=int(sys.argv[sys.argv.index('--port')+1])\n"
            "ThreadingHTTPServer(('127.0.0.1',port),Handler).serve_forever()\n"
        )
        engine.chmod(0o700)
        config_path = root / "config/config.json"
        config = json.loads(config_path.read_text())
        config["llamacpp"] = {"cpu_bin": str(engine)}
        config_path.write_text(json.dumps(config))
        try:
            fixture.start("fixture-inference", "fixture-admin")
            fixture.ok("/v1/juiced/policy", {"model": MODEL, "policy": "on-demand"})
            fixture.ok(
                "/v1/models/" + MODEL + "/options",
                {
                    "ctx_size": 2048,
                    "llamacpp_backend": "cpu",
                    "llamacpp_args": "--slot-save-path " + str(root / "snapshots"),
                },
            )
            fixture.load()
            loaded = next(
                row
                for row in fixture.ok("/v1/health")["all_models_loaded"]
                if row["model_name"] == MODEL
            )
            mock_profile = fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})
            assert mock_profile["configured"], mock_profile
            options = loaded["recipe_options"]
            options.pop("pinned", None)
            fixture.ok("/v1/unload", {})
            fixture.stop()
            state_path = root / "config/juiced/state.json"
            state = json.loads(state_path.read_text())
            state["cache"]["mode"] = "persistent"
            state["qualified_profiles"] = {
                mock_profile["fingerprint"]: {
                    "evidence": "Controlled mock profile only; not useful-reuse qualification"
                }
            }
            state_path.write_text(json.dumps(state))
            fixture.start("fixture-inference", "fixture-admin")
            matrix = []
            for path, mode in (
                (path, mode)
                for path in ("/props", "/slots")
                for mode in ("deadline", "disconnect")
            ):
                delayed.write_text("")
                completion_ms = 2000 if mode == "deadline" else 7000
                profile = {
                    "checkpoint": loaded["checkpoint"],
                    "options": options,
                    "completion_ms": completion_ms,
                    "source": "delayed mock inspection",
                    "load_bytes": {"host": 16 * 1024**2},
                    "resident_bytes": {"host": 8 * 1024**2},
                    "state_bytes": {"host": 1024},
                }
                fixture.ok(
                    "/internal/set",
                    {
                        "admission": {
                            "mode": "legacy",
                            "pools": {
                                "host": {
                                    "capacity_bytes": 1024**3,
                                    "source": "synthetic host ceiling",
                                }
                            },
                            "models": {MODEL: profile},
                        }
                    },
                )
                fixture.load()
                for _ in range(200):
                    model = next(
                        row
                        for row in fixture.ok("/v1/juiced/catalog")["data"]
                        if row["id"] == MODEL
                    )
                    if not model.get("operation", {}).get("pending", False):
                        break
                    time.sleep(0.01)
                assert not model.get("operation", {}).get("pending", False), model
                calls.write_text("")
                delayed.write_text(path)
                started = time.monotonic()
                if mode == "deadline":
                    status, _, data = fixture.ask("delayed-profile")
                    assert status >= 400, (status, data)
                else:
                    connection = socket.create_connection(
                        ("127.0.0.1", fixture.port), timeout=5
                    )
                    payload = json.dumps(BODY).encode()
                    headers = (
                        "POST /v1/chat/completions HTTP/1.1\r\n"
                        f"Host: 127.0.0.1:{fixture.port}\r\n"
                        "Content-Type: application/json\r\n"
                        f"Authorization: Bearer {fixture.api}\r\n"
                        f"Content-Length: {len(payload)}\r\n\r\n"
                    )
                    connection.sendall(headers.encode() + payload)
                    for _ in range(200):
                        if json.dumps(path) in calls.read_text():
                            break
                        time.sleep(0.01)
                    if json.dumps(path) not in calls.read_text():
                        connection.settimeout(1)
                        response = connection.recv(65536)
                        raise AssertionError(
                            (
                                "profile inspection never started",
                                response.decode(errors="replace"),
                                fixture.ok("/v1/juiced/admission"),
                            )
                        )
                    connection.close()
                    for _ in range(300):
                        state = fixture.ok("/v1/juiced/admission")
                        if state["uncertain"] == 1:
                            break
                        time.sleep(0.01)
                elapsed_ms = (time.monotonic() - started) * 1000
                assert elapsed_ms < 4000, (mode, elapsed_ms)
                paths = [json.loads(line) for line in calls.read_text().splitlines()]
                assert paths.count("/props") == 1, paths
                assert paths.count("/slots") == (1 if path == "/slots" else 0), paths
                assert all(
                    item in ("/health", "/props", "/slots") for item in paths
                ), paths
                state = fixture.ok("/v1/juiced/admission")
                assert state["uncertain"] == 1 and state["active"] == 1, state
                fixture.ok("/v1/unload", {})
                state = fixture.ok("/v1/juiced/admission")
                assert state["uncertain"] == 0 and state["active"] == 0, state
                matrix.append(
                    {
                        "path": path,
                        "mode": mode,
                        "completion_ms": completion_ms,
                        "elapsed_ms": elapsed_ms,
                        "no_later_dispatch": True,
                        "state_claim_retained_until_stop": True,
                    }
                )
            proof["inspection_cancellation"] = matrix
            delayed.write_text("/health")
            automatic = []
            for paused in (True, False):
                fixture.ok("/v1/juiced/admission", {"paused": paused})
                for streaming in (False, True):
                    status, headers, data = fixture.ask(
                        "persistent-autoload", stream=streaming
                    )
                    expected = (
                        "admission_paused" if paused else "completion_deadline_exceeded"
                    )
                    assert (
                        status == (503 if paused else 504)
                        and data["error"]["code"] == expected
                    ), (status, data)
                    if paused:
                        assert headers.get("Retry-After") == "1", headers
                    automatic.append(
                        {"status": status, "code": expected, "stream": streaming}
                    )
            proof["persistent_automatic_load_errors"] = automatic
        except Exception:
            fixture.log.flush()
            print((root / "server.log").read_text()[-10000:], file=sys.stderr)
            raise
        finally:
            fixture.stop()
            fixture.log.close()
    return proof


def wait_for(predicate, description):
    for _ in range(200):
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(description)


def save_failure_acceptance(args):
    proof = {
        "scope": "controlled subprocess HTTP and disk ownership; no useful-cache, hardware or model qualification"
    }
    with tempfile.TemporaryDirectory(prefix="juiced-save-failures-") as directory:
        root = Path(directory)
        fixture = Fixture(args, root)
        mode_file, calls = root / "mode", root / "save-calls"
        mode_file.write_text("non200")
        engine = root / "save-engine"
        engine.write_text(
            f"#!{sys.executable}\n"
            "import json,os,sys,time,signal\n"
            "from pathlib import Path\n"
            "from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer\n"
            "signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))\n"
            "signal.pthread_sigmask(signal.SIG_UNBLOCK, {signal.SIGTERM})\n"
            f"mode_file=Path({str(mode_file)!r});calls=Path({str(calls)!r});snapshots=Path({str(root / 'snapshots')!r})\n"
            "class Handler(BaseHTTPRequestHandler):\n"
            " def log_message(self,*_): pass\n"
            " def reply(self,body,status=200):\n"
            "  if not isinstance(body,bytes): body=json.dumps(body).encode()\n"
            "  try:\n"
            "   self.send_response(status);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)\n"
            "  except (BrokenPipeError,ConnectionResetError): pass\n"
            " def do_GET(self):\n"
            "  if self.path == '/slots': self.reply([{'id':0,'is_processing':False}])\n"
            "  else: self.reply({'status':'ok','build_info':'dde879b-mock-save-failures'})\n"
            " def do_POST(self):\n"
            "  body=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))))\n"
            "  if 'action=save' in self.path:\n"
            "   mode=mode_file.read_text();file=snapshots/body['filename'];file.write_bytes(b'x' if mode=='late-disconnect' else b'oversize')\n"
            "   with calls.open('a') as log: log.write(mode+'\\n')\n"
            "   if mode=='late-disconnect': time.sleep(3);file.write_bytes(b'oversize');time.sleep(6)\n"
            "   if mode=='timeout': time.sleep(9)\n"
            "   if mode=='upstream-disconnect': self.connection.close();return\n"
            "   self.reply(b'not json' if mode=='malformed' else {'n_saved':1},500 if mode=='non200' else 200)\n"
            "  elif 'action=restore' in self.path: self.reply({'n_restored':1})\n"
            "  else: self.reply({'id':'mock','choices':[{'index':0,'message':{'role':'assistant','content':'ok'},'finish_reason':'stop'}],'usage':{'prompt_tokens':1,'completion_tokens':1,'total_tokens':2}})\n"
            "port=int(sys.argv[sys.argv.index('--port')+1])\n"
            "ThreadingHTTPServer(('127.0.0.1',port),Handler).serve_forever()\n"
        )
        engine.chmod(0o700)
        config_path = root / "config/config.json"
        config = json.loads(config_path.read_text())
        config["llamacpp"] = {"cpu_bin": str(engine)}
        config_path.write_text(json.dumps(config))
        try:
            fixture.start("fixture-inference", "fixture-admin")
            fixture.ok("/v1/juiced/policy", {"model": MODEL, "policy": "on-demand"})
            fixture.ok(
                "/v1/models/" + MODEL + "/options",
                {
                    "ctx_size": 2048,
                    "llamacpp_backend": "cpu",
                    "llamacpp_args": "--slot-save-path " + str(root / "snapshots"),
                },
            )
            fixture.ok("/v1/load", {"model_name": MODEL, "llamacpp_backend": "cpu"})
            loaded = next(
                row
                for row in fixture.ok("/v1/health")["all_models_loaded"]
                if row["model_name"] == MODEL
            )
            profile = fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})
            options = dict(loaded["recipe_options"])
            options.pop("pinned", None)
            fixture.ok("/v1/unload", {})
            fixture.ok(
                "/internal/set",
                {
                    "admission": {
                        "mode": "legacy",
                        "pools": {
                            "host": {
                                "capacity_bytes": 1024**3,
                                "source": "controlled test pool",
                            }
                        },
                        "models": {
                            MODEL: {
                                "checkpoint": loaded["checkpoint"],
                                "options": options,
                                "completion_ms": 2000,
                                "source": "mock save failure",
                                "load_bytes": {"host": 16 * 1024**2},
                                "resident_bytes": {"host": 8 * 1024**2},
                                "state_bytes": {"host": 1024},
                            }
                        },
                    }
                },
            )
            fixture.ok("/v1/load", {"model_name": MODEL, "llamacpp_backend": "cpu"})
            matrix = []
            for mode in (
                "non200",
                "malformed",
                "upstream-disconnect",
                "timeout",
                "late-disconnect",
            ):
                mode_file.write_text(mode)
                fixture.ok(
                    "/v1/juiced/cache/qualify",
                    {
                        "model": MODEL,
                        "fingerprint": profile["fingerprint"],
                        "max_snapshot_bytes": 1,
                        "evidence": "Deliberately false bound for controlled failure testing only",
                    },
                )
                fixture.ok("/v1/juiced/cache", {"mode": "persistent"})
                calls.write_text("")
                if mode == "late-disconnect":
                    connection = socket.create_connection(
                        ("127.0.0.1", fixture.port), timeout=5
                    )
                    payload = json.dumps(BODY).encode()
                    connection.sendall(
                        (
                            "POST /v1/chat/completions HTTP/1.1\r\n"
                            + f"Host: 127.0.0.1:{fixture.port}\r\nAuthorization: Bearer {fixture.api}\r\nX-Juiced-Session-Id: {mode}\r\nContent-Type: application/json\r\nContent-Length: {len(payload)}\r\n\r\n"
                        ).encode()
                        + payload
                    )
                    wait_for(
                        lambda: calls.exists() and mode in calls.read_text(),
                        "save did not start",
                    )
                    connection.close()
                else:
                    status, _, data = fixture.ask(mode)
                    assert status == 200, (mode, status, data)
                cache = wait_for(
                    lambda: (
                        state
                        if (state := fixture.ok("/v1/juiced/cache"))[
                            "qualified_profiles"
                        ][profile["fingerprint"]].get("write_bound_invalid")
                        else None
                    ),
                    "observed oversize did not invalidate qualification",
                )
                assert cache["pending_writes"] and cache["disk"][
                    "reserved_write_bytes"
                ] >= len(b"oversize"), (mode, cache)
                fixture.ask(mode + "-disabled")
                assert calls.read_text().splitlines() == [mode], (
                    mode,
                    calls.read_text(),
                )
                fixture.ok("/v1/unload", {})
                cache = wait_for(
                    lambda: (
                        state
                        if not (state := fixture.ok("/v1/juiced/cache"))[
                            "pending_writes"
                        ]
                        else None
                    ),
                    "stopped generation write was not reclaimed",
                )
                assert cache["qualified_profiles"][profile["fingerprint"]][
                    "write_bound_invalid"
                ], cache
                fixture.ok("/v1/load", {"model_name": MODEL, "llamacpp_backend": "cpu"})
                fixture.ask(mode + "-after-stop")
                assert calls.read_text().splitlines() == [mode], (
                    mode,
                    calls.read_text(),
                )
                matrix.append(
                    {
                        "mode": mode,
                        "invalidated": True,
                        "charged_until_stop": True,
                        "disabled_after_stop": True,
                    }
                )
            proof["failures"] = matrix
        finally:
            fixture.stop()
            fixture.log.flush()
            args.out.with_suffix(".save-failures.server.log").write_text(
                (root / "server.log").read_text()
            )
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
    result["save_failures"] = save_failure_acceptance(arguments)
    arguments.out.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
