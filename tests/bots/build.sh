#!/usr/bin/env bash
# Builds the SDK bot images of ngteams-server's container tests (`binary::bots::containers`), with podman unless
# NGTEAMS_SERVER_BOT_ENGINE names another engine. The test runs them when NGTEAMS_SERVER_BOT_IMAGES=teams,agents.
set -euo pipefail
cd "$(dirname "$0")"
engine=${NGTEAMS_SERVER_BOT_ENGINE:-podman}
for sdk in teams agents; do
    "$engine" build -q -f "$sdk/Containerfile" -t "localhost/ngteams-bot-$sdk:1" .
done
