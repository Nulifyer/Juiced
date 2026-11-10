#!/usr/bin/env python3
"""Published projector path acquisition through native HTTPS loopback fixtures.

Requires Linux, bwrap, OpenSSL and curl-config. Uses synthetic artifact bytes;
does not load a model or download from an external registry.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import ssl
import subprocess
import tempfile
import threading
import time
from types import SimpleNamespace
from urllib.parse import unquote, urlsplit

from test_library_journey import (
    NamespacedFixture,
    RegistryRequest,
    Repository,
    digest,
    stop_owned_manager,
)

MAIN = "Model-Q4_K_M.gguf"
BASENAME = "mmproj-BF16.gguf"


def catalog_model(fixture, model, downloaded):
    until = time.monotonic() + 10
    while True:
        rows = fixture.ok("/v1/juiced/catalog")["data"]
        row = next((m for m in rows if m["id"] == model.removeprefix("user.")), None)
        if row is not None and row["downloaded"] == downloaded:
            return row
        assert time.monotonic() < until, (model, row, [m["id"] for m in rows])
        time.sleep(0.05)


class ProjectorRequest(RegistryRequest):
    def do_GET(self):
        path = unquote(urlsplit(self.path).path)
        self.server.observations.append({"method": self.command, "path": path})
        repo = self.server.repo
        files = self.server.files
        if path.startswith("/api/models/" + repo):
            return self.json(
                200,
                {
                    "id": repo,
                    "sha": self.server.revision,
                    "pipeline_tag": "text-generation",
                    "gguf": {"architecture": "llama"},
                    "siblings": [
                        {
                            "rfilename": name,
                            "size": len(data),
                            "lfs": {
                                "oid": hashlib.sha256(data).hexdigest(),
                                "size": len(data),
                            },
                        }
                        for name, data in files.items()
                    ],
                },
            )
        prefix = f"/{repo}/resolve/{self.server.revision}/"
        name = path.removeprefix(prefix)
        if not path.startswith(prefix) or name not in files:
            return self.json(404, {"error": "No such fixture artifact"})
        self.send(
            200,
            files[name],
            "application/octet-stream",
            {
                "X-Repo-Commit": self.server.revision,
                "ETag": '"' + hashlib.sha256(files[name]).hexdigest() + '"',
            },
        )


def acceptance(args):
    args.out.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="juiced-projector-acquisition-"))
    synthetic = root / "fixture.gguf"
    synthetic.write_bytes(b"synthetic local registration, never loaded\n")
    fixture_args = SimpleNamespace(
        binary=args.binary, engine=Path("/bin/false"), model_file=synthetic
    )
    fixture = NamespacedFixture(fixture_args, root)
    config_path = root / "config/config.json"
    config = json.loads(config_path.read_text())
    config.update({"no_fetch_executables": True, "default_model_source": "modelscope"})
    config_path.write_text(json.dumps(config))
    environment = {
        key: os.environ[key]
        for key in ("PATH", "LANG", "LC_ALL", "TMPDIR")
        if key in os.environ
    }
    environment["HOME"] = str(root)
    certificate, key = root / "fixture-ca.pem", root / "fixture-key.pem"
    subprocess.run(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=Juiced Projector Fixture",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-keyout",
            str(key),
            "-out",
            str(certificate),
        ],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    system_ca = Path(
        subprocess.check_output(["curl-config", "--ca"], text=True).strip()
    ).resolve(strict=True)
    bundle = root / "fixture-trust.pem"
    bundle.write_bytes(system_ca.read_bytes() + b"\n" + certificate.read_bytes())
    wrapper = root / "native-with-private-trust"
    command = [
        os.environ.get("JUICED_TEST_BWRAP", "bwrap"),
        "--die-with-parent",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--bind",
        str(root),
        str(root),
        "--ro-bind",
        str(bundle),
        str(system_ca),
        "--",
        str(args.binary),
    ]
    wrapper.write_text("#!/bin/sh\nexec " + shlex.join(command) + ' "$@"\n')
    wrapper.chmod(0o755)
    fixture.args = SimpleNamespace(**{**vars(fixture_args), "binary": wrapper})
    registry = Repository(synthetic, fixture, environment)
    registry.RequestHandlerClass = ProjectorRequest
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(certificate, key)
    registry.socket = tls.wrap_socket(registry.socket, server_side=True)
    endpoint = f"https://127.0.0.1:{registry.server_port}"
    environment.update({"HF_ENDPOINT": endpoint, "MODELSCOPE_ENDPOINT": endpoint})
    thread = threading.Thread(target=registry.serve_forever, daemon=True)
    thread.start()
    proof = {
        "schema": 1,
        "passed": False,
        "scope": "Synthetic native Hugging Face acquisition, exact companion paths and selected source. No external download, inference, publisher availability or ModelScope qualification.",
        "binary_sha256": digest(args.binary),
        "cases": [],
    }
    try:
        registry.restart()
        for index, companions in enumerate(
            (
                ["vision/" + BASENAME],
                [BASENAME, "vision-a/" + BASENAME, "vision-b/" + BASENAME],
                [BASENAME, "vision/" + BASENAME],
            )
        ):
            repo = registry.repo = f"Fixture/Projector-{index}-GGUF"
            registry.files = {
                MAIN: b"GGUF\x03\x00\x00\x00" + b"synthetic main bytes\n",
                **{
                    name: b"GGUF\x03\x00\x00\x00"
                    + ("synthetic selected companion " + name + "\n").encode()
                    for name in companions
                },
            }
            variants = fixture.ok(
                "/v1/pull/variants?checkpoint=" + repo + "&source=huggingface"
            )
            assert variants["mmproj_files"] == sorted(companions), variants
            assert len(variants["variants"]) == 1, variants
            selected = variants["mmproj_files"][-1]
            model = f"user.Projector-Fixture-{index}"
            body = {
                "model": model,
                "checkpoints": {
                    "main": repo + ":" + variants["variants"][0]["name"],
                    "mmproj": repo + ":" + selected,
                },
                "recipe": "llamacpp",
                "source": "huggingface",
                "labels": ["chat", "vision"],
            }
            request_start = len(registry.observations)
            if index == 2:
                del registry.files[selected]
                status, _, response = fixture.call("/v1/pull", body)
                expected = (
                    "Additional checkpoint file not found on Hugging Face: "
                    + repo
                    + ":"
                    + selected
                )
                assert status == 500 and expected in response["error"], response
                assert not any(
                    "/resolve/" in request["path"]
                    for request in registry.observations[request_start:]
                ), registry.observations[request_start:]
                row = catalog_model(fixture, model, False)
                assert not row["downloaded"], row
                proof["cases"].append(
                    {"selected": selected, "missing_rejected": True, "error": expected}
                )
                continue
            fixture.ok("/v1/pull", body)
            row = catalog_model(fixture, model, True)
            assert row["downloaded"] and not row["loaded"], row
            assert row["registry_source"] == "huggingface", row
            registration = json.loads((root / "config/user_models.json").read_text())[
                model.removeprefix("user.")
            ]
            assert registration["source"] == "huggingface", registration
            assert registration["checkpoints"]["mmproj"] == repo + ":" + selected
            cached = list((root / "hf").rglob(selected))
            assert len(cached) == 1, cached
            assert cached[0].read_bytes() == registry.files[selected], cached
            assert (
                cached[0]
                .relative_to(root / "hf")
                .as_posix()
                .endswith("/snapshots/" + registry.revision + "/" + selected)
            ), cached
            snapshot = cached[0].parents[len(Path(selected).parts) - 1]
            for other in set(companions) - {selected}:
                assert not (snapshot / other).exists(), other
            resolves = [
                request["path"]
                for request in registry.observations[request_start:]
                if request["method"] == "GET" and "/resolve/" in request["path"]
            ]
            assert set(resolves) == {
                f"/{repo}/resolve/{registry.revision}/{name}"
                for name in (MAIN, selected)
            }, resolves
            proof["cases"].append(
                {
                    "selected": selected,
                    "source": row["registry_source"],
                    "companion_sha256": digest(cached[0]),
                    "cache_path": cached[0].relative_to(root / "hf").as_posix(),
                    "exact_download_paths": resolves,
                }
            )
        proof["passed"] = True
    finally:
        errors = []
        for action in (
            lambda: stop_owned_manager(registry),
            fixture.log.close,
            registry.shutdown,
            registry.server_close,
            lambda: thread.join(timeout=2),
        ):
            try:
                action()
            except Exception as error:
                errors.append(str(error))
        proof["cleanup_errors"] = errors
        proof["cleanup_complete"] = not errors and fixture.process is None
        proof["binary_preserved"] = digest(args.binary) == proof["binary_sha256"]
        proof["passed"] &= proof["cleanup_complete"] and proof["binary_preserved"]
        proof["registry_requests"] = registry.observations
        shutil.copyfile(root / "server.log", args.out / "server.log")
        if proof["cleanup_complete"]:
            shutil.rmtree(root)
        else:
            proof["retained_fixture"] = str(root)
        (args.out / "receipt.json").write_text(json.dumps(proof, indent=2) + "\n")
    assert proof["passed"], "Acquisition or cleanup failed; inspect receipt.json"
    return proof


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    arguments = parser.parse_args()
    arguments.binary = arguments.binary.resolve(strict=True)
    arguments.out = arguments.out.resolve()
    print(json.dumps(acceptance(arguments), indent=2))
