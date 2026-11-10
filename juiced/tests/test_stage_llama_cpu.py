#!/usr/bin/env python3
"""CPU-runtime staging preserves inputs, native arguments and accelerator separation."""

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

MODULE = Path(__file__).resolve().parents[1] / "runtime/stage_llama_cpu.py"
spec = importlib.util.spec_from_file_location("stage_llama_cpu", MODULE)
stager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stager)


class CpuRuntimeStaging(unittest.TestCase):
    def test_separate_cpu_payload_and_argument_preservation(self):
        with tempfile.TemporaryDirectory(prefix="juiced-cpu-staging-") as directory:
            root = Path(directory)
            source, output = root / "published", root / "cpu runtime's files"
            (source / "bin").mkdir(parents=True)
            (source / "licenses").mkdir()
            launcher = source / "bin/llama-server"
            launcher.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            launcher.chmod(0o755)
            for name in (
                "ggml-rpc-server",
                "libllama-server-impl.so",
                "libggml-cpu-x64.so",
                "libggml-base.so.1",
                "libggml.so.1",
                "libggml-vulkan.so",
                "libggml-rpc.so",
            ):
                (source / "bin" / name).write_text(name)
            (source / "bin/libggml-base.so").symlink_to("libggml-base.so.1")
            (source / "licenses/LICENSE").write_text("Retain published-runtime notices")
            (source / "source-revision").write_text(
                "controlled published-runtime fixture"
            )
            before = {
                str(p.relative_to(source)): hashlib.sha256(p.read_bytes()).hexdigest()
                for p in source.rglob("*")
                if p.is_file()
            }
            manifest = stager.stage(source, output, output)
            self.assertEqual(
                manifest["excluded_runtime_files"],
                ["ggml-rpc-server", "libggml-rpc.so", "libggml-vulkan.so"],
            )
            self.assertFalse((output / "bin/libggml-vulkan.so").exists())
            self.assertTrue((output / "bin/libggml-base.so").is_symlink())
            expected = {
                str(p.relative_to(output)): hashlib.sha256(p.read_bytes()).hexdigest()
                for p in output.rglob("*")
                if p.is_file() and p.name != "variant.json"
            }
            self.assertEqual(manifest["files"], expected)
            self.assertEqual(
                json.loads((output / "variant.json").read_text()), manifest
            )
            self.assertEqual(
                before,
                {
                    str(p.relative_to(source)): hashlib.sha256(
                        p.read_bytes()
                    ).hexdigest()
                    for p in source.rglob("*")
                    if p.is_file()
                },
            )
            arguments = [
                "value with spaces",
                "literal$(value)",
                "quote'and\"double",
                "",
            ]
            result = subprocess.run(
                [str(output / "llama-server"), *arguments],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.stdout.splitlines(), arguments)
            second = root / "second runtime"
            repeated = stager.stage(output, second, second)
            self.assertNotIn("variant.json", repeated["files"])
            self.assertEqual(
                repeated["files"],
                {
                    str(p.relative_to(second)): hashlib.sha256(
                        p.read_bytes()
                    ).hexdigest()
                    for p in second.rglob("*")
                    if p.is_file() and p != second / "variant.json"
                },
            )
            with self.assertRaises(FileExistsError):
                stager.stage(source, output, output)
            with self.assertRaises(ValueError):
                stager.stage(source, source / "nested", root / "elsewhere")
            with self.assertRaises(ValueError):
                stager.stage(source, root / "relative-test", Path("relative"))

    def test_incomplete_input_never_creates_a_destination(self):
        with tempfile.TemporaryDirectory(prefix="juiced-cpu-incomplete-") as directory:
            root = Path(directory)
            source = root / "incomplete"
            source.mkdir()
            output = root / "output"
            with self.assertRaises(ValueError):
                stager.stage(source, output, output)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
