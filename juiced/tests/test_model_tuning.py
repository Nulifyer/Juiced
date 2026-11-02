#!/usr/bin/env python3
"""Real CPU fixture: deferred settings, Hot reload and busy-request protection."""
import argparse
import json
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument("--url", required=True)
p.add_argument("--out", required=True)
a = p.parse_args()
base = a.url.rstrip("/") + "/v1/"
model = "Stories15M-Q4_0"


def request(path, body=None):
    q = urllib.request.Request(base + path, data=None if body is None else json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(q, timeout=90) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        with e:
            return e.code, json.load(e)


def call(path, body=None):
    status, result = request(path, body)
    assert status < 300, (status, result)
    return result


def state():
    return next(m for m in call("juiced/catalog")["data"] if m["id"] == model)


def wait(predicate):
    for _ in range(300):
        m = state()
        if m.get("operation", {}).get("state") == "failed":
            raise AssertionError(m["operation"])
        if predicate(m):
            return m
        time.sleep(.1)
    raise AssertionError("Model did not reach the expected state")


initial = state()
assert initial["checkpoints"]["main"].endswith("stories15M-q4_0.gguf"), "Use the isolated Stories15M fixture"
options = call("models/" + model + "/options")
assert options["effective"]["llamacpp_backend"] == "cpu"
original_context = options["saved"].get("ctx_size")
original_policy = initial["policy"]
proof = {}
try:
    call("juiced/policy", {"model": model, "policy": "hot"})
    loaded = wait(lambda m: m["loaded"] and not m.get("operation", {}).get("pending"))
    pid = loaded["runtime"]["pid"]
    target = 256 if options["effective"]["ctx_size"] != 256 else 512
    call("models/" + model + "/options", {"ctx_size": target})
    deferred = wait(lambda m: m.get("runtime", {}).get("settings_pending"))
    assert deferred["runtime"]["pid"] == pid
    assert deferred["policy"] == "hot"
    proof["save_for_next_load"] = {"pid_unchanged": True, "pending_visible": True}
    call("juiced/operation", {"model": model, "action": "reload"})
    loaded = wait(lambda m: m["loaded"] and m["runtime"]["pid"] != pid and not m.get("operation", {}).get("pending"))
    assert loaded["policy"] == "hot"
    assert not loaded["runtime"]["settings_pending"], loaded
    assert loaded["recipe_options"]["ctx_size"] == target
    proof["hot_reload"] = {"old_pid": pid, "new_pid": loaded["runtime"]["pid"], "policy": loaded["policy"], "context": target, "pending_cleared": True}
    saved = call("models/" + model + "/options")["saved"]
    code, _ = request("models/" + model + "/options", {"ctx_size": 0})
    assert code == 400
    assert call("models/" + model + "/options")["saved"] == saved
    proof["invalid_settings_do_not_write"] = True

    started = threading.Event()
    stream_errors = []
    def generate():
        try:
            body = {"model": model, "messages": [{"role": "user", "content": "Tell a long story about a village."}], "stream": True, "max_tokens": 4096, "ignore_eos": True}
            q = urllib.request.Request(base + "chat/completions", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(q, timeout=90) as r:
                for line in r:
                    if line.startswith(b"data:"):
                        started.set()
        except Exception as e:
            stream_errors.append(str(e))
    thread = threading.Thread(target=generate)
    thread.start()
    assert started.wait(30), stream_errors
    code, error = request("juiced/operation", {"model": model, "action": "reload"})
    assert code == 409, (code, error)
    thread.join(timeout=90)
    assert not thread.is_alive() and not stream_errors, stream_errors
    after = state()
    assert after["runtime"]["pid"] == loaded["runtime"]["pid"]
    proof["busy_reload"] = {"status": code, "stream_finished": True, "pid_unchanged": True}
finally:
    call("models/" + model + "/options", {"ctx_size": original_context})
    before = state()["runtime"]["pid"]
    call("juiced/operation", {"model": model, "action": "reload"})
    wait(lambda m: m["loaded"] and m["runtime"]["pid"] != before and not m.get("operation", {}).get("pending"))
    call("juiced/policy", {"model": model, "policy": original_policy})
Path(a.out).write_text(json.dumps(proof, indent=2) + "\n")
print("PASS: save without restart, Hot reload, invalid settings, active-request protection and fixture restoration")
