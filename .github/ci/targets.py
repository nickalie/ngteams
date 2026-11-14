#!/usr/bin/env python3
"""Partition Cargo test executables; all shards use one instrumented build."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def partition(messages, packages, weights, count=8):
    tests = {}
    for message in messages:
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        if not message["profile"]["test"]:
            continue
        manifest = Path(message["manifest_path"])
        package = packages[str(manifest)]
        target = message["target"]
        identity = f"{package}/{'+'.join(target['kind'])}/{target['name']}"
        if identity in tests:
            raise ValueError(f"duplicate test target: {identity}")
        tests[identity] = {
            "id": identity, "package": package, "target": target["name"],
            "executable": message["executable"], "directory": str(manifest.parent),
            "seconds": weights.get(f"{package}/{target['name']}", 10),
        }
    if not tests or set(packages.values()) != {test["package"] for test in tests.values()}:
        raise ValueError("compiled tests do not cover every runtime package")
    shards = [[] for _ in range(count)]
    loads = [0] * count
    for test in sorted(tests.values(), key=lambda test: (-test["seconds"], test["id"])):
        shard = min(range(count), key=lambda index: (loads[index], index))
        shards[shard].append(test)
        loads[shard] += test["seconds"]
    return shards


def inventory(source, destination):
    sys.path.insert(0, "scripts")
    import importlib
    reporter = importlib.import_module("coverage-report")
    _, crates = reporter.configuration()
    packages = {str(path / "Cargo.toml"): name for name, path in crates.items()}
    weights = json.loads(Path(".github/ci/test-times.json").read_text())["seconds"]
    messages = [json.loads(line) for line in source.read_text().splitlines()]
    shards = partition(messages, packages, weights)
    destination.write_text(json.dumps(shards, indent=2) + "\n")
    for index, tests in enumerate(shards):
        print(f"Shard {index}: {len(tests)} targets, estimated {sum(t['seconds'] for t in tests)} seconds")


def run(manifest, index):
    shards = json.loads(manifest.read_text())
    failed = False
    results = []
    for test in shards[index]:
        print(f"::group::{test['id']}", flush=True)
        start = time.monotonic()
        environment = dict(os.environ, CARGO_MANIFEST_DIR=test["directory"], CARGO_PKG_NAME=test["package"])
        result = subprocess.run([test["executable"], "--test-threads", "1"],
                                cwd=test["directory"], env=environment, check=False)
        seconds = round(time.monotonic() - start, 2)
        results.append({"id": test["id"], "seconds": seconds, "status": result.returncode})
        failed |= result.returncode != 0
        print(f"::endgroup::\n{test['id']}: {seconds}s, exit {result.returncode}", flush=True)
    Path(f"target/shard-{index}.json").write_text(json.dumps(results, indent=2) + "\n")
    return int(failed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("inventory")
    build.add_argument("messages", type=Path)
    build.add_argument("destination", type=Path)
    test = commands.add_parser("run")
    test.add_argument("manifest", type=Path)
    test.add_argument("shard", type=int, choices=range(8))
    args = parser.parse_args()
    if args.command == "inventory":
        inventory(args.messages, args.destination)
        return 0
    return run(args.manifest, args.shard)


if __name__ == "__main__":
    sys.exit(main())
