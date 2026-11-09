#!/usr/bin/env python3
"""Verify producer retirement without creating or altering any model weights."""

import argparse
import json
from pathlib import Path
import urllib.error
import urllib.request


def acceptance(args):
    def call(prefix, path, body=None, expected=200):
        request = urllib.request.Request(
            args.url.rstrip("/") + prefix + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        try:
            response = urllib.request.urlopen(request, timeout=15)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            result = json.load(response)
            assert response.status == expected, (path, response.status, result)
            return result

    before = call("/v1/", "juiced/preparation")
    artifacts = call("/v1/", "juiced/preparation?cache=1")
    checked = 0
    for prefix in ("/v1/", "/v0/", "/api/v1/", "/api/v0/"):
        rejected = call(prefix, "juiced/preparation?model=" + args.model, expected=410)
        assert rejected["error"]["code"] == "feature_retired", rejected
        for action in ("", "plan", "start", "resume", "convert", "quantize"):
            rejected = call(
                prefix,
                "juiced/preparation",
                {
                    "action": action,
                    "model": args.model,
                    "quant": "Q5_K_M",
                    "acknowledge": True,
                },
                410,
            )
            assert rejected["error"]["code"] == "feature_retired", rejected
            checked += 1
    after = call("/v1/", "juiced/preparation")
    assert before == after and after["retired"], (before, after)
    assert artifacts == call("/v1/", "juiced/preparation?cache=1")
    if args.runtime_root:
        assert args.runtime_root.is_dir(), args.runtime_root
        assert not (args.runtime_root / "preparation").exists()
        forbidden = [
            p
            for p in args.runtime_root.rglob("*")
            if (p.name == "llama" and not p.is_dir())
            or any(
                token in p.name.lower()
                for token in (
                    "llama-quantize",
                    "convert_hf_to_gguf",
                    "prepare.py",
                    "quantize-impl",
                )
            )
        ]
        assert not forbidden, forbidden
    proof = {
        "schema": 1,
        "passed": True,
        "rejected_creation_requests": checked,
        "prefixes": 4,
        "legacy_records_unchanged": True,
        "artifact_inventory_unchanged": True,
        "producer_free_runtime_checked": bool(args.runtime_root),
        "scope": "Retired producer rejection and retained metadata; no converter or inference executed",
    }
    args.out.write_text(json.dumps(proof, indent=2) + "\n")
    return proof


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", required=True)
    parser.add_argument("--model", default="Stories15M-Q4_0")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--runtime-root", type=Path)
    print(json.dumps(acceptance(parser.parse_args()), indent=2))
