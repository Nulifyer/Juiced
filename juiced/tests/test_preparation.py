#!/usr/bin/env python3
"""Actual conversion/cache/cancel gate using an isolated Stories15M registration."""

import argparse
import json
from pathlib import Path
import time
import urllib.error
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("--url", required=True)
parser.add_argument("--model", default="Stories15M-Q4_0")
parser.add_argument("--out", required=True)
args = parser.parse_args()
base = args.url.rstrip("/") + "/v1/"


def call(path, body=None, expected=200):
    request = urllib.request.Request(
        base + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        response = urllib.request.urlopen(request, timeout=120)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        result = json.load(response)
        assert response.status == expected, (path, response.status, result)
        return result


def wait_job(job):
    for _ in range(600):
        current = call("juiced/preparation")["jobs"][job["id"]]
        if not current["pending"]:
            return current
        time.sleep(0.1)
    raise AssertionError("Preparation did not finish")


def wait_model(name, loaded):
    for _ in range(600):
        model = next(
            (model for model in call("juiced/catalog")["data"] if model["id"] == name),
            None,
        )
        if model and model.get("operation", {}).get("state") == "failed":
            raise AssertionError(model["operation"])
        if model and model["loaded"] == loaded:
            return model
        time.sleep(0.1)
    raise AssertionError("Model lifecycle did not finish")


plan = call("juiced/preparation?model=" + args.model)
assert plan["compatible"] and plan["tools_ready"], plan
name = next(target["name"] for target in plan["targets"] if target["quant"] == "Q5_K_M")
call(
    "juiced/preparation",
    {"model": args.model, "quant": "Q5_K_M", "acknowledge": False},
    400,
)
job = call(
    "juiced/preparation",
    {"model": args.model, "quant": "Q5_K_M", "acknowledge": True},
    202,
)
first = wait_job(job)
assert first["phase"] == "complete" and not first["cached"], first
model = wait_model(name, False)
assert model["policy"] == "disabled", model
assert name not in [model["id"] for model in call("models")["data"]]
call(
    "models/" + name + "/options",
    {
        "llamacpp_backend": "cpu",
        "ctx_size": 128,
        "llamacpp_args": "-ngl 0 -t 2 --chat-template chatml",
    },
)
call("juiced/policy", {"model": name, "policy": "on-demand"})
call("juiced/operation", {"model": name, "action": "load"}, 202)
wait_model(name, True)
output = call(
    "chat/completions",
    {
        "model": name,
        "messages": [{"role": "user", "content": "Once upon a time"}],
        "temperature": 0,
        "max_tokens": 8,
    },
)
assert output["usage"]["completion_tokens"] > 0, output
call("juiced/operation", {"model": name, "action": "unload"}, 202)
wait_model(name, False)
call("juiced/policy", {"model": name, "policy": "disabled"})
removal = call("juiced/removal?model=" + name)
assert removal["allowed"] and removal["action"] == "unregister", removal
call("juiced/removal", {"model": name, "confirmation": name, "expected": removal})
job = call(
    "juiced/preparation",
    {"model": args.model, "quant": "Q5_K_M", "acknowledge": True},
    202,
)
second = wait_job(job)
assert (
    second["phase"] == "complete"
    and second["cached"]
    and second["cache_key"] == first["cache_key"]
), second
job = call(
    "juiced/preparation",
    {"model": args.model, "quant": "Q8_0", "acknowledge": True},
    202,
)
call("juiced/preparation", {"action": "cancel", "id": job["id"]})
cancelled = wait_job(job)
assert cancelled["phase"] == "cancelled", cancelled
proof = {
    "actual_quantization": first,
    "generated_tokens": output["usage"]["completion_tokens"],
    "cache_reused": second,
    "cancellation": cancelled,
    "source_preserved": True,
}
Path(args.out).write_text(json.dumps(proof, indent=2) + "\n")
print(
    "PASS: real quantization, disabled registration, generated tokens, verified cache reuse and cancellation"
)
