"""Offline model preparation, supervised by the native Juiced lifecycle owner."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time

active_process = None


def interrupt(signum, _frame):
    if active_process is not None and active_process.poll() is None:
        if os.name == "posix":
            os.killpg(active_process.pid, signal.SIGTERM)
        else:
            active_process.terminate()
        try:
            active_process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            if os.name == "posix":
                os.killpg(active_process.pid, signal.SIGKILL)
            else:
                active_process.kill()
            active_process.wait()
    raise InterruptedError("Preparation interrupted")


TOOL_REVISION = "llama.cpp-669204ee2b9004845b34f26466a91d003edc98aa"


def atomic(path, value):
    temp = path.with_suffix(".tmp")
    temp.write_text(json.dumps(value, indent=2))
    temp.replace(path)


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(8 * 1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def tensor_inventory(path):
    import gguf

    reader = gguf.GGUFReader(str(path), "r")
    result = {tensor.name: [int(n) for n in tensor.shape] for tensor in reader.tensors}
    if not result:
        raise ValueError("Prepared GGUF has no tensors")
    for tensor in reader.tensors:
        if int(tensor.data_offset) + int(tensor.n_bytes) > path.stat().st_size:
            raise ValueError("Prepared GGUF contains truncated tensor data")
    return result


def prepare(job_path):
    global active_process
    job = json.loads(job_path.read_text())
    status_path = job_path.with_name("progress.json")
    result_path = job_path.with_name("result.json")
    source = Path(job["source"])
    cache = Path(job["cache"])
    cache.mkdir(parents=True, exist_ok=True)
    stage = cache / (".partial-" + job_path.parent.name)
    stage.mkdir(exist_ok=False)

    def status(phase, **details):
        atomic(status_path, {"phase": phase, **details})

    try:
        status("hashing")
        if source.is_dir():
            files = sorted(
                p
                for p in source.iterdir()
                if p.is_file()
                and (
                    p.suffix
                    in {
                        ".json",
                        ".safetensors",
                        ".model",
                        ".tiktoken",
                        ".txt",
                        ".jinja",
                    }
                )
            )
            if not (source / "config.json").is_file() or not any(
                p.suffix == ".safetensors" for p in files
            ):
                raise ValueError(
                    "A local Hugging Face snapshot with config, tokenizer and safetensors is required"
                )
        else:
            files = [source]
        source_mtp = False
        if source.is_dir():
            from safetensors import safe_open

            tensor_names = []
            for file in files:
                if file.suffix == ".safetensors":
                    with safe_open(file, framework="np") as tensors:
                        tensor_names.extend(tensors.keys())
            source_mtp = any(
                ".mtp." in name or name.startswith("mtp.") or "nextn" in name
                for name in tensor_names
            )
            multimodal = any(
                any(
                    part in name
                    for part in (
                        "visual.",
                        "vision.",
                        "vision_",
                        "audio.",
                        "audio_",
                        "vpm.",
                        "vit.",
                        "projector.",
                        "talker.",
                    )
                )
                for name in tensor_names
            )
            if multimodal and not job.get("text_only", False):
                raise ValueError(
                    "The snapshot contains unreviewed multimodal tensors; review a text-only preparation plan or use a prepared publisher bundle"
                )
        companions = {
            role: Path(path) for role, path in job.get("companions", {}).items() if path
        }
        hashes = {str(p): digest(p) for p in files + list(companions.values())}
        tool_files = {"prepare.py": digest(Path(__file__))}
        native_directory = Path(job["quantizer"]).parent / "native"
        if native_directory.is_dir():
            for file in sorted(native_directory.iterdir()):
                if file.is_file() and (
                    ".so" in file.name or file.name == "llama-quantize"
                ):
                    tool_files["native/" + file.name] = digest(file)
        converter = Path(job.get("converter", ""))
        if converter.is_file():
            for file in sorted(converter.parent.rglob("*.py")):
                tool_files["source/" + str(file.relative_to(converter.parent))] = (
                    digest(file)
                )
        lock = Path(__file__).parent / "requirements.lock"
        if lock.is_file():
            tool_files["requirements.lock"] = digest(lock)
        content_hashes = {
            str(file.relative_to(source)) if source.is_dir() else "main": hashes[
                str(file)
            ]
            for file in files
        }
        content_hashes.update(
            {
                "companion/" + role: hashes[str(file)]
                for role, file in companions.items()
            }
        )
        receipt = {
            "schema": 1,
            "tool": TOOL_REVISION,
            "quant": job["quant"],
            "source_sha256": content_hashes,
            "changes": job["changes"],
            "text_only": job.get("text_only", False),
            "quantizer_sha256": digest(Path(job["quantizer"])),
            "tool_files_sha256": tool_files,
        }
        key = hashlib.sha256(json.dumps(receipt, sort_keys=True).encode()).hexdigest()
        destination = cache / key
        manifest_path = destination / "manifest.json"
        if manifest_path.is_file():
            manifest = json.loads(manifest_path.read_text())
            output = destination / "model.gguf"
            if (
                manifest.get("recipe") == receipt
                and output.is_file()
                and digest(output) == manifest.get("output_sha256")
            ):
                tensor_inventory(output)
                atomic(
                    result_path,
                    {
                        "path": str(output),
                        "cache_key": key,
                        "cached": True,
                        "manifest": manifest,
                        "companions": {k: str(v) for k, v in companions.items()},
                    },
                )
                status("complete", cached=True)
                return
            destination.rename(cache / (key + ".invalid-" + str(time.time_ns())))
        total = sum(p.stat().st_size for p in files)
        required = total * (4 if source.is_dir() else 2) + 512 * 1024**2
        if shutil.disk_usage(cache).free < required:
            raise ValueError(
                f"Preparation needs at least {required} free bytes for staging and output"
            )
        output = stage / "model.gguf"
        quantizer = job["quantizer"]
        source_tensors = tensor_inventory(source) if source.is_file() else None
        commands = []
        if source.is_dir():
            intermediate = stage / "intermediate.gguf"
            commands.append(
                (
                    "converting",
                    [
                        sys.executable,
                        job["converter"],
                        str(source),
                        "--outfile",
                        str(intermediate),
                        "--outtype",
                        "f16",
                    ],
                )
            )
            quant_input = intermediate
        else:
            quant_input = source
        if job["quant"] == "F16" and source.is_dir():
            pass
        else:
            commands.append(
                (
                    "quantizing",
                    [
                        quantizer,
                        "--allow-requantize",
                        str(quant_input),
                        str(output),
                        job["quant"],
                        str(job.get("threads", 4)),
                    ],
                )
            )
        log_path = job_path.with_name("output.log")
        log_bytes = 0
        skipped = []
        with log_path.open("w") as log:
            for phase, command in commands:
                status(phase)
                with subprocess.Popen(
                    command,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    text=True,
                    start_new_session=os.name == "posix",
                    env={
                        **os.environ,
                        "HF_HUB_OFFLINE": "1",
                        "TRANSFORMERS_OFFLINE": "1",
                        "OMP_NUM_THREADS": str(job.get("threads", 4)),
                    },
                ) as process:
                    active_process = process
                    for line in process.stdout:
                        if (
                            "skipping tensor" in line.lower()
                            or "unmapped tensor" in line.lower()
                        ):
                            skipped.append(line.strip()[:500])
                        if log_bytes < 2 * 1024 * 1024:
                            log.write(line)
                            log.flush()
                            log_bytes += len(line.encode())
                    if process.wait() != 0:
                        raise ValueError(
                            f"{phase.capitalize()} failed; review preparation output"
                        )
                    active_process = None
        if job["quant"] == "F16" and source.is_dir():
            intermediate.replace(output)
        if skipped:
            raise ValueError(
                "Converter omitted tensors; this route is refused: "
                + "; ".join(skipped[:3])
            )
        status("validating")
        tensors = tensor_inventory(output)
        if source_mtp:
            import gguf

            reader = gguf.GGUFReader(str(output), "r")
            if not any(
                key.endswith(".nextn_predict_layers") and int(field.contents()) > 0
                for key, field in reader.fields.items()
            ):
                raise ValueError(
                    "The converter omitted the source MTP feature; this route is refused"
                )
        if source_tensors is not None and tensors != source_tensors:
            raise ValueError(
                "Prepared tensor names or shapes differ from the source; no model was registered"
            )
        # Catch source changes during a long conversion, including companion files.
        if any(digest(Path(path)) != value for path, value in hashes.items()):
            raise ValueError(
                "Source files changed during preparation; no model was registered"
            )
        manifest = {
            "recipe": receipt,
            "output_sha256": digest(output),
            "tensor_count": len(tensors),
            "source_files": hashes,
            "validated": "tensor inventory and complete data ranges",
            "inference_tested": False,
        }
        for path in stage.iterdir():
            if path.name != "model.gguf":
                path.unlink()
        atomic(stage / "manifest.json", manifest)
        stage.rename(destination)
        atomic(
            result_path,
            {
                "path": str(destination / "model.gguf"),
                "cache_key": key,
                "cached": False,
                "manifest": manifest,
                "companions": {k: str(v) for k, v in companions.items()},
            },
        )
        status("complete", cached=False)
    except Exception as error:
        atomic(status_path, {"phase": "failed", "error": str(error)})
        raise
    finally:
        shutil.rmtree(stage, ignore_errors=True)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupt)
    signal.signal(signal.SIGINT, interrupt)
    prepare(Path(sys.argv[1]))
