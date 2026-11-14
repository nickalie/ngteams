#!/usr/bin/env bash
set -euo pipefail

source .github/ci/environment.sh
case "${1:-}" in
    build)
        .github/ci/vendor-times.sh
        coverage-ci-smoke
        CARGO_TARGET_DIR=target/ci-bootstrap-build cargo run --locked -p proto-lab -- chrome install
        chrome_path=$(target/ci-bootstrap-build/debug/proto-lab chrome path)
        coverage-ci-smoke "$chrome_path"
        CARGO_TARGET_DIR=target/ci-bootstrap-build cargo test --locked -p gpui-pre-linux --lib local_selection_tests
        package_args=()
        while read -r package; do package_args+=(-p "$package"); done < <(python3 scripts/coverage-report.py list)
        cargo llvm-cov clean --workspace
        eval "$(cargo llvm-cov show-env --sh)"
        cargo test --locked --tests --no-run "${package_args[@]}" --message-format=json > target/test-artifacts.json
        python3 .github/ci/targets.py inventory target/test-artifacts.json target/test-shards.json
        ;;
    test)
        # Without these the server's Postgres and Redis cases skip themselves and still pass.
        : "${NGTEAMS_SERVER_PG_URL:?the shard needs its Postgres service}"
        : "${NGTEAMS_SERVER_REDIS_URL:?the shard needs its Redis service}"
        coverage-ci-smoke
        target/ci-bootstrap-build/debug/proto-lab chrome install
        coverage-ci-smoke "$(target/ci-bootstrap-build/debug/proto-lab chrome path)"
        export LLVM_PROFILE_FILE_NAME="shard-$2-%p-%m%c.profraw"
        eval "$(cargo llvm-cov show-env --sh)"
        export LD_LIBRARY_PATH="$CARGO_LLVM_COV_TARGET_DIR/debug/deps:$(rustc --print target-libdir)${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
        exec python3 .github/ci/targets.py run target/test-shards.json "$2"
        ;;
    fetch)
        exec cargo fetch --locked --target "$(rustc -vV | sed -n 's/^host: //p')"
        ;;
    report)
        mkdir -p target/coverage/continuous/all
        cargo llvm-cov report --locked --json --summary-only --output-path target/coverage/continuous/all/llvm.json
        mapfile -t packages < <(python3 scripts/coverage-report.py list)
        exec python3 scripts/coverage-report.py report target/coverage/continuous/all/llvm.json \
            --output target/coverage/continuous/all/summary.json --require-target --packages "${packages[@]}"
        ;;
    *) echo 'usage: coverage-ci build|test INDEX|fetch|report' >&2; exit 2 ;;
esac
