#!/usr/bin/env bash
set -euo pipefail
case "$1" in
    pack)
        python3 - <<'PY'
import json
from pathlib import Path
paths = {Path('target/test-shards.json'), Path('target/ci-bootstrap-build/debug/proto-lab')}
root = Path('target/coverage-continuous-build')
paths.update(root.glob('debug/.fingerprint/*/*.json'))
for path in root.rglob('*'):
    if path.is_file() and path.suffix == '.so':
        paths.add(path)
for line in Path('target/test-artifacts.json').read_text().splitlines():
    message = json.loads(line)
    if message.get('reason') == 'compiler-artifact' and message.get('executable'):
        paths.add(Path(message['executable']).relative_to('/workspace'))
shards = json.loads(Path('target/test-shards.json').read_text())
assigned = [{Path(test['executable']).relative_to('/workspace') for test in shard} for shard in shards]
all_tests = set().union(*assigned)
if not all_tests <= paths:
    raise ValueError(f'missing test executables: {all_tests - paths}')
shared = {path for path in all_tests if sum(path in tests for tests in assigned) > 1}
lists = {'common': (paths - all_tests) | shared,
         **{f'shard-{i}': tests - shared for i, tests in enumerate(assigned)}}
for name, files in lists.items():
    Path(f'target/bundle-{name}-files').write_text(''.join(f'{path}\n' for path in sorted(files)))
PY
        for list in target/bundle-*-files; do
            name=${list#target/bundle-}
            name=${name%-files}
            tar --format=posix -I 'zstd --long=30 -T4 -3' -cf "target/coverage-$name.tar.zst" -T "$list"
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
        tar --format=posix -I 'zstd --long=30 -T4 -3' -cf .ci-cache/build/dependencies.tar.zst target/coverage-continuous-build
        bytes=$(stat -c %s .ci-cache/build/dependencies.tar.zst)
        echo "Compressed dependency cache: $bytes bytes (limit 1 GiB)"
        if ((bytes > 1073741824)); then
            rm .ci-cache/build/dependencies.tar.zst
        fi
        ;;
    *) exit 2 ;;
esac
