#!/usr/bin/env python3
"""Partition Cargo test executables; all shards use one instrumented build."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


LARGE_TARGET = "ngteams/test/large_call_window"


def large_weights(weights):
    prefix = LARGE_TARGET + "/"
    return {name.removeprefix(prefix): seconds for name, seconds in weights.items() if name.startswith(prefix)}


def large_cases(weights):
    scenes = large_weights(weights)
    # The 1:1 scene rings only after Leave ends the existing group call.
    sequence = ("leave", "one-to-one")
    if not set(sequence).isdisjoint(scenes):
        if not set(sequence).issubset(scenes):
            raise ValueError("large-call Leave and 1:1 scenes must stay together")
        scenes[",".join(sequence)] = sum(scenes.pop(name) for name in sequence)
    return scenes


def validate_large_steps(source, weights):
    body = source.partition("fn scenarios(")[2].partition("\n}\n")[0]
    table = re.search(r"let steps:\s*\[[^\]]+;\s*(\d+)\]\s*=\s*\[(.*?)\];", body, re.S)
    names = re.findall(r'\("([^"]*)",', table[2]) if table else []
    if not table or len(names) != int(table[1]):
        raise ValueError("large-call step table cannot be inventoried")
    names += re.findall(r'wanted\("([^"]*)"\)', body)
    configured = large_weights(weights)
    if any(not name or "," in name for name in names) or len(names) != len(set(names)) or set(names) != set(configured):
        raise ValueError(f"large-call scenes changed: declared {sorted(names)}, configured {sorted(configured)}")


def partition(messages, packages, weights, count=8):
    tests = {}
    compiled = set()
    for message in messages:
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        if not message["profile"]["test"]:
            continue
        manifest = Path(message["manifest_path"])
        package = packages[str(manifest)]
        target = message["target"]
        identity = f"{package}/{'+'.join(target['kind'])}/{target['name']}"
        if identity in compiled:
            raise ValueError(f"duplicate test target: {identity}")
        compiled.add(identity)
        test = {
            "id": identity, "package": package, "target": target["name"],
            "executable": message["executable"], "directory": str(manifest.parent),
            "seconds": weights.get(identity, weights.get(f"{package}/{target['name']}", 10)),
        }
        if identity == LARGE_TARGET:
            scenes = large_cases(weights)
            if not scenes:
                raise ValueError("large-call scene timings are missing")
            for name, seconds in scenes.items():
                tests[f"{identity}/{name}"] = dict(test, id=f"{identity}/{name}", seconds=seconds,
                                                  environment={"LARGE_CALL_STEPS": name})
        else:
            tests[identity] = test
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
    validate_large_steps(Path("crates/ngteams/tests/support/linux/large_call_window.rs").read_text(), weights)
    messages = [json.loads(line) for line in source.read_text().splitlines()]
    shards = partition(messages, packages, weights)
    destination.write_text(json.dumps(shards, indent=2) + "\n")
    for index, tests in enumerate(shards):
        print(f"Shard {index}: {len(tests)} test invocations, estimated {sum(t['seconds'] for t in tests)} seconds")


def run(manifest, index):
    shards = json.loads(manifest.read_text())
    failed = False
    results = []
    for position, test in enumerate(shards[index]):
        print(f"::group::{test['id']}", flush=True)
        start = time.monotonic()
        environment = dict(os.environ, CARGO_MANIFEST_DIR=test["directory"], CARGO_PKG_NAME=test["package"])
        environment.update(test.get("environment", {}))
        if profile := environment.get("LLVM_PROFILE_FILE"):
            environment["LLVM_PROFILE_FILE"] = str(
                Path(profile).with_name(f"shard-{index}-case-{position}-%p-%m%c.profraw"))
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
