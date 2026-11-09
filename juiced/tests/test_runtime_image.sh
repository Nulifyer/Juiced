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
test ! -e /opt/juiced/preparation
test ! -e /opt/juiced/llama/bin/llama
if find /opt/juiced/llama -name '*quantize*' -o -name 'convert_hf_to_gguf*' | grep -q .; then
    echo 'FAIL: model producer tooling remains in the serving image' >&2
    exit 1
fi
echo 'PASS: runtime excludes build tools and resolves native binary dependencies'
