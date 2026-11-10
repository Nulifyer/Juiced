#!/usr/bin/env python3
"""Isolated real CPU profile binding and read-only native host-fit acceptance."""

import argparse
import json
from pathlib import Path
import tempfile
import time

from test_cache_admission import Fixture, MODEL, PREFIXES


def run(args):
    checks = []

    def check(condition, label):
        checks.append({"check": label, "passed": bool(condition)})
        if not condition:
            raise AssertionError(label)

    with tempfile.TemporaryDirectory(prefix="juiced-suitability-") as directory:
        root = Path(directory)
        fixture = Fixture(args, root)
        state_file = root / "config/juiced/state.json"

        def settled():
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                row = next(
                    row
                    for row in fixture.ok("/v1/juiced/catalog")["data"]
                    if row["id"] == MODEL
                )
                if not row.get("operation", {}).get("pending", False):
                    return
                time.sleep(0.02)
            raise AssertionError("model operation did not settle")

        def assessment(prefix="/v1/"):
            return fixture.ok(prefix + "juiced/suitability?model=" + MODEL)

        def settings(options, checkpoint, peak=64 * 1024**2, capacity=1024**3):
            return {
                "mode": "legacy",
                "pools": {
                    "host": {
                        "capacity_bytes": capacity,
                        "source": "Synthetic ceiling with native host observation",
                    }
                },
                "models": {
                    MODEL: {
                        "checkpoint": checkpoint,
                        "options": options,
                        "source": "Controlled CPU ceiling fixture, not a measured memory peak",
                        "load_bytes": {"host": peak},
                        "resident_bytes": {"host": peak // 2},
                        "request_bytes": {"host": peak // 4},
                        "state_bytes": {"host": peak // 4},
                    }
                },
            }

        try:
            fixture.start()
            before = state_file.read_bytes()
            for prefix in PREFIXES:
                result = assessment(prefix)
                check(
                    result["resources"]["fit"] == "unknown"
                    and result["read_only"]
                    and not result["enabled"],
                    prefix + " preserves unknown estimates and Disabled policy",
                )
            check(
                state_file.read_bytes() == before
                and not fixture.ok("/v1/health")["all_models_loaded"],
                "inspection does not publish state or load a worker",
            )
            fixture.ok("/v1/juiced/policy", {"model": MODEL, "policy": "on-demand"})
            settled()
            fixture.load()
            loaded = next(
                row
                for row in fixture.ok("/v1/health")["all_models_loaded"]
                if row["model_name"] == MODEL
            )
            options = loaded["recipe_options"]
            options.pop("pinned", None)
            checkpoint = loaded["checkpoint"]
            fixture.ok("/v1/unload", {})
            fixture.wait_loaded(False)
            fixture.configure_admission(settings(options, checkpoint))
            fit = assessment()
            check(
                fit["resources"]["fit"] == "fits" and fit["context_tokens"] == 2048,
                "selected saved settings match the resource profile captured from an actual CPU worker",
            )
            check(
                fit["resources"]["pools"]["host"]["required_peak_bytes"] == 64 * 1024**2
                and not fit["resources"]["hardware_qualified"],
                "whole resident/request/state peak retains explicit qualification limits",
            )
            fixture.ok("/v1/juiced/policy", {"model": MODEL, "policy": "disabled"})
            settled()
            before = state_file.read_bytes()
            reservations = fixture.ok("/v1/juiced/admission")
            disabled_fit = assessment()
            after = fixture.ok("/v1/juiced/admission")
            for key in (
                "resident",
                "pending",
                "active",
                "admitted",
                "rejected",
                "uncertain",
            ):
                if key in reservations:
                    check(
                        after[key] == reservations[key],
                        "host fit does not alter " + key,
                    )
            check(
                disabled_fit["resources"]["fit"] == "fits"
                and not disabled_fit["enabled"]
                and state_file.read_bytes() == before,
                "a fitting model remains Disabled and its committed state is unchanged",
            )
            fixture.ok("/v1/juiced/admission", {"paused": True})
            paused = assessment()
            check(
                not paused["resources"]["admission_open"]
                and paused["resources"]["fit"] == "fits",
                "pause remains distinct from physical fit",
            )
            fixture.ok("/v1/juiced/admission", {"paused": False})
            fixture.ok(f"/v1/models/{MODEL}/options", {"ctx_size": 4096})
            changed = assessment()
            check(
                changed["resources"]["fit"] == "unknown"
                and changed["resources"]["reason_code"] == "admission_profile_mismatch",
                "changed context cannot reuse the former estimate",
            )
            fixture.ok(f"/v1/models/{MODEL}/options", {"ctx_size": -1})
            check(
                assessment()["resources"]["reason_code"] == "context_ceiling_unknown",
                "automatic context is not advertised as measured serving fit",
            )
            fixture.ok(f"/v1/models/{MODEL}/options", {"ctx_size": 2048})
            fixture.configure_admission(
                settings(options, checkpoint, capacity=16 * 1024**2)
            )
            check(
                assessment()["resources"]["fit"] == "does_not_fit",
                "declared peak above the pool ceiling is reported without attempting a load",
            )
            fixture.configure_admission(
                settings(options, checkpoint, peak=1024**4, capacity=2 * 1024**4)
            )
            check(
                assessment()["resources"]["fit"] == "limited_fit",
                "synthetic oversized demand exposes observed headroom pressure without allocating that memory",
            )
            check(
                not fixture.ok("/v1/health")["all_models_loaded"],
                "all post-unload assessments leave the worker unloaded",
            )
            check(
                fixture.call("/v1/juiced/suitability")[0] == 400,
                "missing model rejects with an actionable client error",
            )
            check(
                fixture.call("/v1/juiced/suitability?model=missing-model")[0] == 400,
                "unknown model rejects without creating a registration",
            )
        finally:
            fixture.stop()
            args.out.with_suffix(".server.log").write_bytes(
                (root / "server.log").read_bytes()
            )
            args.out.write_text(
                json.dumps(
                    {
                        "scope": "Isolated real CPU execution-profile binding and read-only host-fit projection; synthetic resource ceilings, no actual memory-peak or hardware qualification",
                        "checks": checks,
                    },
                    indent=2,
                )
                + "\n"
            )
    print(f"Model suitability: {len(checks)} checks passed")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "engine", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    for name in ("binary", "engine", "model_file"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    run(args)
