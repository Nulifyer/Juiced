"""Discover Juiced speech deployments and launch the maintained Wyoming server."""

import argparse
import asyncio
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


def voices(base_url, models):
    key = os.environ.get("TTS_OPENAI_KEY", "")
    headers = {"Authorization": "Bearer " + key} if key else {}
    for _ in range(120):
        found = set()
        try:
            for model in models:
                url = (
                    base_url
                    + "/audio/voices?"
                    + urllib.parse.urlencode({"model": model})
                )
                request = urllib.request.Request(url, headers=headers)
                with urllib.request.urlopen(request, timeout=5) as response:
                    data = json.load(response)
                found.update(data["voices"])
            return sorted(
                voice
                for voice in found
                if voice.startswith(("af_", "am_", "bf_", "bm_"))
            )
        except (OSError, ValueError, KeyError):
            time.sleep(1)
    raise RuntimeError("Speech voice discovery did not become ready")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", required=True)
    parser.add_argument("--uri", required=True)
    parser.add_argument("--models", required=True)
    args = parser.parse_args()
    models = json.loads(args.models)
    argv = [
        "juiced-wyoming",
        "--uri",
        args.uri,
        "--languages",
        "en",
        "--log-level",
        "WARNING",
    ]
    if models["stt"]:
        argv += [
            "--stt-backend",
            "OPENAI",
            "--stt-openai-url",
            args.url,
            "--stt-models",
            *models["stt"],
        ]
    if models["tts"]:
        argv += [
            "--tts-backend",
            "OPENAI",
            "--tts-openai-url",
            args.url,
            "--tts-extra-body",
            '{"response_format":"pcm"}',
            "--tts-models",
            *models["tts"],
            "--tts-voices",
            *voices(args.url, models["tts"]),
        ]
    sys.argv = argv
    from wyoming_openai.__main__ import main as upstream_main

    asyncio.run(upstream_main())


if __name__ == "__main__":
    main()
