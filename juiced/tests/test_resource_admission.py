#!/usr/bin/env python3
"""Real CPU engine acceptance for native admission, queues and maintenance."""

import argparse
import concurrent.futures
import json
from pathlib import Path
import tempfile
import time

from test_cache_admission import BODY, Fixture, MODEL, PREFIXES


def acceptance(args):
    proof = {
        "scope": "isolated Stories15M CPU serving; no hardware or model quality qualification"
    }
    with tempfile.TemporaryDirectory(prefix="juiced-resource-admission-") as directory:
        fixture = Fixture(args, Path(directory))
        try:
            fixture.start("fixture-inference", "fixture-operator")
            fixture.ok(
                "/v1/models/" + MODEL + "/options",
                {
                    "llamacpp_backend": "cpu",
                    "ctx_size": 128,
                    "llamacpp_args": "-t 2 --parallel 1 --chat-template chatml",
                },
            )
            fixture.ok("/v1/juiced/policy", {"model": MODEL, "policy": "on-demand"})
            fixture.load()
            loaded = next(
                row
                for row in fixture.ok("/v1/health")["all_models_loaded"]
                if row["model_name"] == MODEL
            )
            options = loaded["recipe_options"]
            options.pop("pinned", None)
            fixture.ok("/v1/juiced/operation", {"model": MODEL, "action": "unload"})
            fixture.wait_loaded(False)
            assert fixture.ok("/v1/juiced/admission")["resident"] == 0
            profile = {
                "checkpoint": loaded["checkpoint"],
                "options": options,
                "priority": "interactive",
                "concurrency": 1,
                "completion_ms": 30000,
                "load_bytes": {"host": 512 * 1024**2},
                "resident_bytes": {"host": 128 * 1024**2},
                "request_limits": {
                    "input_bytes": 65536,
                    "batch": 1,
                    "output_tokens": 128,
                },
                "request_bytes": {"host": 64 * 1024**2},
                "state_bytes": {"host": 16 * 1024**2},
                "source": "bounded synthetic CPU fixture; not a general estimate",
            }
            settings = {
                "mode": "protected",
                "interactive_wait_ms": 1000,
                "pools": {
                    "host": {
                        "capacity_bytes": 2 * 1024**3,
                        "essential_headroom_bytes": 128 * 1024**2,
                        "source": "single host physical pool with live headroom",
                    }
                },
                "models": {MODEL: profile},
            }
            fixture.ok("/internal/set", {"admission": settings})
            fixture.load()
            state = fixture.ok("/v1/juiced/admission")
            assert (
                state["resident"] == 1
                and state["pools"]["host"]["reserved_bytes"] == 128 * 1024**2
            ), state
            assert state["latency_qualified"] is False
            proof["load_peak_to_residency"] = True
            matrix = []
            for prefix in PREFIXES:
                for stream in (False, True):
                    status, _, data = fixture.ask(
                        "native", prefix=prefix, streaming=stream, cache_prompt=False
                    )
                    assert status == 200 and "error" not in data, (status, data)
                    state = fixture.ok(prefix + "juiced/admission")
                    assert (
                        state["active"] == 0
                        and state["uncertain"] == 0
                        and state["pending"] == 0
                    ), state
                    matrix.append(
                        {"prefix": prefix, "stream": stream, "released": True}
                    )
            proof["native_matrix"] = matrix
            inference = {"Authorization": "Bearer fixture-inference"}
            status, _, _ = fixture.call(
                "/v1/juiced/admission", {"paused": True}, headers=inference
            )
            assert status == 401, status
            fixture.ok("/v1/juiced/admission", {"paused": True})
            before = fixture.ok("/v1/juiced/admission")["admitted_total"]
            status, _, data = fixture.ask("paused", cache_prompt=False)
            assert (
                status == 503 and data["error"]["details"]["code"] == "admission_paused"
            ), (status, data)
            assert fixture.ok("/v1/juiced/admission")["admitted_total"] == before
            fixture.ok("/v1/juiced/admission", {"paused": False})
            assert fixture.ask("resumed", cache_prompt=False)[0] == 200
            proof["operator_pause_resume"] = True
            status, _, data = fixture.call(
                "/internal/set", {"admission": {"mode": "legacy"}}
            )
            assert (
                status == 409
                and data["error"]["details"]["code"] == "admission_configuration_busy"
            ), (status, data)
            proof["live_topology_change_rejected"] = True
            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                results = list(
                    pool.map(
                        lambda index: fixture.ask(
                            f"parallel-{index}", cache_prompt=False
                        )[0],
                        range(8),
                    )
                )
            assert results == [200] * 8, results
            state = fixture.ok("/v1/juiced/admission")
            assert (
                state["active"] == 0
                and state["uncertain"] == 0
                and state["pending"] == 0
            ), state
            proof["concurrent_native_requests"] = len(results)
            for path, body in (
                (
                    "/api/chat",
                    {
                        "model": MODEL,
                        "messages": BODY["messages"],
                        "stream": False,
                        "options": {"num_predict": 8},
                    },
                ),
                (
                    "/v1/messages",
                    {"model": MODEL, "messages": BODY["messages"], "max_tokens": 8},
                ),
                (
                    "/mcp",
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {
                            "name": "lemonade_chat",
                            "arguments": {
                                "model": MODEL,
                                "messages": BODY["messages"],
                                "max_tokens": 8,
                            },
                        },
                    },
                ),
            ):
                before = fixture.ok("/v1/juiced/admission")["admitted_total"]
                status, _, data = fixture.call(path, body)
                assert status == 200, (path, status, data)
                after = fixture.ok("/v1/juiced/admission")
                assert after["admitted_total"] > before and after["active"] == 0, (
                    path,
                    data,
                    after,
                )
            proof["direct_client_ingress"] = ["ollama", "anthropic", "mcp"]
            fixture.ok("/v1/juiced/operation", {"model": MODEL, "action": "unload"})
            fixture.wait_loaded(False)
            state = fixture.ok("/v1/juiced/admission")
            assert (
                state["resident"] == 0 and state["pools"]["host"]["reserved_bytes"] == 0
            ), state
            proof["unload_releases_residency"] = True
            args.out.write_text(json.dumps(proof, indent=2) + "\n")
        finally:
            fixture.stop()
            fixture.log.close()
    print(
        "PASS: real CPU load/request leases, protocol ingress, concurrency and maintenance"
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--model-file", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    for key in ("binary", "engine", "model_file", "out"):
        setattr(args, key, getattr(args, key).resolve())
    acceptance(args)


if __name__ == "__main__":
    main()
