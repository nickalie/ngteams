#!/usr/bin/env bash
# Builds the sample bots' images (`localhost/ngteams-bot-<name>:1`), which ngteams-server's container tests
# (`binary::bots::containers`) run: the names in NGTEAMS_SERVER_BOT_IMAGES (`teams,agents,botbuilder-python,
# botbuilder-dotnet`), or all. With podman unless NGTEAMS_SERVER_BOT_ENGINE names another engine.
set -euo pipefail
cd "$(dirname "$0")"
engine=${NGTEAMS_SERVER_BOT_ENGINE:-podman}
IFS=, read -r -a names <<< "${NGTEAMS_SERVER_BOT_IMAGES:-teams,agents,botbuilder-python,botbuilder-dotnet}"
for sdk in "${names[@]}"; do
    sdk=${sdk// /}
    [[ -f "$sdk/Containerfile" ]] || { echo "no sample bot named $sdk" >&2; exit 1; }
    "$engine" build -q -f "$sdk/Containerfile" -t "localhost/ngteams-bot-$sdk:1" .
done
