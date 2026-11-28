#!/usr/bin/env bash
# Zips a sample's Teams app package: `package.sh <sample> [<out.zip>]` (default `<sample>.zip` here). The manifest's
# `${APP_ID}` and `${BOT_ID}` become APP_ID and BOT_ID from the environment, or new GUIDs; the script prints both.
# Upload the zip in ngteams (Settings → Manage apps) or with `ngteams-server app add --package <out.zip>`.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
sample=${1:?usage: package.sh <sample> [<out.zip>]}
sample=${sample%/}
out=$(realpath -m "${2:-$here/$sample.zip}")
[[ -f "$here/$sample/manifest.json" ]] || { echo "no sample named $sample" >&2; exit 1; }
guid() { python3 -c 'import uuid; print(uuid.uuid4())'; }
app_id=${APP_ID:-$(guid)}
bot_id=${BOT_ID:-$(guid)}
python3 - "$here/$sample" "$out" "$app_id" "$bot_id" <<'PY'
import sys, zipfile
sample, out, app_id, bot_id = sys.argv[1:]
manifest = open(f"{sample}/manifest.json", encoding="utf-8").read()
manifest = manifest.replace("${APP_ID}", app_id).replace("${BOT_ID}", bot_id)
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as package:
    package.writestr("manifest.json", manifest)
    for icon in ("color.png", "outline.png"):
        package.write(f"{sample}/{icon}", icon)
PY
echo "package $out"
echo "app id $app_id"
echo "bot id $bot_id"
