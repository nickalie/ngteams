#!/usr/bin/env bash
set -euo pipefail
archives=("$1"/*.tar.zst)
[[ -f "${archives[0]}" ]] || { echo "No coverage archives in $1" >&2; exit 1; }
printf '%s\0' "${archives[@]}" | xargs -0 -P 4 -I '{}' tar -I 'zstd --long=30' -xf '{}'
