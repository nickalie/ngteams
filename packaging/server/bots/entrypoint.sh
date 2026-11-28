#!/bin/sh
# The CA the server's certificate chains to comes as `BOT_CA_PEM`: an image run through a remote engine (CI's Docker
# socket) can't mount a file of the caller's filesystem. Node adds it to its own CAs; Python and .NET read a bundle.
set -eu
if [ -n "${BOT_CA_PEM:-}" ]; then
    printf '%s\n' "$BOT_CA_PEM" > /tmp/ca.pem
    cat /etc/ssl/certs/ca-certificates.crt /tmp/ca.pem > /tmp/bundle.pem 2>/dev/null || cp /tmp/ca.pem /tmp/bundle.pem
    export NODE_EXTRA_CA_CERTS=/tmp/ca.pem SSL_CERT_FILE=/tmp/bundle.pem REQUESTS_CA_BUNDLE=/tmp/bundle.pem
fi
exec "$@"
