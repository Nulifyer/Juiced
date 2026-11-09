#!/usr/bin/env python3
"""Model-free subprocess acceptance for owned terminal shutdown."""

import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request


def launch(binary, root, port, embedded=False):
    for name in ("config", "cache", "run", "watched"):
        (root / name).mkdir(exist_ok=True)
    (root / "config/config.json").write_text(
        json.dumps(
            {
                "offline": True,
                "broadcast": False,
                "disable_model_filtering": True,
                "auto_check_model_updates": False,
                "no_fetch_executables": True,
                "extra_models_dir": str(root / "watched"),
                "log_file": str(root / "manager.log"),
            }
        )
    )
    env = {
        **os.environ,
        "HOME": str(root),
        "XDG_RUNTIME_DIR": str(root / "run"),
        "LEMONADE_API_KEY": "",
        "LEMONADE_ADMIN_API_KEY": "",
    }
    log = (root / "server.log").open("w")
    process = subprocess.Popen(
        [
            str(binary),
            str(root / "cache"),
            str(root / "config"),
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
        ]
        + (["--silent"] if embedded else []),
        stdout=log,
        stderr=log,
        env=env,
    )
    return process, log


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def run_case(binary, mode, embedded=False):
    with tempfile.TemporaryDirectory(prefix="juiced-shutdown-") as temporary:
        root = Path(temporary)
        port = free_port()
        blocker = None
        if mode == "occupied-port":
            blocker = socket.socket()
            blocker.bind(("127.0.0.1", port))
            blocker.listen()
        process, log = launch(binary, root, port, embedded)
        entry_mode = "direct"
        try:
            if not blocker:
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    if process.poll() is not None:
                        raise AssertionError("manager exited before readiness")
                    try:
                        with urllib.request.urlopen(
                            f"http://127.0.0.1:{port}/v1/health", timeout=1
                        ) as response:
                            if response.status == 200:
                                break
                    except OSError:
                        pass
                    time.sleep(0.05)
                else:
                    raise AssertionError("manager did not become ready")
                if embedded:
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        observed = (root / "manager.log").read_text(errors="replace")
                        if "Embedded tray event loop starting" in observed:
                            entry_mode = "event-loop"
                            break
                        if "running headless" in observed:
                            entry_mode = "headless"
                            break
                        if process.poll() is not None:
                            raise AssertionError(
                                "embedded manager exited before its event loop"
                            )
                        time.sleep(0.05)
                    else:
                        raise AssertionError(
                            "embedded manager did not publish its run mode"
                        )
                # Include a native watcher event and pending hardware reload.
                (root / "watched/change.txt").write_text("metadata only")
                if hasattr(signal, "SIGHUP"):
                    process.send_signal(signal.SIGHUP)
                if mode.startswith("/"):
                    request = urllib.request.Request(
                        f"http://127.0.0.1:{port}{mode}",
                        data=b"{}",
                        headers={"Content-Type": "application/json"},
                    )
                    with urllib.request.urlopen(request, timeout=20) as response:
                        assert response.status == 200
                        assert json.load(response)["status"] == "shutting down"
                else:
                    process.send_signal(getattr(signal, mode))
            code = process.wait(timeout=25)
            assert code == (1 if blocker else 0), f"unexpected exit status {code}"
            log.flush()
            text = (root / "server.log").read_text(errors="replace")
            if (root / "manager.log").exists():
                text += (root / "manager.log").read_text(errors="replace")
            assert (
                "Background model activity stopped" in text
            ), "terminal ownership cleanup did not run"
            return {
                "mode": mode,
                "entry_mode": entry_mode,
                "exit_status": code,
                "owned_cleanup": True,
            }
        except Exception as error:
            log.flush()
            detail = (root / "server.log").read_text(errors="replace")
            if (root / "manager.log").exists():
                detail += (root / "manager.log").read_text(errors="replace")
            detail = detail[-6000:]
            raise AssertionError(f"{mode}: {error}\n{detail}") from error
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)
            log.close()
            if blocker:
                blocker.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--embedded", action="store_true")
    args = parser.parse_args()
    modes = ["/internal/shutdown", "occupied-port"]
    # Python's Windows termination is TerminateProcess, which does not deliver
    # the C runtime's handled signal. HTTP and startup failure remain portable.
    if os.name != "nt" and not args.embedded:
        modes.extend(["SIGTERM", "SIGINT"])
    rows = [run_case(args.binary.resolve(), mode, args.embedded) for mode in modes]
    proof = {
        "scope": "Model-free owned shutdown with real native watcher events; no inference or live services",
        "cases": rows,
        "embedded": args.embedded,
    }
    if args.out:
        args.out.write_text(json.dumps(proof, indent=2) + "\n")
    print(f"PASS: {len(rows)} HTTP/signal/startup shutdown cases")


if __name__ == "__main__":
    main()
