#!/usr/bin/env bash
set -euo pipefail
# The exact cache key hashes vendor contents; checkout times must not invalidate matching objects.
if [[ -d vendor ]]; then
    find vendor -type f -exec touch -m -d @0 -- {} +
fi
