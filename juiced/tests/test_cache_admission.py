#!/usr/bin/env python3
"""Isolated real-engine cache admission, failure cleanup and identity acceptance."""

import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import signal
import socket
import stat
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

from admission_fixture import configure_admission

MODEL = "Stories15M-Q4_0"
ALIAS = "Stories-Cache-Alias"
ROUTE = "Stories-Cache-Route"
PREFIXES = ("/api/v0/", "/api/v1/", "/v0/", "/v1/")
MESSAGES = [
    {
        "role": "user",
        "content": "Once upon a time, a little girl and her cat lived in a blue house. "
        * 3,
    }
]
BODY = {
    "model": MODEL,
    "messages": MESSAGES,
    "temperature": 0,
    "seed": 42,
    "max_tokens": 24,
    "ignore_eos": True,
    "cache_prompt": True,
}


class Fixture:
    def __init__(self, args, root):
        self.args, self.root = args, root
        self.process = None
        self.api, self.admin = "", ""
        self.admission_transitions = []
        for name in ("config", "cache", "run", "snapshots"):
            (root / name).mkdir()
        with socket.socket() as sock, socket.socket() as websocket:
            sock.bind(("127.0.0.1", 0))
            websocket.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
            self.websocket_port = websocket.getsockname()[1]
        self.url = f"http://127.0.0.1:{self.port}"
        (root / "config/config.json").write_text(
            json.dumps(
                {
                    "broadcast": False,
                    "websocket_port": self.websocket_port,
                    "auto_check_model_updates": False,
                    "disable_model_filtering": True,
                    "llamacpp": {"cpu_bin": str(args.engine)},
                }
            )
        )
        (root / "config/user_models.json").write_text(
            json.dumps(
                {
                    MODEL: {
                        "recipe": "llamacpp",
                        "source": "local_path",
                        "checkpoints": {"main": str(args.model_file)},
                        "labels": ["chat"],
                        "recipe_options": {"llamacpp_backend": "cpu", "ctx_size": 2048},
                    },
                    ROUTE: {
                        "recipe": "collection.router",
                        "version": "1",
                        "components": [MODEL],
                        "routing": {
                            "candidates": [MODEL],
                            "default_model": MODEL,
                            "rules": [],
                        },
                    },
                }
            )
        )
        self.log = (root / "server.log").open("w")

    def start(self, api="", admin=""):
        self.stop()
        self.api, self.admin = api, admin or api
        env = {
            **os.environ,
            "HOME": str(self.root),
            "XDG_CACHE_HOME": str(self.root / "cache"),
            "XDG_RUNTIME_DIR": str(self.root / "run"),
            "HF_HUB_CACHE": str(self.root / "hf"),
            "JUICED_SNAPSHOT_DIR": str(self.root / "snapshots"),
            "LEMONADE_API_KEY": api,
            "LEMONADE_ADMIN_API_KEY": admin or "",
        }
        if admin is None:
            env.pop("LEMONADE_ADMIN_API_KEY", None)
        self.process = subprocess.Popen(
            [
                str(self.args.binary),
                str(self.root / "cache"),
                str(self.root / "config"),
                "--host",
                "127.0.0.1",
                "--port",
                str(self.port),
            ],
            env=env,
            stdout=self.log,
            stderr=self.log,
            start_new_session=True,
        )
        for _ in range(200):
            try:
                status, _, data = self.call("/v1/juiced/catalog")
                if status == 200 and any(m["id"] == MODEL for m in data["data"]):
                    return
            except OSError:
                pass
            if self.process.poll() is not None:
                raise AssertionError("Isolated manager exited; inspect fixture log")
            time.sleep(0.1)
        raise AssertionError("Isolated manager did not initialize")

    def stop(self):
        if self.process is None:
            return
        self.process.terminate()
        try:
            self.process.wait(timeout=20)
        except subprocess.TimeoutExpired:
            os.killpg(self.process.pid, signal.SIGKILL)
            self.process.wait()
        self.process = None

    def call(
        self, path, body=None, headers=None, method=None, stream=False, timeout=120
    ):
        credential = {"Authorization": "Bearer " + self.admin} if self.admin else {}
        request = urllib.request.Request(
            self.url + path,
            data=None if body is None else json.dumps(body).encode(),
            method=method,
            headers={
                "Content-Type": "application/json",
                **credential,
                **(headers or {}),
            },
        )
        try:
            response = urllib.request.urlopen(request, timeout=timeout)
        except urllib.error.HTTPError as response:
            with response:
                return response.code, dict(response.headers), json.load(response)
        with response:
            if stream:
                chunks, done = [], False
                for line in response:
                    if line.startswith(b"data:"):
                        payload = line[5:].strip()
                        if payload == b"[DONE]":
                            done = True
                        else:
                            value = json.loads(payload)
                            assert "error" not in value, value
                            chunks.append(value)
                assert done and chunks, "Incomplete SSE response"
                data = dict(chunks[-1])
                data["stream_content"] = "".join(
                    choice.get("delta", {}).get("content", "")
                    for chunk in chunks
                    for choice in chunk.get("choices", [])
                    if isinstance(choice.get("delta", {}).get("content", ""), str)
                )
            else:
                data = json.load(response)
            return response.status, dict(response.headers), data

    def ok(self, path, body=None, **kwargs):
        status, _, data = self.call(path, body, **kwargs)
        assert 200 <= status < 300, (path, status, data)
        return data

    def configure_admission(self, settings):
        return configure_admission(self.call, settings, self.admission_transitions)

    def load(self):
        self.ok("/v1/juiced/operation", {"model": MODEL, "action": "load"})
        self.wait_loaded(True)

    def wait_loaded(self, loaded):
        for _ in range(200):
            model = next(
                m for m in self.ok("/v1/juiced/catalog")["data"] if m["id"] == MODEL
            )
            assert model.get("operation", {}).get("state") != "failed", model.get(
                "operation"
            )
            if model["loaded"] == loaded:
                return
            time.sleep(0.1)
        raise AssertionError("Worker did not reach requested residency")

    def reload(self):
        self.ok("/v1/juiced/operation", {"model": MODEL, "action": "unload"})
        self.wait_loaded(False)
        self.load()

    def ask(
        self,
        session,
        model=MODEL,
        prefix="/v1/",
        streaming=False,
        headers=None,
        timeout=120,
        **options,
    ):
        return self.call(
            prefix + "chat/completions",
            {
                **BODY,
                "model": model,
                "stream": streaming,
                "stream_options": {"include_usage": True},
                **options,
            },
            headers={"X-Juiced-Session-Id": session, **(headers or {})},
            stream=streaming,
            timeout=timeout,
        )


def resume_session(fixture, item):
    started = time.monotonic()
    deadline = started + 120
    attempts = []
    index, session = item
    while True:
        remaining = deadline - time.monotonic()
        assert remaining > 0, ("Ingress did not become available", session, attempts)
        response = fixture.ask(session, streaming=bool(index % 2), timeout=remaining)
        status, headers, data = response
        code = data.get("error", {}).get("details", {}).get("code")
        attempts.append({"status": status, "code": code})
        remaining = deadline - time.monotonic()
        assert remaining > 0, (
            "Session restore exceeded its deadline",
            session,
            attempts,
        )
        if status != 429 or code != "ingress_capacity_exceeded":
            return response, {
                "session": session,
                "attempts": attempts,
                "elapsed_seconds": time.monotonic() - started,
            }
        delay = float(headers.get("Retry-After", 1))
        time.sleep(min(max(0.01, delay), remaining))


def forget_session(fixture, session):
    started = time.monotonic()
    deadline = started + 10
    attempts = []
    while True:
        remaining = deadline - time.monotonic()
        assert remaining > 0, ("Snapshot finalization did not drain", attempts)
        status, _, data = fixture.call(
            "/v1/juiced/cache/forget", {"session": session}, timeout=remaining
        )
        attempts.append({"status": status, "error": data.get("error")})
        if status == 200:
            assert data["forgotten"] > 0, data
            return {
                "attempts": attempts,
                "elapsed_seconds": time.monotonic() - started,
                "cleanup_pending": data["cleanup_pending"],
            }
        assert status == 409 and data.get("error") in (
            "Snapshot publication requires recovery before mutation",
            "Session cache is in use; retry after generation finishes",
        ), (status, data)
        remaining = deadline - time.monotonic()
        assert remaining > 0, ("Snapshot finalization did not drain", attempts)
        time.sleep(min(0.05, remaining))


def acceptance(args):
    proof = {
        "scope": "isolated real CPU engine; no production services or model quality claim",
        "snapshot_forget_attempts": [],
    }
    with tempfile.TemporaryDirectory(prefix="juiced-cache-admission-") as directory:
        fixture = Fixture(args, Path(directory))
        try:
            fixture.start()
            base_proof = Path(directory) / "base-proof.json"
            subprocess.run(
                [
                    "python3",
                    str(Path(__file__).with_name("test_engine_cache.py")),
                    "--url",
                    fixture.url,
                    "--model-file",
                    str(args.model_file),
                    "--snapshots",
                    str(fixture.root / "snapshots"),
                    "--out",
                    str(base_proof),
                ],
                check=True,
            )
            proof["useful_restart_reuse"] = True
            proof["manual_persistent_slot_mutations_rejected"] = True
            fixture.ok("/internal/aliases", {"alias": ALIAS, "target": MODEL})
            fixture.ok("/v1/juiced/policy", {"model": ROUTE, "policy": "on-demand"})
            matrix = []
            for global_mode, override in (
                ("memory", "persistent"),
                ("persistent", "memory"),
            ):
                fixture.ok(
                    "/v1/juiced/cache",
                    {"mode": global_mode, "model_modes": {MODEL: override}},
                )
                for prefix in PREFIXES:
                    if override == "persistent":
                        for action in ("erase", "save", "restore"):
                            status, _, data = fixture.call(
                                prefix + "slots/0?action=" + action,
                                {"model": MODEL, "filename": "manual-fixture.bin"},
                            )
                            assert (
                                status == 409
                                and data["error"]["details"]["code"]
                                == "prompt_state_busy"
                            ), (status, data)
                        assert fixture.ok(prefix + "juiced/admission")["uncertain"] == 0
                    for name in (MODEL, ALIAS, MODEL + ":latest", ALIAS + ":latest"):
                        for streaming in (False, True):
                            session = f"matrix-{len(matrix)}"
                            status, headers, _ = fixture.ask(
                                session, name, prefix, streaming
                            )
                            assert status == 200, (status, headers)
                            cache = headers.get("X-Juiced-Cache")
                            if override == "persistent":
                                assert cache == "cold", headers
                                status, headers, _ = fixture.ask(
                                    session, MODEL, prefix, streaming
                                )
                                assert (
                                    status == 200
                                    and headers.get("X-Juiced-Cache") == "restored"
                                ), headers
                                proof["snapshot_forget_attempts"].append(
                                    forget_session(fixture, session)
                                )
                            else:
                                assert cache is None, headers
                            matrix.append(
                                {
                                    "prefix": prefix,
                                    "model": name,
                                    "stream": streaming,
                                    "global": global_mode,
                                    "override": override,
                                }
                            )
            proof["model_name_mode_matrix"] = matrix
            fixture.ok(
                "/v1/juiced/cache",
                {"mode": "memory", "model_modes": {MODEL: "persistent"}},
            )
            for prefix in PREFIXES:
                for endpoint, payload in (
                    ("chat/completions", {"messages": MESSAGES}),
                    ("responses", {"input": "fixture"}),
                    ("completions", {"prompt": "fixture"}),
                ):
                    status, _, _ = fixture.call(
                        prefix + endpoint, {"model": ROUTE, **payload}
                    )
                    assert status == 409, (prefix, endpoint, status)
            proof["unqualified_routed_access_rejected"] = 12
            for value in (None, "false", 0, [], {}):
                status, _, _ = fixture.ask("invalid-input", cache_prompt=value)
                assert status == 400, status
                status, _, _ = fixture.ask("after-invalid", cache_prompt=False)
                assert status == 200, status
            proof["malformed_input_releases"] = 5
            for prefix in PREFIXES:
                assert (
                    fixture.call(prefix + "chat/completions", {"messages": MESSAGES})[0]
                    == 409
                )
            proof["implicit_protected_target_rejected"] = 4
            profile = fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})

            def slot_inventory():
                with urllib.request.urlopen(
                    profile["address"] + "/slots", timeout=10
                ) as response:
                    return json.load(response)

            def processing_count():
                return sum(bool(slot.get("is_processing")) for slot in slot_inventory())

            context_before = next(
                m["context"]
                for m in fixture.ok("/v1/juiced/catalog")["data"]
                if m["id"] == MODEL
            )

            def has_output(response):
                if "stream_content" in response:
                    return bool(response["stream_content"].strip())
                return any(
                    choice.get("message", {}).get("content", "").strip()
                    for choice in response.get("choices", [])
                )

            def concurrent_requests(slot_count, persistent):
                with concurrent.futures.ThreadPoolExecutor(max_workers=5) as executor:
                    running = [
                        executor.submit(
                            fixture.ask, f"concurrent-{slot_count}-{i}", max_tokens=1024
                        )
                        for i in range(slot_count)
                    ]
                    until = time.monotonic() + 5
                    while processing_count() < slot_count:
                        assert (
                            not any(task.done() for task in running)
                            and time.monotonic() < until
                        ), "Engine concurrency was not observed"
                        time.sleep(0.005)
                    queued = [
                        executor.submit(
                            fixture.ask,
                            f"queued-{slot_count}-{i}",
                            streaming=bool(i % 2),
                        )
                        for i in range(5 - slot_count)
                    ]
                    while True:
                        observation = fixture.ok("/v1/juiced/admission")
                        if observation["pending"] >= 5 - slot_count:
                            break
                        assert (
                            not any(task.done() for task in running)
                            and time.monotonic() < until
                        ), observation
                        time.sleep(0.005)
                    assert (
                        observation["optional_limits"]["principal_pending_limit"]
                        is None
                    ), observation
                    assert any(
                        len(group["claimed"]) == slot_count
                        and group["pending"] >= 5 - slot_count
                        for group in observation["physical_slots"]
                    ), observation
                    if persistent:
                        assert (
                            fixture.call(
                                "/v1/juiced/cache",
                                {"mode": "memory", "model_modes": {}},
                            )[0]
                            == 409
                        )
                    responses = [
                        future.result(timeout=120) for future in running + queued
                    ]
                    assert all(
                        response[0] == 200 and has_output(response[2])
                        for response in responses
                    ), responses
                    return observation

            observation = concurrent_requests(1, True)
            idle_sessions = [f"over-slot-{i}" for i in range(8)]
            for session in idle_sessions:
                status, headers, _ = fixture.ask(session)
                assert status == 200 and headers.get("X-Juiced-Cache") == "cold", (
                    status,
                    headers,
                )
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
                resumed = list(
                    executor.map(
                        lambda item: resume_session(fixture, item),
                        enumerate(idle_sessions),
                    )
                )
            responses = [response for response, _ in resumed]
            assert all(
                status == 200
                and headers.get("X-Juiced-Cache") == "restored"
                and has_output(data)
                for status, headers, data in responses
            ), responses
            assert not any(
                group["claimed"] or group["pending"]
                for group in fixture.ok("/v1/juiced/admission")["physical_slots"]
            )
            context_after = next(
                m["context"]
                for m in fixture.ok("/v1/juiced/catalog")["data"]
                if m["id"] == MODEL
            )
            for field in (
                "configured_tokens",
                "applied_tokens",
                "observed_per_request_tokens",
                "observed_slots",
            ):
                assert context_after[field] == context_before[field], (
                    field,
                    context_before,
                    context_after,
                )
            proof["physical_slot_fairness"] = {
                "simultaneous_requests": 5,
                "formats": ["JSON", "SSE"],
                "all_responses_nonempty": True,
                "observed_pending": observation["pending"],
                "idle_and_resumed_sessions": 8,
                "resumed_from_disk": 8,
                "ingress_attempts": [attempts for _, attempts in resumed],
                "context_unchanged": True,
                "principal_limit_unset": True,
                "configured_context": context_after["configured_tokens"],
                "observed_context": context_after["observed_per_request_tokens"],
                "observed_slots": context_after["observed_slots"],
            }
            saved = fixture.ok(f"/v1/models/{MODEL}/options")["saved"]
            parallel_args = saved["llamacpp_args"].replace(
                "--parallel 1", "--parallel 2"
            )
            assert parallel_args != saved["llamacpp_args"], saved
            fixture.ok("/v1/juiced/cache", {"mode": "memory", "model_modes": {}})
            fixture.ok(
                f"/v1/models/{MODEL}/options",
                {"ctx_size": 256, "llamacpp_args": parallel_args},
            )
            fixture.reload()
            profile = fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})
            two_before = next(
                m["context"]
                for m in fixture.ok("/v1/juiced/catalog")["data"]
                if m["id"] == MODEL
            )
            assert (
                two_before["configured_tokens"] == 256
                and two_before["observed_per_request_tokens"] == 128
                and two_before["observed_slots"] == 2
            ), two_before
            two_observation = concurrent_requests(2, False)
            status, _, choices = fixture.ask("memory-two-choices", n=2)
            assert status == 200 and len(choices["choices"]) == 2, (status, choices)
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
                fixed = executor.submit(
                    fixture.ask, "fixed-slot-one", id_slot=1, max_tokens=1024
                )
                until = time.monotonic() + 5
                while True:
                    processing_ids = [
                        slot["id"]
                        for slot in slot_inventory()
                        if slot.get("is_processing")
                    ]
                    if processing_ids:
                        break
                    assert not fixed.done() and time.monotonic() < until
                    time.sleep(0.005)
                assert processing_ids == [1], processing_ids
                mutation = executor.submit(
                    fixture.call, "/v1/slots/1?action=erase", {"model": MODEL}
                )
                while fixture.ok("/v1/juiced/admission")["pending"] < 1:
                    assert not fixed.done() and time.monotonic() < until
                    time.sleep(0.005)
                assert (
                    not mutation.done()
                ), "Manual mutation bypassed the owned physical slot"
                assert fixed.result(timeout=120)[0] == 200
                status, _, result = mutation.result(timeout=120)
                assert status == 200, (status, result)
            two_after = next(
                m["context"]
                for m in fixture.ok("/v1/juiced/catalog")["data"]
                if m["id"] == MODEL
            )
            assert two_after == two_before, (two_before, two_after)
            proof["memory_slot_fairness"] = {
                "simultaneous_requests": 5,
                "observed_running_slots": 2,
                "observed_pending": two_observation["pending"],
                "formats": ["JSON", "SSE"],
                "all_responses_nonempty": True,
                "configured_total_context": 256,
                "observed_per_request_context": 128,
                "context_unchanged": True,
                "explicit_test_profile": True,
                "explicit_slot_binding": True,
                "manual_mutation_waits": True,
                "multiple_choices": 2,
            }
            fixture.ok(f"/v1/models/{MODEL}/options", saved)
            fixture.reload()
            fixture.ok(
                "/v1/juiced/cache",
                {"mode": "memory", "model_modes": {MODEL: "persistent"}},
            )
            profile = fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})
            cancel = urllib.request.Request(
                fixture.url + "/v1/chat/completions",
                data=json.dumps({**BODY, "stream": True, "max_tokens": 8192}).encode(),
                headers={
                    "Content-Type": "application/json",
                    "X-Juiced-Session-Id": "disconnect-lease",
                },
            )
            with urllib.request.urlopen(cancel, timeout=120) as response:
                assert response.read(128), "Expected actual output before disconnect"
            for _ in range(300):
                if not processing_count():
                    break
                time.sleep(0.05)
            else:
                raise AssertionError("Engine did not settle after disconnect")
            assert fixture.ask("disconnect-lease")[0] == 200
            proof["observed_engine_idle_after_stream_disconnect"] = True
            status, _, _ = fixture.ask("filesystem-fault-source")
            assert status == 200
            snapshot = next(
                x
                for x in fixture.ok("/v1/juiced/cache")["snapshots"]
                if x["session"] == "filesystem-fault-source"
            )
            target = fixture.root / "snapshots" / (snapshot["id"] + ".bin")
            saved = target.with_suffix(".fault-backup")
            target.rename(saved)
            target.symlink_to(target.name)
            try:
                status, _, error = fixture.ask("filesystem-fault")
                assert status == 503, (status, error)
            finally:
                target.unlink()
                saved.rename(target)
            assert fixture.ask("filesystem-fault")[0] == 200
            fixture.ok("/v1/juiced/cache", {"mode": "memory", "model_modes": {}})
            proof["filesystem_failure_releases_slot_and_session"] = True

            fixture.start("fixture-inference-v1", "fixture-operator-v1")
            fixture.load()
            fixture.ok("/v1/juiced/cache", {"mode": "persistent", "model_modes": {}})
            inference = {"Authorization": "Bearer fixture-inference-v1"}
            operator = {"Authorization": "Bearer fixture-operator-v1"}

            def isolated_turn(label, credential, expected):
                status, headers, response = fixture.ask(
                    "same-client-session", headers=credential
                )
                assert status == 200 and headers.get("X-Juiced-Cache") == expected, (
                    label,
                    status,
                    headers,
                )
                if expected == "cold":
                    assert response["timings"]["cache_n"] == 0, (
                        label,
                        response["timings"],
                    )
                elif expected == "restored":
                    assert response["timings"]["cache_n"] > 30, (
                        label,
                        response["timings"],
                    )
                snapshots = fixture.ok("/v1/juiced/cache")["snapshots"]
                return {
                    x["id"] for x in snapshots if x["session"] == "same-client-session"
                }

            ids_api = isolated_turn("inference first", inference, "cold")
            ids_admin = isolated_turn("operator first, same worker", operator, "cold")
            isolated_turn("inference returns, same worker", inference, "restored")
            assert len(ids_admin - ids_api) == 1
            fixture.start("fixture-inference-v1", "fixture-operator-v1")
            fixture.load()
            isolated_turn("inference restarted", inference, "restored")
            fixture.reload()
            isolated_turn("operator restarted", operator, "restored")
            fixture.reload()
            isolated_turn(
                "x-api-key equivalent",
                {
                    "Authorization": "Bearer fixture-inference-v1",
                    "x-api-key": "fixture-inference-v1",
                },
                "restored",
            )
            # Remove the default operator header to test x-api-key by itself.
            admin = fixture.admin
            fixture.admin = ""
            try:
                status, headers, _ = fixture.ask(
                    "same-client-session", headers={"x-api-key": fixture.api}
                )
                assert (
                    status == 200 and headers.get("X-Juiced-Cache") == "restored"
                ), headers
            finally:
                fixture.admin = admin
            for staging in (True, False):
                fixture.stop()
                state_path = fixture.root / "config/juiced/state.json"
                before = state_path.read_bytes()
                temporary = state_path.with_suffix(".json.tmp")
                permissions = stat.S_IMODE(state_path.parent.stat().st_mode)
                if staging:
                    temporary.mkdir()
                else:
                    temporary.write_text("{}")
                    os.chmod(state_path.parent, stat.S_IRUSR | stat.S_IXUSR)
                rejected = False
                try:
                    fixture.start("fixture-inference-rejected", "fixture-operator-v1")
                except AssertionError:
                    rejected = (
                        fixture.process is not None
                        and fixture.process.poll() is not None
                    )
                finally:
                    fixture.stop()
                    os.chmod(state_path.parent, permissions)
                    if temporary.is_dir():
                        temporary.rmdir()
                    elif temporary.exists():
                        temporary.unlink()
                assert (
                    rejected and state_path.read_bytes() == before
                ), "Failed authentication activation changed committed state"
                fixture.start("fixture-inference-v1", "fixture-operator-v1")
                fixture.load()
                isolated_turn(
                    "failed auth activation retains namespace", inference, "restored"
                )
            fixture.start("fixture-inference-v2", "fixture-operator-v1")
            fixture.load()
            isolated_turn(
                "credential rotated",
                {"Authorization": "Bearer fixture-inference-v2"},
                "cold",
            )
            assert fixture.ask("same-client-session", headers=inference)[0] == 401
            fixture.reload()
            isolated_turn("operator epoch rotated", operator, "cold")
            fixture.start()
            fixture.load()
            ids_anonymous = isolated_turn("authentication disabled", {}, "cold")
            fixture.start()
            fixture.load()
            isolated_turn(
                "anonymous restart",
                {"Authorization": "Bearer anonymous-dummy"},
                "restored",
            )
            fixture.start("fixture-inference-v2", "fixture-operator-v1")
            fixture.load()
            isolated_turn(
                "authentication restored",
                {"Authorization": "Bearer fixture-inference-v2"},
                "cold",
            )
            fixture.start()
            fixture.load()
            isolated_turn("anonymous reinstated", {}, "cold")
            fixture.start("fixture-inference-v1", "fixture-operator-v1")
            fixture.load()
            isolated_turn("original credential reinstated", inference, "cold")
            fixture.start("fixture-inference-v2", "fixture-operator-v1")
            fixture.load()
            isolated_turn(
                "replacement credential reinstated",
                {"Authorization": "Bearer fixture-inference-v2"},
                "cold",
            )
            fixture.start("fixture-inference-v1", "fixture-operator-v1")
            fixture.load()
            isolated_turn("original credential returns again", inference, "cold")
            fallback = {"Authorization": "Bearer fixture-fallback"}
            fixture.start("fixture-fallback", None)
            fixture.load()
            isolated_turn("unset admin fallback", fallback, "cold")
            fixture.start("fixture-fallback", "")
            fixture.load()
            isolated_turn("empty admin keeps effective namespace", fallback, "restored")
            isolated_turn(
                "fallback x-api-key equivalent",
                {
                    "Authorization": "Bearer fixture-fallback",
                    "x-api-key": "fixture-fallback",
                },
                "restored",
            )
            proof["identity_isolation"] = {
                "principal_namespaces_distinct": True,
                "same_worker_principal_switch_cold": True,
                "restart_restore": True,
                "bearer_api_key_equivalent": True,
                "credential_rotation_cold": True,
                "auth_mode_change_cold": True,
                "failed_auth_activation_preserves_namespace": True,
                "returning_auth_mode_cold": True,
                "returning_credentials_cold": True,
                "unset_empty_admin_effective_namespace_preserved": True,
                "old_credential_rejected": True,
                "retained_same_session_namespaces": len(ids_anonymous),
            }
        finally:
            fixture.stop()
            fixture.log.close()
            args.out.with_suffix(".server.log").write_bytes(
                (fixture.root / "server.log").read_bytes()
            )
    args.out.write_text(json.dumps(proof, indent=2) + "\n")
    print(
        "PASS: real-engine name/mode admission, failure cleanup and authenticated restart isolation"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--model-file", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    for name in ("binary", "engine", "model_file"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    acceptance(args)


if __name__ == "__main__":
    main()
