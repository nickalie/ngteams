#!/usr/bin/env bash
set -euo pipefail
case "$1" in
    pack)
        python3 - <<'PY' > target/bundle-files
from pathlib import Path
paths = {Path('target/test-shards.json'), Path('target/ci-bootstrap-build/debug/proto-lab')}
root = Path('target/coverage-continuous-build')
for path in root.rglob('*'):
    if path.is_file() and (path.suffix == '.so' or (path.stat().st_mode & 0o111 and not path.suffix)):
        paths.add(path)
for path in sorted(paths):
    print(path)
PY
        tar -I 'zstd -T2 -3' -cf target/test-build.tar.zst -T target/bundle-files
        ;;
    cache)
        source .github/ci/environment.sh
        sdk=$(find target/coverage-continuous-build -path '*/linux-x64-release/lib/libwebrtc.a' -print -quit)
        if [[ -n "$sdk" && ! -d .ci-cache/webrtc/include ]]; then
            mkdir -p .ci-cache/webrtc
            cp -a "${sdk%/lib/libwebrtc.a}/." .ci-cache/webrtc/
        fi
        cargo llvm-cov clean --workspace
        find target/coverage-continuous-build -name '*.profraw' -delete
        find target/coverage-continuous-build -name '*.profdata' -delete
        find target/coverage-continuous-build -type d -name livekit_webrtc -prune -exec rm -rf -- {} +
        mkdir -p .ci-cache/build
        tar -I 'zstd -T2 -3' -cf .ci-cache/build/dependencies.tar.zst target/coverage-continuous-build
        bytes=$(stat -c %s .ci-cache/build/dependencies.tar.zst)
        echo "Compressed dependency cache: $bytes bytes (limit 1 GiB)"
        if ((bytes > 1073741824)); then
            rm .ci-cache/build/dependencies.tar.zst
        fi
        ;;
    *) exit 2 ;;
esac
