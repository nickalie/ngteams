#!/usr/bin/env bash
set -euo pipefail
mkdir -p .ci-cache/registry .ci-cache/git
arguments=()
arguments+=(--env "COVERAGE_WORKSPACE_RESTORED=${COVERAGE_WORKSPACE_RESTORED:-0}")
if [[ -d .ci-cache/webrtc/include ]]; then
    arguments+=(--env LK_CUSTOM_WEBRTC=/workspace/.ci-cache/webrtc)
fi
exec docker run --rm --init --privileged --shm-size=2g \
    --tmpfs /dev/dri:rw,nosuid,nodev,noexec,size=1m \
    --volume "$PWD:/workspace" \
    --volume "$PWD/.ci-cache/registry:/home/ci/.cargo/registry" \
    --volume "$PWD/.ci-cache/git:/home/ci/.cargo/git" \
    "${arguments[@]}" ngteams-coverage /usr/local/bin/coverage-ci "$@"
