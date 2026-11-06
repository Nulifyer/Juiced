#!/usr/bin/env python3
"""Real lifecycle gate: profile settings survive switches; failure restores service."""

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


def call(path, body=None):
    request = urllib.request.Request(
        base + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=180) as response:
        return json.load(response)


def wait(predicate):
    for _ in range(900):
        model = next(m for m in call("juiced/catalog")["data"] if m["id"] == args.model)
        if predicate(model):
            return model
        time.sleep(0.1)
    raise AssertionError("Execution switch did not settle")


def select(profile, **options):
    current = call("juiced/execution?model=" + args.model)
    call(
        "juiced/execution",
        {
            "model": args.model,
            "profile": profile,
            "reload": True,
            "acknowledge": True,
            "expected_selected": current["selected"],
            **options,
        },
    )
    return wait(
        lambda m: not m.get("operation", {}).get("pending")
        and m.get("execution_profile") == profile
        and m["loaded"]
    )


before = call("models/" + args.model + "/options")
original = next(m for m in call("juiced/catalog")["data"] if m["id"] == args.model)
assert original["downloaded"] and original["recipe"] == "llamacpp"
call("juiced/policy", {"model": args.model, "policy": "hot"})
wait(lambda m: m["loaded"])
select("llamacpp:cpu")
call("models/" + args.model + "/options", {"ctx_size": 256})
select("registered")
assert (
    call("models/" + args.model + "/options")["effective"]["ctx_size"]
    == before["effective"]["ctx_size"]
)
select("llamacpp:cpu")
assert call("models/" + args.model + "/options")["effective"]["ctx_size"] == 256
before_trial = wait(lambda m: m["loaded"] and not m.get("operation", {}).get("pending"))
call(
    "juiced/execution",
    {
        "model": args.model,
        "profile": "llamacpp:vulkan",
        "device": "Vulkan999",
        "reload": True,
        "expected_selected": "llamacpp:cpu",
    },
)
restored = wait(
    lambda m: m["loaded"] and m.get("operation", {}).get("state") == "failed"
)
assert (
    restored["execution_profile"] == "llamacpp:cpu" and restored["policy"] == "hot"
), restored
assert "previous execution profile restored" in restored["operation"]["error"], restored
output = call(
    "chat/completions",
    {
        "model": args.model,
        "messages": [{"role": "user", "content": "Once upon a time"}],
        "max_tokens": 8,
        "temperature": 0,
    },
)
assert output["usage"]["completion_tokens"] > 0
select("registered")
call("models/" + args.model + "/options", before["saved"])
call("juiced/policy", {"model": args.model, "policy": original["policy"]})
Path(args.out).write_text(
    json.dumps(
        {
            "independent_settings": True,
            "failed_gpu_profile": restored["operation"],
            "previous_profile_restored": True,
            "hot_policy_preserved": True,
            "generation_after_recovery": output["usage"],
            "original_profile_restored": True,
        },
        indent=2,
    )
    + "\n"
)
print(
    "PASS: independent profile settings, failed GPU trial, Hot recovery and generated tokens"
)
