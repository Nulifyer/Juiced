#!/usr/bin/env python3
"""Packaged CPU defaults and persisted explicit binary overrides use native owners."""

import argparse
import json
from pathlib import Path
import shlex
import tempfile
from unittest.mock import patch

from test_cache_admission import Fixture, MODEL


def acceptance(args):
    defaults = json.loads(args.defaults_file.read_text())
    assert defaults["llamacpp"]["cpu_bin"] == "/opt/juiced/llama-cpu", defaults
    assert defaults["llamacpp"]["vulkan_bin"] == "/opt/juiced/llama", defaults
    proof = {
        "scope": "native application-default and explicit-override selection with isolated CPU serving; no deployment qualification"
    }
    with tempfile.TemporaryDirectory(prefix="juiced-runtime-defaults-") as directory:
        root = Path(directory)
        args.engine = args.cpu_runtime / "llama-server"
        fixture = Fixture(args, root)
        config_file = root / "config/config.json"
        config = json.loads(config_file.read_text())
        config.pop("llamacpp")
        config_file.write_text(json.dumps(config))
        defaults["llamacpp"]["cpu_bin"] = str(args.cpu_runtime)
        defaults_file = root / "application-defaults.json"
        defaults_file.write_text(json.dumps(defaults))

        def start():
            with patch.dict(
                "os.environ", {"LEMONADE_DEFAULTS_PATH": str(defaults_file)}
            ):
                fixture.start()

        def profile():
            fixture.ok("/v1/load", {"model_name": MODEL})
            return fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})

        try:
            start()
            fixture.ok("/v1/juiced/policy", {"model": MODEL, "policy": "on-demand"})
            fixture.ok(
                "/v1/models/" + MODEL + "/options",
                {
                    "ctx_size": 128,
                    "llamacpp_backend": "cpu",
                    "llamacpp_args": "--slot-save-path "
                    + shlex.quote(str(root / "snapshots")),
                },
            )
            initial = profile()
            assert initial["configured"], initial
            assert initial["identity"]["launch_executable"]["path"] == str(
                args.cpu_runtime / "llama-server"
            ), initial
            assert initial["identity"]["runtime_image"]["executable"]["path"] == str(
                args.cpu_runtime / "bin/llama-server"
            ), initial
            status, _, reply = fixture.ask("default-cpu", max_tokens=4)
            assert status == 200, reply
            fixture.stop()
            start()
            assert profile()["fingerprint"] == initial["fingerprint"]
            proof["application_default_serves_and_survives_restart"] = True
            fixture.stop()
            override = root / "explicit-wrapper"
            override.write_text(
                "#!/bin/sh\nexec "
                + shlex.quote(str(args.cpu_runtime / "llama-server"))
                + ' "$@"\n'
            )
            override.chmod(0o700)
            config = json.loads(config_file.read_text())
            config["llamacpp"] = {"cpu_bin": str(override)}
            config_file.write_text(json.dumps(config))
            start()
            selected = profile()
            assert selected["configured"] and selected["identity"]["launch_executable"][
                "path"
            ] == str(override), selected
            assert selected["fingerprint"] != initial["fingerprint"], selected
            status, _, reply = fixture.ask("explicit-cpu", max_tokens=4)
            assert status == 200, reply
            fixture.stop()
            assert json.loads(config_file.read_text())["llamacpp"]["cpu_bin"] == str(
                override
            )
            start()
            assert profile()["fingerprint"] == selected["fingerprint"]
            proof["explicit_override_serves_and_remains_persisted_across_restart"] = (
                True
            )
        finally:
            fixture.stop()
            fixture.log.close()
            args.out.with_suffix(".server.log").write_bytes(
                (root / "server.log").read_bytes()
            )
    args.out.write_text(json.dumps(proof, indent=2) + "\n")
    print(
        "PASS: packaged CPU default, normal serving and preserved explicit override through restart"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "cpu-runtime", "defaults-file", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    for name in ("binary", "cpu_runtime", "defaults_file", "model_file"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    acceptance(args)


if __name__ == "__main__":
    main()
