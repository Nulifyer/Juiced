#!/usr/bin/env python3
"""Actual CPU template identity, companion application and unknown-input fallback."""

import argparse
import hashlib
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time
import urllib.request

from test_cache_admission import Fixture, MODEL

TEMPLATE = """{% for message in messages %}{{ message['role'] }}: {{ message['content'] }}
{% endfor %}{% if add_generation_prompt %}assistant: {% endif %}"""


def acceptance(args):
    proof = {
        "scope": "isolated CPU external-file and declared-definition identity; no adapter, draft quality or complete runtime qualification"
    }
    with tempfile.TemporaryDirectory(prefix="juiced-execution-inputs-") as directory:
        root = Path(directory)
        fixture = Fixture(args, root)
        template = root / "custom, template.jinja"
        template.write_text(TEMPLATE)
        source_checksum = hashlib.sha256(args.model_file.read_bytes()).hexdigest()
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
                    "--template-file",
                    str(template),
                    "--out",
                    str(root / "template-cache.json"),
                ],
                check=True,
            )
            proof["template_useful_reuse"] = json.loads(
                (root / "template-cache.json").read_text()
            )

            def profile():
                return fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})

            def props(current):
                with urllib.request.urlopen(
                    current["address"] + "/props", timeout=10
                ) as response:
                    return json.load(response)

            before = profile()
            assert before["qualified"] and before["configured"], before
            captured = before["identity"]["argument_files"]["--chat-template-file:0"][0]
            assert (
                captured["sha256"] == hashlib.sha256(template.read_bytes()).hexdigest()
            )
            assert props(before)["chat_template"] == TEMPLATE
            template.write_text("Setup instructions.\n" + TEMPLATE)
            held = profile()
            assert held["fingerprint"] == before["fingerprint"]
            fixture.ok("/v1/unload", {})
            fixture.ok("/v1/load", {"model_name": MODEL})
            changed = profile()
            assert changed["fingerprint"] != before["fingerprint"]
            assert not changed["qualified"] and changed["configured"], changed
            assert props(changed)["chat_template"] == template.read_text()
            fixture.ok("/v1/juiced/cache", {"mode": "persistent"})
            for streaming in (False, True):
                status, headers, response = fixture.ask(
                    "changed-template", streaming=streaming, max_tokens=4
                )
                assert (
                    status == 200
                    and headers.get("X-Juiced-Cache") == "unqualified-profile"
                ), (status, headers, response)
            proof["template_change"] = {
                "old_generation": before["worker_generation"],
                "new_generation": changed["worker_generation"],
                "old_fingerprint": before["fingerprint"],
                "new_fingerprint": changed["fingerprint"],
                "old_worker_keeps_frozen_identity": True,
                "new_profile_serves_json_sse_without_old_qualification": True,
            }
            fixture.stop()
            fixture.start()
            fixture.ok("/v1/load", {"model_name": MODEL})
            assert profile()["fingerprint"] == changed["fingerprint"]
            proof["unchanged_template_restart_identity"] = True

            fixture.ok("/v1/unload", {})
            valid_args = "--slot-save-path " + shlex.quote(str(root / "snapshots"))
            fixture.ok(
                "/v1/models/" + MODEL + "/options",
                {
                    "ctx_size": 128,
                    "llamacpp_backend": "cpu",
                    "llamacpp_args": valid_args,
                },
            )
            first, second = root / "draft-a.gguf", root / "draft-b.gguf"
            shutil.copyfile(args.model_file, first)
            shutil.copyfile(args.model_file, second)

            def register(draft):
                fixture.ok(
                    "/v1/models/register",
                    {
                        "model_name": "user." + MODEL,
                        "recipe": "llamacpp",
                        "source": "local_path",
                        "checkpoints": {
                            "main": str(args.model_file),
                            "draft": str(draft),
                        },
                        "labels": ["chat"],
                    },
                )

            register(first)
            fixture.ok("/v1/load", {"model_name": MODEL})
            initial = profile()
            register(second)
            pending = next(
                x for x in fixture.ok("/v1/models")["data"] if x["id"] == MODEL
            )
            assert pending["runtime"]["settings_pending"], pending
            for _ in range(100):
                catalog = next(
                    x
                    for x in fixture.ok("/v1/juiced/catalog")["data"]
                    if x["id"] == MODEL
                )
                if "runtime" in catalog and catalog.get("checkpoints", {}).get(
                    "draft"
                ) == str(second):
                    break
                time.sleep(0.1)
            else:
                raise AssertionError(
                    "Catalog did not refresh the desired companion definition"
                )
            assert catalog["runtime"]["settings_pending"], catalog
            assert profile()["identity"]["checkpoints"]["draft"] == str(first)
            fixture.ok("/v1/load", {"model_name": MODEL})
            applied = profile()
            assert applied["worker_generation"] != initial["worker_generation"]
            assert applied["identity"]["checkpoints"]["draft"] == str(second)
            current = next(
                x for x in fixture.ok("/v1/models")["data"] if x["id"] == MODEL
            )
            assert not current["runtime"]["settings_pending"], current
            fixture.ok("/v1/load", {"model_name": MODEL})
            assert profile()["worker_generation"] == applied["worker_generation"]
            proof["companion_apply"] = {
                "old_generation": initial["worker_generation"],
                "applied_generation": applied["worker_generation"],
                "direct_and_catalog_pending": True,
                "matching_repeat_reuses_generation": True,
            }

            fixture.ok("/v1/unload", {})
            fixture.ok(
                "/v1/models/" + MODEL + "/options",
                {"llamacpp_args": valid_args + " --log-timestamps"},
            )
            fixture.ok("/v1/load", {"model_name": MODEL})
            unknown = profile()
            assert not unknown["configured"] and not unknown["qualified"], unknown
            assert "--log-timestamps" in unknown["configuration_reason"], unknown
            status, _, rejected = fixture.call(
                "/v1/juiced/cache/qualify",
                {
                    "model": MODEL,
                    "fingerprint": unknown["fingerprint"],
                    "evidence": "must reject unknown inputs",
                },
            )
            assert status == 400 and "--log-timestamps" in json.dumps(rejected), (
                status,
                rejected,
            )
            for streaming in (False, True):
                status, _, response = fixture.ask(
                    "unknown-input", streaming=streaming, max_tokens=4
                )
                assert status == 200, response
            proof["unknown_argument"] = {
                "reason": unknown["configuration_reason"],
                "qualification_rejected": True,
                "ordinary_json_sse_serving": True,
            }
            assert (
                hashlib.sha256(args.model_file.read_bytes()).hexdigest()
                == source_checksum
            )
            proof["source_weights_unchanged"] = True
        finally:
            fixture.stop()
            fixture.log.close()
            args.out.with_suffix(".server.log").write_bytes(
                (root / "server.log").read_bytes()
            )
    args.out.write_text(json.dumps(proof, indent=2) + "\n")
    print(
        "PASS: template useful reuse/invalidation, companion pending/apply and unknown-input serving"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "engine", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    for name in ("binary", "engine", "model_file"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    acceptance(args)


if __name__ == "__main__":
    main()
