#!/usr/bin/env bash
set -euo pipefail
case "$1" in
    pack)
        python3 - <<'PY'
import json
from pathlib import Path
paths = {Path('target/test-shards.json'), Path('target/ci-bootstrap-build/debug/proto-lab')}
root = Path('target/coverage-continuous-build')
for path in root.rglob('*'):
    if path.is_file() and (path.suffix == '.so' or (path.stat().st_mode & 0o111 and not path.suffix)):
        paths.add(path)
shards = json.loads(Path('target/test-shards.json').read_text())
assigned = [{Path(test['executable']).relative_to('/workspace') for test in shard} for shard in shards]
all_tests = set().union(*assigned)
if not all_tests <= paths:
    raise ValueError(f'missing test executables: {all_tests - paths}')
lists = {'common': paths - all_tests, **{f'shard-{i}': tests for i, tests in enumerate(assigned)}}
for name, files in lists.items():
    Path(f'target/bundle-{name}-files').write_text(''.join(f'{path}\n' for path in sorted(files)))
PY
        for list in target/bundle-*-files; do
            name=${list#target/bundle-}
            name=${name%-files}
            tar -I 'zstd -T2 -3' -cf "target/coverage-$name.tar.zst" -T "$list"
        done
        du -h target/coverage-*.tar.zst
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
