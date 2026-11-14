#!/usr/bin/env bash
set -euo pipefail
mkdir -p .ci-cache/registry .ci-cache/git
arguments=()
if [[ -d .ci-cache/webrtc/include ]]; then
    arguments+=(--env LK_CUSTOM_WEBRTC=/workspace/.ci-cache/webrtc)
fi
# The shards' service containers listen on the runner; only a job that sets their URLs forwards them.
for name in NGTEAMS_SERVER_PG_URL NGTEAMS_SERVER_REDIS_URL; do
    if [[ -n "${!name:-}" ]]; then
        arguments+=(--env "$name")
    fi
done
exec docker run --rm --init --privileged --shm-size=2g \
    --tmpfs /dev/dri:rw,nosuid,nodev,noexec,size=1m \
    --add-host=host.docker.internal:host-gateway \
    --volume "$PWD:/workspace" \
    --volume "$PWD/.ci-cache/registry:/home/ci/.cargo/registry" \
    --volume "$PWD/.ci-cache/git:/home/ci/.cargo/git" \
    "${arguments[@]}" ngteams-coverage /usr/local/bin/coverage-ci "$@"
