#!/usr/bin/env python3
"""Cancellation must reap converter descendants and remove staging files."""

import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument("--python", required=True)
parser.add_argument("--script", required=True)
parser.add_argument("--source", required=True)
args = parser.parse_args()
with tempfile.TemporaryDirectory(prefix="juiced-cancel-worker-") as temporary:
    root = Path(temporary)
    job_dir = root / "job"
    job_dir.mkdir()
    quantizer = root / "slow-quantizer"
    quantizer.write_text(
        '#!/bin/sh\necho $$ > "' + str(root / "child.pid") + '"\nsleep 60\n'
    )
    quantizer.chmod(0o755)
    (job_dir / "job.json").write_text(
        json.dumps(
            {
                "source": args.source,
                "cache": str(root / "cache"),
                "quant": "Q8_0",
                "quantizer": str(quantizer),
                "changes": [],
            }
        )
    )
    process = subprocess.Popen(
        [args.python, args.script, str(job_dir / "job.json")],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        for _ in range(300):
            if (root / "child.pid").exists():
                break
            if process.poll() is not None:
                raise AssertionError("Worker exited before the converter started")
            time.sleep(0.1)
        assert (root / "child.pid").exists()
        child = int((root / "child.pid").read_text())
        process.terminate()
        process.wait(timeout=8)
        assert process.returncode != 0
        try:
            os.kill(child, 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError("Converter survived cancellation")
        assert not list((root / "cache").glob(".partial-*"))
        assert not (job_dir / "result.json").exists()
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
print("PASS: cancellation terminates a running converter and removes partial output")
