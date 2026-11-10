#!/usr/bin/env python3
"""Linux actual child executable/library identity and safe unqualified serving."""

import argparse
import hashlib
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile

from test_cache_admission import Fixture, MODEL


def checksum(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def acceptance(args):
    proof = {
        "scope": "isolated Linux CPU process-image file identity; no mapped-page, hardware, quality or full P1 qualification"
    }
    with tempfile.TemporaryDirectory(prefix="juiced-runtime-images-") as directory:
        root = Path(directory)
        runtime = root / "runtime"
        shutil.copytree(args.runtime_dir, runtime, symlinks=True)
        assert not (
            runtime / "libggml-vulkan.so"
        ).exists(), "Use the staged CPU runtime, not the Vulkan payload"
        assert not (
            runtime / "libggml-rpc.so"
        ).exists(), "CPU qualification excludes unselected runtime backends"
        proof["private_cpu_subset"] = {
            "source_is_staged_cpu_variant": True,
            "original_runtime_preserved": True,
        }
        executable = runtime / "llama-server"
        library = runtime / "libllama-server-impl.so"
        source_executable = checksum(args.runtime_dir / "llama-server")
        source_library = checksum(args.runtime_dir / "libllama-server-impl.so")
        source_model = checksum(args.model_file)
        wrapper = root / "private-wrapper"
        wrapper.write_text(
            "#!/bin/sh\nexport LD_LIBRARY_PATH="
            + shlex.quote(str(runtime))
            + "\nexec "
            + shlex.quote(str(executable))
            + ' "$@"\n'
        )
        wrapper.chmod(0o700)
        args.engine = wrapper
        wrapper_hash = checksum(wrapper)
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
                    "--out",
                    str(root / "cache.json"),
                ],
                check=True,
            )
            proof["useful_reuse"] = json.loads((root / "cache.json").read_text())

            def profile():
                return fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})

            def library_fact(current):
                return next(
                    x
                    for x in current["identity"]["runtime_image"]["modules"]
                    if x["path"] == str(library)
                )

            before = profile()
            assert before["configured"] and before["qualified"], before
            image = before["identity"]["runtime_image"]
            assert (
                image["executable"]["sha256"] == checksum(executable) != wrapper_hash
            ), image
            assert image["executable"]["path"] == str(executable), image
            assert library_fact(before)["sha256"] == checksum(library), image
            proof["actual_executable"] = image["executable"]
            proof["association"] = image["association"]
            proof["module_count"] = len(image["modules"])
            retained = root / "retained-library.so"
            library.rename(retained)
            shutil.copyfile(retained, library)
            with library.open("ab") as output:
                output.write(b"\ncontrolled-runtime-input-change\n")
            old = profile()
            assert old["fingerprint"] == before["fingerprint"], old
            assert not old["configured"] and not old["qualified"], old
            assert "Runtime executable" in old["configuration_reason"], old
            fixture.ok("/v1/juiced/cache", {"mode": "persistent"})
            for stream in (False, True):
                status, headers, reply = fixture.ask(
                    "changed-live-module", streaming=stream, max_tokens=4
                )
                assert (
                    status == 200
                    and headers.get("X-Juiced-Cache") == "unqualified-profile"
                ), (status, headers, reply)
            status, _, rejected = fixture.call(
                "/v1/juiced/cache/qualify",
                {
                    "model": MODEL,
                    "fingerprint": old["fingerprint"],
                    "evidence": "must reject changed live runtime",
                },
            )
            assert status == 400, (status, rejected)
            assert retained.is_file(), "Retained live library was reclaimed"
            fixture.ok("/v1/unload", {})
            fixture.ok("/v1/load", {"model_name": MODEL})
            changed = profile()
            assert changed["configured"] and not changed["qualified"], changed
            assert changed["fingerprint"] != before["fingerprint"], changed
            assert (
                changed["identity"]["engine"] == before["identity"]["engine"]
            ), changed
            assert (
                changed["identity"]["launch_executable"]
                == before["identity"]["launch_executable"]
            )
            assert (
                library_fact(changed)["sha256"]
                == checksum(library)
                != library_fact(before)["sha256"]
            )
            proof["library_replacement"] = {
                "before_fingerprint": before["fingerprint"],
                "after_fingerprint": changed["fingerprint"],
                "unchanged_reported_build_and_wrapper": True,
                "old_generation_keeps_frozen_identity_but_cannot_qualify": True,
                "ordinary_json_sse_serving_continues": True,
                "retained_library_survives_owned_process_retirement": retained.is_file(),
            }
            fixture.stop()
            fixture.start()
            fixture.ok("/v1/load", {"model_name": MODEL})
            restarted = profile()
            assert (
                restarted["configured"]
                and restarted["fingerprint"] == changed["fingerprint"]
            ), restarted
            proof["unchanged_restart_identity"] = True
            fixture.ok("/v1/unload", {})
            fixture.stop()
            with executable.open("ab") as output:
                output.write(b"\ncontrolled-actual-executable-change\n")
            fixture.start()
            fixture.ok("/v1/load", {"model_name": MODEL})
            executable_changed = profile()
            assert (
                executable_changed["configured"]
                and executable_changed["fingerprint"] != changed["fingerprint"]
            ), executable_changed
            assert (
                executable_changed["identity"]["engine"]
                == changed["identity"]["engine"]
            )
            assert executable_changed["identity"]["runtime_image"]["executable"][
                "sha256"
            ] == checksum(executable)
            assert checksum(wrapper) == wrapper_hash
            status, _, reply = fixture.ask("changed-actual-executable", max_tokens=4)
            assert status == 200, reply
            proof[
                "actual_executable_change_invalidates_with_same_wrapper_and_build"
            ] = True
            assert checksum(args.runtime_dir / "llama-server") == source_executable
            assert (
                checksum(args.runtime_dir / "libllama-server-impl.so") == source_library
            )
            assert checksum(args.model_file) == source_model
            proof["original_runtime_and_weights_unchanged"] = True
        finally:
            fixture.stop()
            fixture.log.close()
            args.out.with_suffix(".server.log").write_bytes(
                (root / "server.log").read_bytes()
            )
    args.out.write_text(json.dumps(proof, indent=2) + "\n")
    print(
        "PASS: actual executable/library identity, stale live-runtime rejection, ordinary serving and restart"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "runtime-dir", "model-file", "out"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    for name in ("binary", "runtime_dir", "model_file"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    acceptance(args)


if __name__ == "__main__":
    main()
