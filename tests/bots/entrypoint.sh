#!/bin/sh
# The CA the test's server certificate chains to comes as `BOT_CA_PEM`: an image run through a remote engine (CI's
# Docker socket) can't mount a file of the test's own filesystem. Node reads it at start.
set -eu
if [ -n "${BOT_CA_PEM:-}" ]; then
    printf '%s\n' "$BOT_CA_PEM" > /tmp/ca.pem
    export NODE_EXTRA_CA_CERTS=/tmp/ca.pem
fi
exec node "$@"
