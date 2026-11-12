#!/bin/sh
set -eu

data=${JUICED_DATA:-/data}
config="$data/config.json"
port=${LEMONADE_PORT:-13305}
host=${JUICED_LISTEN_HOST:-0.0.0.0}
ui=${JUICED_UI:-/opt/juiced/ui}
uid=10001
gid=10001

log() { printf 'juiced-entrypoint: %s\n' "$*" >&2; }

# Supplementary groups that own passed-through GPU and NPU nodes. Their GIDs come from the
# host, so the image cannot name them in advance.
device_groups() {
    found=""
    for node in /dev/dri/renderD* /dev/dri/card* /dev/kfd /dev/accel/accel*; do
        [ -e "$node" ] || continue
        group=$(stat -c %g "$node")
        case "$group" in 0 | 65534) continue ;; esac
        case ",$found," in *",$group,"*) continue ;; esac
        found="${found:+$found,}$group"
    done
    printf '%s' "$found"
}

memory_bytes() {
    bytes=$(($(awk '/^MemTotal:/ { print $2 }' /proc/meminfo) * 1024))
    limit=$(cat /sys/fs/cgroup/memory.max 2>/dev/null || true)
    case "$limit" in
        "" | max | *[!0-9]*) ;;
        *) [ "$limit" -lt "$bytes" ] && bytes=$limit ;;
    esac
    printf '%s' "$bytes"
}

write_default_config() {
    budget=${JUICED_RESIDENT_BUDGET_BYTES:-$(($(memory_bytes) / 4 * 3))}
    case "$budget" in "" | *[!0-9]*) log "JUICED_RESIDENT_BUDGET_BYTES must be a byte count"; exit 64 ;; esac
    staging="$config.first-boot.$$"
    cat >"$staging" <<EOF
{
  "schema_version": 2,
  "inhibit_suspend": false,
  "admission": {
    "startup": { "concurrent": 1, "pending": 64, "wait_ms": 30000 }
  },
  "resident_budget_bytes": $budget,
  "startup_timeout_ms": 120000,
  "request_timeout_ms": 300000,
  "models": []
}
EOF
    mv "$staging" "$config"
    log "created $config with a resident budget of $budget bytes"
}

case "${1:-}" in
    "" | -*) ;;
    *) exec "$@" ;;
esac

if [ "$(id -u)" = 0 ]; then
    if [ "${JUICED_UPDATE_USERSPACE:-0}" = 1 ]; then
        /usr/local/bin/juiced-update-userspace || log "userspace update failed; starting with the installed packages"
    fi
    [ -d "$data" ] || install -d -m 0700 "$data"
    if [ "$(stat -c %u "$data")" != "$uid" ]; then
        log "assigning $data to uid $uid (top directory only)"
        chown "$uid:$gid" "$data"
        chmod 0700 "$data"
    fi
    groups=$(device_groups)
    if [ -n "$groups" ]; then
        log "adding device groups $groups"
        set -- --groups="$groups" -- "$0" "$@"
    else
        set -- --clear-groups -- "$0" "$@"
    fi
    exec setpriv --reuid="$uid" --regid="$gid" --inh-caps=-all --bounding-set=-all "$@"
fi

if [ ! -w "$data" ]; then
    log "$data is not writable by uid $(id -u); mount a volume owned by that uid or start the container as root once"
    exit 73
fi

# Warnings only: system-info reports the same conditions as backend reasons.
device_checks() {
    for node in /dev/kfd /dev/dri/renderD* /dev/accel/accel*; do
        [ -e "$node" ] || continue
        [ -r "$node" ] && [ -w "$node" ] || log "$node is present but not readable and writable by uid $(id -u); add its group or start the container as root once"
    done
    if [ -e /dev/kfd ] && ! ls /dev/dri/renderD* >/dev/null 2>&1; then
        log "/dev/kfd is present without /dev/dri render nodes; ROCm engines need --device /dev/dri as well"
    fi
    if ls /dev/accel/accel* >/dev/null 2>&1; then
        # shellcheck disable=SC3045 # dash, the image's /bin/sh, implements ulimit -l.
        memlock=$(ulimit -l 2>/dev/null || echo unknown)
        [ "$memlock" = unlimited ] || log "the locked-memory limit is $memlock KiB; AMD NPU engines (FastFlowLM, XDNA hybrid) need --ulimit memlock=-1:-1"
    fi
}

umask 077
device_checks
mkdir -p "$data/home"
[ -e "$config" ] || write_default_config

case "$host" in *:*) listen="[$host]:$port" ;; *) listen="$host:$port" ;; esac
exec /usr/local/bin/lemond --config "$config" --ui "$ui" --listen "$listen" "$@"
