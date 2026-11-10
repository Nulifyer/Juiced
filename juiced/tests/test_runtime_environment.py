#!/usr/bin/env python3
"""Actual CPU exec-environment identity and conservative unsupported-input behavior."""

import argparse
import hashlib
import json
from pathlib import Path
import shlex
import subprocess
import tempfile
import urllib.request

from test_cache_admission import Fixture, MODEL
from test_execution_inputs import TEMPLATE


def acceptance(args):
    proof = {
        "scope": "isolated Linux CPU initial exec environment; not runtime environment mutation or complete physical identity"
    }
    with tempfile.TemporaryDirectory(prefix="juiced-runtime-environment-") as directory:
        root = Path(directory)
        template, threads, unsupported = (
            root / "template.jinja",
            root / "threads",
            root / "unsupported",
        )
        template.write_text(TEMPLATE)
        threads.write_text("2")
        wrapper = root / "wrapper"
        wrapper.write_text(
            "#!/bin/sh\ncd "
            + shlex.quote(str(root))
            + "\nexport LLAMA_ARG_CHAT_TEMPLATE_FILE=template.jinja"
            + '\nexport OMP_NUM_THREADS="$(cat '
            + shlex.quote(str(threads))
            + ')"'
            + "\nif [ -f "
            + shlex.quote(str(unsupported))
            + " ]; then export LLAMA_ARG_FUTURE_INPUT=private-fixture-sentinel; fi"
            + "\nexec "
            + shlex.quote(str(args.engine))
            + ' "$@"\n'
        )
        wrapper.chmod(0o700)
        args.engine = wrapper
        fixture = Fixture(args, root)
        try:
            fixture.start()
            subprocess.run(
                [
                    "python3",
                    str(Path(__file__).with_name("test_engine_cache.py")),
                    "--url",
                    fixture.url,
                    "--model-file",
                    str(args.model_file),
                    "--snapshots",
                    str(root / "snapshots"),
                    "--environment-template",
                    "--out",
                    str(root / "cache.json"),
                ],
                check=True,
            )
            useful = json.loads((root / "cache.json").read_text())
            proof["useful_reuse_tokens"] = useful["reuse"]["timings"]["cache_n"]

            def profile():
                return fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})

            def props(current):
                with urllib.request.urlopen(
                    current["address"] + "/props", timeout=10
                ) as response:
                    return json.load(response)

            def reload():
                fixture.ok("/v1/unload", {})
                fixture.ok("/v1/load", {"model_name": MODEL})
                return profile()

            before = profile()
            assert before["configured"] and before["qualified"], before
            assert props(before)["chat_template"] == TEMPLATE
            fixture.stop()
            fixture.start()
            fixture.ok("/v1/load", {"model_name": MODEL})
            stable = profile()
            assert stable["fingerprint"] == before["fingerprint"]
            proof["unchanged_restart_identity"] = True
            changed_template = TEMPLATE.replace("assistant", "Assistant")
            assert len(changed_template) == len(TEMPLATE)
            template.write_text(changed_template)
            assert profile()["fingerprint"] == before["fingerprint"]
            changed = reload()
            assert props(changed)["chat_template"] == changed_template
            for stream in (False, True):
                status, _, response = fixture.ask(
                    "environment", streaming=stream, max_tokens=4
                )
                assert status == 200, response
            if args.expect_gap:
                assert changed["fingerprint"] == before["fingerprint"], changed
                proof["old_binary_gap"] = {
                    "same_fingerprint": True,
                    "changed_applied_template": True,
                    "json_sse_serving": True,
                }
            else:
                assert changed["configured"] and not changed["qualified"], changed
                assert changed["fingerprint"] != before["fingerprint"], changed
                captured = changed["identity"]["environment"]["files"][
                    "LLAMA_ARG_CHAT_TEMPLATE_FILE"
                ][0]
                assert (
                    captured["sha256"]
                    == hashlib.sha256(template.read_bytes()).hexdigest()
                )
                threads.write_text("3")
                scalar = reload()
                assert (
                    scalar["configured"]
                    and scalar["fingerprint"] != changed["fingerprint"]
                ), scalar
                assert (
                    scalar["identity"]["environment"]["scalars"]["OMP_NUM_THREADS"]
                    == "3"
                )
                unsupported.touch()
                unknown = reload()
                assert not unknown["configured"] and not unknown["qualified"], unknown
                assert "environment" in unknown["configuration_reason"].lower(), unknown
                assert "private-fixture-sentinel" not in json.dumps(unknown), unknown
                for stream in (False, True):
                    status, _, response = fixture.ask(
                        "unsupported-environment", streaming=stream, max_tokens=4
                    )
                    assert status == 200, response
                unsupported.unlink()
                recovered = reload()
                assert (
                    recovered["configured"]
                    and recovered["fingerprint"] == scalar["fingerprint"]
                ), recovered
                fixture.ok("/v1/unload", {})
                fixture.ok(
                    "/v1/models/" + MODEL + "/options",
                    {
                        "llamacpp_args": "-t 2 --parallel 1 --chat-template chatml --slot-save-path "
                        + shlex.quote(str(root / "snapshots"))
                    },
                )
                fixture.ok("/v1/load", {"model_name": MODEL})
                overridden = profile()
                assert (
                    not overridden["configured"]
                    and "observed applied template"
                    in overridden["configuration_reason"]
                ), overridden
                status, _, response = fixture.ask(
                    "overridden-environment", max_tokens=4
                )
                assert status == 200, response
                proof.update(
                    {
                        "file_content_invalidates": True,
                        "wrapper_scalar_invalidates": True,
                        "unsupported_input_unqualified_without_value_disclosure": True,
                        "ordinary_json_sse_serving": True,
                        "resolved_input_recovers_without_manager_restart": True,
                        "relative_file_resolved_from_actual_child_directory": True,
                        "mismatched_applied_template_remains_unqualified": True,
                    }
                )
        finally:
            fixture.stop()
            fixture.log.close()
            args.out.with_suffix(".server.log").write_bytes(
                (root / "server.log").read_bytes()
            )
    args.out.write_text(json.dumps(proof, indent=2) + "\n")
    print(
        "PASS: "
        + (
            "old runtime environment identity gap reproduced"
            if args.expect_gap
            else "actual exec-environment capture, invalidation, restart and unqualified serving"
        )
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "engine", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--expect-gap", action="store_true")
    args = parser.parse_args()
    for name in ("binary", "engine", "model_file"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    acceptance(args)


if __name__ == "__main__":
    main()
