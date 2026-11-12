#!/bin/sh
set -eu

host=${JUICED_LISTEN_HOST:-0.0.0.0}
case "$host" in
    0.0.0.0) host=127.0.0.1 ;;
    "::") host="[::1]" ;;
    *:*) host="[$host]" ;;
esac
key=${LEMONADE_ADMIN_API_KEY:-${LEMONADE_API_KEY:-}}

# The key goes through a curl config on stdin so it never appears in the process list.
if [ -n "$key" ]; then
    printf 'header = "Authorization: Bearer %s"\n' "$key"
fi | curl --config - --fail --silent --show-error --max-time 4 --output /dev/null \
    "http://$host:${LEMONADE_PORT:-13305}/v1/health"
