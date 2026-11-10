#!/usr/bin/env python3
"""Loaded-input identity across registration, content changes and manager restart."""

import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

from test_cache_admission import Fixture, MODEL


def acceptance(args):
    proof = {
        "scope": "isolated CPU loaded-input and cache compatibility; not full runtime/device identity"
    }
    with tempfile.TemporaryDirectory(prefix="juiced-execution-identity-") as directory:
        root = Path(directory)
        original = root / "original.gguf"
        replacement = root / "replacement.gguf"
        shutil.copyfile(args.model_file, original)
        shutil.copyfile(args.model_file, replacement)
        source = args.model_file
        engine_source = args.engine
        engine_checksum = hashlib.sha256(engine_source.read_bytes()).hexdigest()
        private_engine = root / "private-engine"
        private_engine.write_text(
            "#!/usr/bin/env python3\nimport os,sys\n"
            f"os.execv({str(engine_source)!r}, [{str(engine_source)!r}, *sys.argv[1:]])\n"
        )
        private_engine.chmod(0o700)
        args.engine = private_engine
        args.model_file = original
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
                    str(original),
                    "--snapshots",
                    str(root / "snapshots"),
                    "--out",
                    str(root / "base.json"),
                ],
                check=True,
            )
            fixture.ok("/v1/juiced/cache", {"mode": "persistent"})

            def profile():
                return fixture.ok("/v1/juiced/cache/profile", {"model": MODEL})

            before = profile()
            assert (
                before["qualified"] and before["identity"]["profile_schema"] == 4
            ), before
            expected = hashlib.sha256(original.read_bytes()).hexdigest()
            assert before["identity"]["files"]["main"][0]["sha256"] == expected
            status, _, response = fixture.ask("definition-change")
            assert (
                status == 200 and response["choices"][0]["message"]["content"]
            ), response
            definition = {
                "model_name": "user." + MODEL,
                "recipe": "llamacpp",
                "source": "local_path",
                "checkpoints": {"main": str(replacement)},
                "labels": ["chat"],
            }
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                reading = pool.submit(lambda: [profile() for _ in range(10)])
                fixture.ok("/v1/models/register", definition)
                observations = reading.result(timeout=30)
            after = profile()
            assert all(
                p["fingerprint"] == before["fingerprint"]
                and p["worker_generation"] == before["worker_generation"]
                for p in observations + [after]
            )
            assert after["identity"]["files"]["main"][0]["path"] == str(original)
            status, headers, response = fixture.ask("definition-change")
            assert status == 200 and headers.get("X-Juiced-Cache") == "restored", (
                status,
                headers,
                response,
            )
            assert response["timings"]["cache_n"] > 30, response
            proof["registration_overlap"] = {
                "coherent_profiles": len(observations) + 1,
                "loaded_generation": before["worker_generation"],
                "old_generation_keeps_actual_files_and_restores": True,
            }
            fixture.ok("/v1/unload", {})
            fixture.ok("/v1/load", {"model_name": MODEL})
            replaced = profile()
            assert replaced["worker_generation"] != before["worker_generation"]
            assert (
                replaced["fingerprint"] != before["fingerprint"]
                and not replaced["qualified"]
            )
            assert replaced["identity"]["files"]["main"][0]["path"] == str(replacement)
            assert replaced["identity"]["files"]["main"][0]["sha256"] == expected
            status, headers, _ = fixture.ask("definition-change")
            assert (
                status == 200 and headers.get("X-Juiced-Cache") == "unqualified-profile"
            ), headers
            proof["replacement_cold_until_requalification"] = True
            fixture.stop()
            fixture.start()
            fixture.ok("/v1/load", {"model_name": MODEL})
            restarted = profile()
            assert restarted["fingerprint"] == replaced["fingerprint"]
            proof["unchanged_manager_restart_identity"] = True
            fixture.ok("/v1/unload", {})
            info = replacement.stat()
            with replacement.open("r+b") as file:
                file.seek(-1, os.SEEK_END)
                byte = file.read(1)
                file.seek(-1, os.SEEK_END)
                file.write(bytes([byte[0] ^ 1]))
            os.utime(replacement, ns=(info.st_atime_ns, info.st_mtime_ns))
            assert (
                replacement.stat().st_size == info.st_size
                and replacement.stat().st_mtime_ns == info.st_mtime_ns
            )
            fixture.ok("/v1/load", {"model_name": MODEL})
            changed = profile()
            assert changed["fingerprint"] != replaced["fingerprint"]
            assert changed["identity"]["files"]["main"][0]["sha256"] != expected
            assert not changed["qualified"]
            proof["same_size_mtime_content_change_invalidates"] = True
            executable_hash = changed["identity"]["launch_executable"]["sha256"]
            private_engine.write_bytes(
                private_engine.read_bytes() + b"\n# identity fixture revision\n"
            )
            assert profile()["fingerprint"] == changed["fingerprint"]
            fixture.ok("/v1/unload", {})
            fixture.ok("/v1/load", {"model_name": MODEL})
            executable_changed = profile()
            assert (
                executable_changed["identity"]["engine"]
                == changed["identity"]["engine"]
            )
            assert (
                executable_changed["identity"]["launch_executable"]["sha256"]
                != executable_hash
            )
            assert executable_changed["fingerprint"] != changed["fingerprint"]
            proof["executable_change_with_same_reported_build_invalidates"] = True
            fixture.ok("/v1/unload", {})
            private_engine.write_text(
                "#!/usr/bin/env python3\n"
                "import os,sys\n"
                f"with open({str(replacement)!r}, 'r+b') as file:\n"
                " file.seek(-1, os.SEEK_END); value=file.read(1); file.seek(-1, os.SEEK_END); file.write(bytes([value[0] ^ 1]))\n"
                f"os.execv({str(engine_source)!r}, [{str(engine_source)!r}, *sys.argv[1:]])\n"
            )
            status, _, rejected = fixture.call("/v1/load", {"model_name": MODEL})
            assert status == 409 and "execution_inputs_changed" in json.dumps(
                rejected
            ), (status, rejected)
            assert not any(
                row["model_name"] == MODEL
                for row in fixture.ok("/v1/health")["all_models_loaded"]
            )
            proof["startup_content_change_rejected_without_publication"] = True
            proof["engine_source_unchanged"] = (
                hashlib.sha256(engine_source.read_bytes()).hexdigest()
                == engine_checksum
            )
            proof["source_unchanged"] = (
                hashlib.sha256(source.read_bytes()).hexdigest() == expected
            )
            proof["identity_scope"] = changed["identity"]["scope"]
            proof["unresolved_identity"] = changed["identity"]["unresolved"]
        finally:
            fixture.stop()
            fixture.log.close()
            args.out.with_suffix(".server.log").write_bytes(
                (root / "server.log").read_bytes()
            )
    args.out.write_text(json.dumps(proof, indent=2) + "\n")
    print(
        "PASS: frozen loaded inputs, registration overlap, content change and restart"
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
