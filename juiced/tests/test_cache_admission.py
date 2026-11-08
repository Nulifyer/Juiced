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
        for name in ("config", "cache", "run", "snapshots"):
            (root / name).mkdir()
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
        self.url = f"http://127.0.0.1:{self.port}"
        (root / "config/config.json").write_text(
            json.dumps(
                {
                    "broadcast": False,
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

    def call(self, path, body=None, headers=None, method=None, stream=False):
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
            response = urllib.request.urlopen(request, timeout=120)
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
                data = chunks[-1]
            else:
                data = json.load(response)
            return response.status, dict(response.headers), data

    def ok(self, path, body=None, **kwargs):
        status, _, data = self.call(path, body, **kwargs)
        assert 200 <= status < 300, (path, status, data)
        return data

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
        )


def acceptance(args):
    proof = {
        "scope": "isolated real CPU engine; no production services or model quality claim"
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
                                fixture.ok(
                                    "/v1/juiced/cache/forget", {"session": session}
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

            def processing():
                with urllib.request.urlopen(
                    profile["address"] + "/slots", timeout=10
                ) as response:
                    return any(
                        slot.get("is_processing") for slot in json.load(response)
                    )

            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
                running = executor.submit(
                    fixture.ask, "concurrent-lease", max_tokens=4096
                )
                for _ in range(300):
                    if processing():
                        break
                    assert (
                        not running.done()
                    ), "Fixture finished before concurrency could be observed"
                    time.sleep(0.01)
                else:
                    raise AssertionError("Fixture never entered the engine")
                assert fixture.ask("concurrent-lease")[0] in (409, 429)
                assert (
                    fixture.call(
                        "/v1/juiced/cache", {"mode": "memory", "model_modes": {}}
                    )[0]
                    == 409
                )
                assert running.result(timeout=120)[0] == 200
            assert fixture.ask("after-concurrency")[0] == 200
            proof["observed_concurrent_slot_exclusion"] = True
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
                if not processing():
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
