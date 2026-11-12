#!/bin/sh
# Writes a runtime-package import specification for an engine directory baked into the
# image, so the entrypoint can install it with `lemond --import-runtime-bundle` offline.
#   bundle-spec.sh ROOT ENTRYPOINT IMPLEMENTATION_ID PROVENANCE_JSON TARGET_JSON
# ROOT is the absolute path the directory has in the runtime image. Roles and library
# directories follow the engine installer's archive description.
set -eu

[ "$#" = 5 ] || { echo "usage: $0 ROOT ENTRYPOINT IMPLEMENTATION_ID PROVENANCE_JSON TARGET_JSON" >&2; exit 64; }
root=$1 entrypoint=$2 implementation=$3 provenance=$4 target=$5
[ -f "$root/$entrypoint" ] || { echo "$root/$entrypoint is missing" >&2; exit 66; }
if find "$root" ! -type f ! -type d | grep -q .; then
    echo "$root contains links or special files; the importer refuses them" >&2
    exit 65
fi

listing=$(mktemp)
trap 'rm -f "$listing"' EXIT
(cd "$root" && find . -type f | sed 's#^\./##' | LC_ALL=C sort) | while IFS= read -r path; do
    file="$root/$path"
    bytes=$(stat -c %s "$file")
    name=${path##*/}
    if [ "$bytes" = 0 ]; then
        role=resource
    elif [ "$path" = "$entrypoint" ]; then
        role=executable
    else
        case "$name" in
            *.so | *.so.[0-9]*) role=library ;;
            *) if [ -x "$file" ]; then role=executable; else role=resource; fi ;;
        esac
    fi
    case "$path" in */*) directory=${path%/*} ;; *) directory=. ;; esac
    jq -nc --arg path "$path" --arg sha "$(sha256sum "$file" | cut -d' ' -f1)" \
        --arg role "$role" --argjson bytes "$bytes" --arg directory "$directory" \
        '{path: $path, sha256: $sha, role: $role, bytes: $bytes, directory: $directory}'
done >"$listing"

case "$entrypoint" in */*) entry_directory=${entrypoint%/*} ;; *) entry_directory=. ;; esac
jq -s --arg root "$root" --arg entrypoint "$entrypoint" --arg implementation "$implementation" \
    --arg entry_directory "$entry_directory" --argjson provenance "$provenance" \
    --argjson target "$target" '{
  schema_version: 1,
  bundle: {
    schema_version: 1,
    implementation_id: $implementation,
    root: $root,
    entrypoint: $entrypoint,
    library_directories: ([$entry_directory] + [.[] | select(.role == "library") | .directory] | unique),
    files: [.[] | {path, sha256, role}]
  },
  provenance: $provenance,
  declared_target: $target,
  files: [.[] | {path, bytes}]
}' "$listing"
