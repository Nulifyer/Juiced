#!/bin/sh
# Builds the image, runs it against a throwaway volume, probes the API and UI, then removes
# the container and volume. Extra arguments go to `podman run`, for example:
#   smoke-test.sh --device nvidia.com/gpu=all
#   SKIP_BUILD=1 EXPECT_GPU=nvidia smoke-test.sh --device nvidia.com/gpu=all
set -eu

here=$(cd "$(dirname "$0")" && pwd)
image=${JUICED_IMAGE:-localhost/juiced-rewrite:dev}
engine=${CONTAINER_ENGINE:-podman}
port=${SMOKE_PORT:-13399}
name="juiced-smoke-$$"
volume="juiced-smoke-$$"
key="smoke-admin-$$-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
base="http://127.0.0.1:$port"

for tool in curl jq; do
    command -v "$tool" >/dev/null || { echo "smoke-test needs $tool" >&2; exit 69; }
done

cleanup() {
    status=$?
    if [ "$status" != 0 ]; then
        echo "--- container log (last 40 lines)" >&2
        "$engine" logs --tail 40 "$name" >&2 2>&1 || true
    fi
    "$engine" rm -f -t 5 "$name" >/dev/null 2>&1 || true
    "$engine" volume rm -f "$volume" >/dev/null 2>&1 || true
    exit "$status"
}
trap cleanup EXIT INT TERM

step() { printf '== %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
admin() { curl -fsS --max-time 20 -H "Authorization: Bearer $key" "$@"; }

if [ "${SKIP_BUILD:-0}" != 1 ]; then
    step "build $image"
    start=$(date +%s)
    "$here/build.sh"
    echo "build took $(($(date +%s) - start))s"
fi
echo "image size: $("$engine" image inspect "$image" --format '{{.Size}}' | awk '{ printf "%.0f MB", $1 / 1000000 }')"

step "run $name"
"$engine" volume create "$volume" >/dev/null
"$engine" run -d --name "$name" -p "127.0.0.1:$port:13305" -v "$volume:/data" \
    -e LEMONADE_ADMIN_API_KEY="$key" "$@" "$image" >/dev/null

step "wait for /v1/health"
i=0
until admin -o /dev/null "$base/v1/health" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -lt 60 ] || fail "health did not respond within 60s"
    sleep 1
done
admin "$base/v1/health" | jq -c .
echo "healthy after ~${i}s"

step "authentication"
code=$(curl -s -o /dev/null -w '%{http_code}' "$base/v1/system-info")
[ "$code" = 401 ] || fail "unauthenticated /v1/system-info returned $code, expected 401"
echo "unauthenticated system-info: $code"

step "container healthcheck"
"$engine" healthcheck run "$name" && echo "healthcheck: healthy"

step "first-boot config"
"$engine" exec "$name" cat /data/config.json | jq -c '{schema_version, inhibit_suspend, resident_budget_bytes, models: (.models | length)}'
if "$engine" exec "$name" grep -rqF "$key" /data; then
    fail "the API key was written under /data"
fi
echo "API key not present under /data"
"$engine" exec "$name" ls -A /data | tr '\n' ' '
echo

step "/v1/system-info"
info=$(admin "$base/v1/system-info")
printf '%s' "$info" | jq -c '{detection: .host.detection, cpu: .devices.cpu.name, threads: .devices.cpu.usable_threads, memory_bytes: .devices.memory.total_bytes}'
printf '%s' "$info" | jq -c '.devices.gpus[]? | {vendor, name, family, compute_capability, vram_total_bytes, render_node_access, sources}' | sed 's/^/gpu: /'
printf '%s' "$info" | jq -c '{vulkan: .devices.vulkan.state, vulkan_basis: .devices.vulkan.basis, vulkan_devices: [.devices.vulkan.devices[]?.name], cuda: .devices.cuda, rocm_kfd: .devices.rocm.kfd, npus: (.devices.npus | length)}'
printf '%s' "$info" | jq -c '.recommendations | with_entries(select(.value != null))' | sed 's/^/recommendations: /'
if [ -n "${EXPECT_GPU:-}" ]; then
    printf '%s' "$info" | jq -e --arg vendor "$EXPECT_GPU" \
        'any(.devices.gpus[]?; .vendor == $vendor and (.sources | length) > 1)' >/dev/null ||
        fail "system-info did not confirm a $EXPECT_GPU GPU through a vendor tool"
    echo "system-info confirms a $EXPECT_GPU GPU"
fi

step "/v1/juiced/engines"
engines=$(admin "$base/v1/juiced/engines")
printf '%s' "$engines" | jq -r '
  .installable[] |
  "\(.engine) \(.backend): availability=\(.availability) recommended=\(.recommended) build=\(.build.asset // ("unresolved:" + (.unresolved | tostring))) installed=\(.installed_package // "no")"'
printf '%s' "$engines" | jq -e 'any(.installable[]; .engine == "futo-transcribe.cpp" and .installed_package != null)' >/dev/null ||
    fail "the bundled FUTO engine was not installed on first boot"

step "console at /"
page=$(curl -fsS --max-time 10 "$base/")
printf '%s' "$page" | grep -qi '<html' || fail "/ did not return HTML"
script=$(printf '%s' "$page" | grep -o 'src="[^"]*\.js"' | head -1 | sed 's/src="//; s/"$//; s#^\./#/#')
[ -n "$script" ] || fail "no script tag in /"
case "$script" in /*) ;; *) script="/$script" ;; esac
curl -fsS --max-time 10 -o /dev/null "$base$script" || fail "console script $script did not load"
echo "/ returned HTML and $script loaded"

step "restart keeps config"
before=$("$engine" exec "$name" sha256sum /data/config.json)
"$engine" restart -t 10 "$name" >/dev/null
i=0
until admin -o /dev/null "$base/v1/health" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -lt 60 ] || fail "health did not return after restart"
    sleep 1
done
after=$("$engine" exec "$name" sha256sum /data/config.json)
[ "$before" = "$after" ] || fail "config.json changed across restart"
echo "config.json unchanged across restart"

echo "PASS"
