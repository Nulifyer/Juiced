#!/bin/sh
set -eu

for tool in cc gcc g++ c++ make cmake ninja git node npm bun cargo rustc pip pip3; do
    if command -v "$tool" >/dev/null 2>&1; then
        echo "Unexpected build tool in runtime image: $tool" >&2
        exit 1
    fi
done

for package in build-essential dpkg-dev linux-libc-dev libc6-dev; do
    if [ "$(dpkg-query -W -f='${db:Status-Status}' "$package" 2>/dev/null || true)" = installed ]; then
        echo "Unexpected development package in runtime image: $package" >&2
        exit 1
    fi
done

check_links() {
    output=$(LD_LIBRARY_PATH="$2" ldd "$1")
    if printf '%s\n' "$output" | grep -q 'not found'; then
        printf '%s\n' "$output" >&2
        exit 1
    fi
}

check_links /opt/lemonade/lemond ''
check_links /opt/lemonade/lemonade ''
check_links /opt/juiced/futo/futo-server /opt/juiced/futo/lib
check_links /opt/juiced/parakeet/bin/parakeet-server /opt/juiced/parakeet/lib
check_links /opt/juiced/llama/bin/llama-server /opt/juiced/llama/bin
test -f /opt/lemonade/resources/juiced/index.html
test -x /opt/juiced/futo/futo-asr-server
test -x /opt/juiced/parakeet/parakeet-server
test -x /opt/juiced/llama/llama-server
/opt/juiced/voice/bin/python3 -c 'import wyoming_openai, wyoming; import importlib.util; assert importlib.util.find_spec("pip") is None; assert importlib.util.find_spec("setuptools") is None'
test -f /opt/juiced/voice/launcher.py
check_links /opt/juiced/preparation/native/llama-quantize /opt/juiced/preparation/native
/opt/juiced/preparation/bin/python3 -c 'import torch, gguf, transformers; import importlib.util; assert not torch.cuda.is_available(); assert importlib.util.find_spec("pip") is None'
test -f /opt/juiced/preparation/prepare.py
echo 'PASS: runtime excludes build tools and resolves native binary dependencies'
