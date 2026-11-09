#!/usr/bin/env python3
"""Check links and example syntax in the active Juiced documentation."""

import json
import re
import shlex
import shutil
import subprocess
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[2]
REPO_URL = "https://github.com/Nulifyer/Juiced/blob/main/"
FENCE = re.compile(r"^```([^\n]*)\n(.*?)^```\s*$", re.MULTILINE | re.DOTALL)
LINK = re.compile(r"\]\(([^\s)]+)\)|\b(?:src|href)=\"([^\"]+)\"")


def headings(path):
    text = FENCE.sub("", path.read_text())
    counts = {}
    result = set()
    for title in re.findall(r"^#{1,6}\s+(.+)$", text, re.MULTILINE):
        slug = re.sub(r"[^\w\- ]", "", title.lower()).replace(" ", "-")
        count = counts.get(slug, 0)
        result.add(f"{slug}-{count}" if count else slug)
        counts[slug] = count + 1
    return result


def main():
    paths = [
        ROOT / name
        for name in (
            "README.md",
            "CONTRIBUTING.md",
            "src/cpp/README.md",
            "juiced/README.md",
            "juiced/BACKENDS.md",
            "juiced/MCP-TOOLS.md",
        )
    ] + sorted((ROOT / "docs").rglob("*.md"))
    errors = []
    links = examples = shell_examples = 0
    for path in paths:
        text = path.read_text()
        for match in FENCE.finditer(text):
            language, body = match.groups()
            if language == "bash":
                if shutil.which("bash"):
                    result = subprocess.run(
                        ["bash", "-n"],
                        input=body,
                        text=True,
                        capture_output=True,
                        check=False,
                    )
                    shell_examples += 1
                    if result.returncode:
                        errors.append(
                            f"{path.relative_to(ROOT)}: {result.stderr.strip()}"
                        )
                tokens = shlex.split(body)
                for index, token in enumerate(tokens[:-1]):
                    if token in ("-d", "--data", "--data-raw"):
                        payload = tokens[index + 1]
                        if payload.startswith(("{", "[")):
                            try:
                                json.loads(payload)
                            except ValueError as error:
                                errors.append(
                                    f"{path.relative_to(ROOT)}: invalid request JSON: {error}"
                                )
                            examples += 1
            if language == "json":
                try:
                    json.loads(body)
                except ValueError as error:
                    errors.append(f"{path.relative_to(ROOT)}: invalid JSON: {error}")
                examples += 1
        for match in LINK.finditer(FENCE.sub("", text)):
            target = next(value for value in match.groups() if value is not None)
            if target.startswith(REPO_URL):
                target = "/" + target[len(REPO_URL) :]
            parsed = urlsplit(target)
            if parsed.scheme or parsed.netloc:
                continue
            if parsed.path.startswith("/"):
                destination = ROOT / unquote(parsed.path.lstrip("/"))
            elif parsed.path:
                destination = path.parent / unquote(parsed.path)
            else:
                destination = path
            links += 1
            if not destination.exists():
                errors.append(f"{path.relative_to(ROOT)}: missing {target}")
            elif parsed.fragment and destination.suffix == ".md":
                if unquote(parsed.fragment) not in headings(destination):
                    errors.append(f"{path.relative_to(ROOT)}: missing anchor {target}")
    for error in errors:
        print(error)
    if errors:
        return 1
    print(
        f"Checked {len(paths)} documents, {links} repository links, "
        f"{shell_examples} shell examples, and {examples} JSON examples."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
