#!/usr/bin/env python3
"""Real HF converter fixture; reject hidden modality loss and preserve text tensors."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument("--script", required=True)
parser.add_argument("--converter", required=True)
parser.add_argument("--quantizer", required=True)
parser.add_argument("--out", required=True)
args = parser.parse_args()
revision = "9fb191250dd56d0ba7ec9785a025ed29c03d5998"
repository = "hf-internal-testing/tiny-random-LlamaForCausalLM"
with tempfile.TemporaryDirectory(prefix="juiced-hf-preparation-") as temporary:
    root = Path(temporary)
    source = root / "source"
    source.mkdir()
    for name in [
        "config.json",
        "generation_config.json",
        "model.safetensors",
        "special_tokens_map.json",
        "tokenizer.json",
        "tokenizer.model",
        "tokenizer_config.json",
    ]:
        urllib.request.urlretrieve(
            f"https://huggingface.co/{repository}/resolve/{revision}/{name}",
            source / name,
        )
    original = hashlib.sha256((source / "model.safetensors").read_bytes()).hexdigest()
    config = json.loads((source / "config.json").read_text())
    assert config["pad_token_id"] == -1
    config["pad_token_id"] = None
    (source / "config.json").write_text(json.dumps(config))
    import sys

    def run(name, text_only=False):
        job = root / name
        job.mkdir()
        (job / "job.json").write_text(
            json.dumps(
                {
                    "source": str(source),
                    "cache": str(root / "cache"),
                    "quant": "Q4_K_M",
                    "quantizer": args.quantizer,
                    "converter": args.converter,
                    "changes": ["F32 through F16 to Q4_K_M"]
                    + (["Vision/audio omitted: text-only output"] if text_only else []),
                    "text_only": text_only,
                }
            )
        )
        process = subprocess.run(
            [sys.executable, args.script, str(job / "job.json")],
            capture_output=True,
            text=True,
            timeout=120,
        )
        return process, json.loads((job / "progress.json").read_text()), job

    process, status, job = run("original-text")
    assert process.returncode == 0, (status, (job / "output.log").read_text())
    result = json.loads((job / "result.json").read_text())
    assert result["manifest"]["tensor_count"] == 21
    assert (
        hashlib.sha256((source / "model.safetensors").read_bytes()).hexdigest()
        == original
    )
    import numpy as np
    from safetensors.numpy import load_file, save_file

    tensors = load_file(source / "model.safetensors")
    tensors["visual.layer.weight"] = np.zeros((16, 16), dtype=np.float32)
    save_file(tensors, source / "model.safetensors")
    config["vision_config"] = {}
    (source / "config.json").write_text(json.dumps(config))
    process, status, job = run("unreviewed-vision")
    assert (
        process.returncode != 0 and "unreviewed multimodal" in status["error"]
    ), status
    assert not (job / "result.json").exists()
    process, status, job = run("reviewed-text-only", True)
    assert process.returncode == 0, (status, (job / "output.log").read_text())
    text_result = json.loads((job / "result.json").read_text())
    assert text_result["manifest"]["tensor_count"] == 21
    assert text_result["manifest"]["recipe"]["text_only"]
    proof = {
        "repository": repository,
        "revision": revision,
        "fixture_metadata_change": "Legacy negative pad_token_id unset in the isolated fixture",
        "tensor_count": 21,
        "original_weight_sha256": original,
        "source_unchanged_by_conversion": True,
        "unreviewed_modality_loss_rejected": True,
        "explicit_text_only_conversion": True,
        "inference_quality_tested": False,
    }
    Path(args.out).write_text(json.dumps(proof, indent=2) + "\n")
print(
    "PASS: actual HF conversion, preserved source and explicit text-only modality loss"
)
