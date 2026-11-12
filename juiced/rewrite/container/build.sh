#!/bin/sh
# Builds the Juiced rewrite image locally. Never pushes.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
image=${JUICED_IMAGE:-localhost/juiced-rewrite:dev}
engine=${CONTAINER_ENGINE:-podman}
revision=$(git -C "$root" rev-parse --short=12 HEAD 2>/dev/null || echo development)

set -- --file "$here/Containerfile" --build-arg "SOURCE_REVISION=$revision" --build-arg "SERVICE_VERSION=${JUICED_SERVICE_VERSION:-}" --tag "$image" "$@"
# Docker BuildKit finds Containerfile.dockerignore by name. Podman needs it named, and
# --format docker keeps HEALTHCHECK, which the OCI image format drops.
if [ "$engine" = podman ]; then
    set -- --format docker --ignorefile "$here/Containerfile.dockerignore" "$@"
fi

exec "$engine" build "$@" "$root"
