#!/usr/bin/env python3
"""Actual CPU discovery context against the owned worker's read-only properties."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import time
import urllib.request

from test_cache_admission import ALIAS, Fixture, MODEL, PREFIXES


def acceptance(args):
    digest = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
    identities = {
        name: digest(getattr(args, name)) for name in ("binary", "engine", "model_file")
    }
    proof = {
        "schema": 1,
        "passed": False,
        "identities": identities,
        "checks": [],
        "scope": "Isolated tiny-model CPU metadata and functional serving; no quality, accelerator, concurrency or fit qualification",
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="juiced-context-") as directory:
        fixture = Fixture(args, Path(directory))

        def record(label, value):
            proof["checks"].append({"label": label, "value": value})

        def catalog_ready(expected):
            deadline = time.monotonic() + 10
            while True:
                row = next(
                    m
                    for m in fixture.ok("/v1/juiced/catalog")["data"]
                    if m["id"] == MODEL
                )
                if expected(row) and not row.get("operation", {}).get("pending", False):
                    return row
                assert time.monotonic() < deadline, row
                time.sleep(0.05)

        def verify_loaded(label, configured):
            worker = next(
                m
                for m in fixture.ok("/v1/health")["all_models_loaded"]
                if m["model_name"] == MODEL
            )
            address = worker["backend_url"].removesuffix("/v1")
            with urllib.request.urlopen(address + "/props", timeout=3) as response:
                props = json.load(response)
            expected = props["default_generation_settings"]["n_ctx"]
            assert isinstance(expected, int) and expected > 0, props
            assert (
                worker["context_observation"]["per_request_tokens"] == expected
            ), worker
            row = catalog_ready(
                lambda m: m.get("context", {}).get("worker_generation")
                == worker["worker_generation"]
            )
            assert row["context"]["configured_tokens"] == configured, row
            assert (
                row["context"]["advertised_tokens"] == expected
                and row["context_length"] == expected
            ), row
            for prefix in PREFIXES:
                for model in (MODEL, ALIAS):
                    data = fixture.ok(prefix + "models/" + model)
                    assert data["context_length"] == expected, data
                    assert (
                        data["context"]["observed_per_request_tokens"] == expected
                    ), data
                    assert (
                        data["context"]["advertised_source"] == "runtime_observation"
                    ), data
                    assert (
                        data["context"]["worker_generation"]
                        == data["runtime"]["worker_generation"]
                    ), data
                models = fixture.ok(prefix + "models")["data"]
                assert (
                    next(m for m in models if m["id"] == MODEL)["context_length"]
                    == expected
                ), models
                capabilities = fixture.ok(prefix + "juiced/capabilities")["models"]
                contract = next(m for m in capabilities if m["model"] == MODEL)
                assert contract["context"]["advertised_tokens"] == expected, contract
                assert (
                    contract["limits"]["per_request_context_tokens"] == expected
                ), contract
                assert (
                    contract["limits"]["observed_slots"] == props["total_slots"]
                ), contract
                assert contract["limits"]["concurrent_requests"] is None, contract
            shown = fixture.ok("/api/show", {"model": MODEL})
            assert shown["parameters"] == "num_ctx " + str(expected), shown
            assert shown["model_info"]["general.architecture"] == "llama", shown
            assert shown["model_info"]["llama.context_length"] == expected, shown
            assert "general.parameter_count" not in shown["model_info"], shown
            mcp = fixture.ok(
                "/mcp",
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {"name": "lemonade_list_models", "arguments": {}},
                },
            )
            assert not mcp["result"]["isError"], mcp
            available = json.loads(mcp["result"]["content"][1]["text"])["available"]
            assert (
                next(m for m in available if m["model_name"] == MODEL)["context_length"]
                == expected
            ), available
            record(
                label,
                {
                    "expected_from_props": expected,
                    "slots": props["total_slots"],
                    "context": row["context"],
                    "profile": row["execution_profile"],
                },
            )
            return row

        try:
            fixture.start()
            fixture.ok("/v1/juiced/policy", {"model": MODEL, "policy": "on-demand"})
            fixture.ok("/internal/aliases", {"alias": ALIAS, "target": MODEL})
            unloaded = fixture.ok("/v1/models/" + MODEL)
            assert unloaded["context"]["declared_tokens"] == 128, unloaded
            assert (
                unloaded["context"]["configured_tokens"] == 2048
                and unloaded["context"]["observed_per_request_tokens"] is None
            ), unloaded
            record("unloaded declared versus configured", unloaded["context"])
            fixture.ok(
                "/v1/load",
                {"model_name": MODEL, "ctx_size": 1024, "llamacpp_backend": "cpu"},
            )
            fixture.wait_loaded(True)
            current = verify_loaded("native load and all discovery protocols", 2048)
            assert current["context"]["applied_tokens"] == 1024, current
            fixture.ok("/v1/models/" + MODEL + "/options", {"ctx_size": 4096})
            catalog_ready(
                lambda m: m.get("context", {}).get("configured_tokens") == 4096
            )
            current = verify_loaded(
                "saved context remains pending without changing active capacity", 4096
            )
            assert (
                current["runtime"]["settings_pending"]
                and current["context"]["applied_tokens"] == 1024
            ), current
            fixture.ok("/v1/unload", {"model_name": MODEL})
            fixture.wait_loaded(False)
            fixture.stop()
            fixture.start()
            restarted = fixture.ok("/v1/models/" + MODEL)
            assert (
                restarted["context"]["configured_tokens"] == 4096
                and restarted["context"]["observed_per_request_tokens"] is None
            ), restarted
            record(
                "unchanged settings survive restart without reviving an old worker observation",
                restarted["context"],
            )
            fixture.ok("/v1/models/" + MODEL + "/options", {"ctx_size": -1})
            automatic = fixture.ok("/v1/models/" + MODEL)
            assert (
                automatic["context"]["configured_automatic"]
                and automatic["context"]["configured_tokens"] is None
            ), automatic
            assert automatic["context_length"] == 128, automatic
            fixture.ok(
                "/v1/juiced/execution", {"model": MODEL, "profile": "llamacpp:cpu"}
            )
            catalog_ready(lambda m: m.get("execution_profile") == "llamacpp:cpu")
            fixture.load()
            verify_loaded("automatic context and selected profile reload", None)
            status, _, response = fixture.ask(
                "context-functional",
                messages=[{"role": "user", "content": "Hello"}],
                max_tokens=2,
            )
            assert status == 200 and response.get("choices"), (status, response)
            record("functional CPU response", {"status": status, "has_choices": True})
            assert identities == {
                name: digest(getattr(args, name)) for name in identities
            }
            proof["source_identity_preserved"] = True
            proof["passed"] = True
        finally:
            fixture.stop()
            fixture.log.close()
            shutil.copyfile(
                Path(directory) / "server.log", args.out.with_suffix(".server.log")
            )
            args.out.write_text(json.dumps(proof, indent=2) + "\n")
    return proof


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    for name in ("binary", "engine", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    for name in ("binary", "engine", "model_file"):
        setattr(args, name, getattr(args, name).resolve())
    print(json.dumps(acceptance(args), indent=2))
