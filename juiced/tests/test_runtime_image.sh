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
check_links /opt/juiced/llama-cpu/bin/llama-server /opt/juiced/llama-cpu/bin
check_links /opt/juiced/llama-cpu/bin/libllama-server-impl.so /opt/juiced/llama-cpu/bin
test -f /opt/lemonade/resources/juiced/index.html
test -x /opt/juiced/futo/futo-asr-server
test -x /opt/juiced/parakeet/parakeet-server
test -x /opt/juiced/llama/llama-server
test -x /opt/juiced/llama-cpu/llama-server
test ! -e /opt/juiced/llama-cpu/bin/libggml-vulkan.so
test ! -e /opt/juiced/llama-cpu/bin/libggml-rpc.so
test ! -e /opt/juiced/llama-cpu/bin/ggml-rpc-server
python3 - <<'PYTHON'
import hashlib, json
from pathlib import Path
root = Path('/opt/juiced/llama-cpu')
manifest = json.loads((root/'variant.json').read_text())
assert manifest['variant'] == 'cpu'
actual = {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest() for path in root.rglob('*') if path.is_file() and path != root/'variant.json'}
assert actual == manifest['files'] and actual
assert not any(path.name.startswith('libggml-') and not path.name.startswith(('libggml-cpu', 'libggml-base')) for path in (root/'bin').iterdir())
settings = json.loads(Path('/opt/juiced/defaults.json').read_text())
assert settings['llamacpp']['cpu_bin'] == '/opt/juiced/llama-cpu'
assert settings['llamacpp']['vulkan_bin'] == '/opt/juiced/llama'
PYTHON
/opt/juiced/voice/bin/python3 -c 'import wyoming_openai, wyoming; import importlib.util; assert importlib.util.find_spec("pip") is None; assert importlib.util.find_spec("setuptools") is None'
test -f /opt/juiced/voice/launcher.py
test ! -e /opt/juiced/preparation
test ! -e /opt/juiced/llama/bin/llama
if find /opt/juiced/llama /opt/juiced/llama-cpu -name '*quantize*' -o -name 'convert_hf_to_gguf*' | grep -q .; then
    echo 'FAIL: model producer tooling remains in the serving image' >&2
    exit 1
fi
echo 'PASS: runtime excludes build tools and resolves native binary dependencies'
