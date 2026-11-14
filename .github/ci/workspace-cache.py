#!/usr/bin/env python3
"""Reuse compiler artifacts while checking every tracked source before restoring its timestamp."""

import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


DIRECTORIES = (Path("target/coverage-continuous-build"), Path("target/ci-bootstrap-build"))
SOURCES = Path("target/compiler-workspace-sources.json")
INPUTS = Path(".ci-cache/workspace-inputs")


def tracked():
    return [Path(os.fsdecode(name)) for name in subprocess.check_output(["git", "ls-files", "-z"]).split(b"\0")
            if name]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def source_stamps(key):
    return {"key": key, "sources": {str(path): [digest(path), path.stat().st_mtime_ns]
                                    for path in tracked() if path.is_file() and not path.is_symlink()}}


def restore_stamps(key):
    saved = json.loads(SOURCES.read_text())
    if saved["key"] != key:
        raise ValueError("incompatible compiler cache")
    reused = 0
    for path in tracked():
        if not path.is_file() or path.is_symlink():
            continue
        previous = saved["sources"].get(str(path))
        if previous and digest(path) == previous[0]:
            os.utime(path, ns=(previous[1], previous[1]))
            reused += 1
        else:
            os.utime(path, None)
    print(f"Restored timestamps of {reused} content-verified tracked files", flush=True)


def pack(key):
    SOURCES.write_text(json.dumps(source_stamps(key)) + "\n")
    bundled = {line for listing in Path("target").glob("bundle-*-files") for line in listing.read_text().splitlines()}
    files = [SOURCES, *(path for root in DIRECTORIES for path in root.rglob("*")
                        if path.is_file() and str(path) not in bundled and path.suffix not in (".profraw", ".profdata"))]
    listing = Path("target/compiler-workspace-files")
    listing.write_bytes(b"".join(os.fsencode(path) + b"\0" for path in sorted(files)))
    subprocess.run(["tar", "--format=posix", "--null", "-I", "zstd --long=30 -T4 -3",
                    "-cf", "target/compiler-workspace.tar.zst", "-T", str(listing)], check=True)
    print(f"Compiler metadata archive: {Path('target/compiler-workspace.tar.zst').stat().st_size} bytes", flush=True)


def api(path, *arguments):
    return json.loads(subprocess.check_output(["gh", "api", "--method", "GET", path, *arguments]))


def find_run(repository, branch, name):
    runs = api(f"repos/{repository}/actions/workflows/coverage.yml/runs", "-f", f"branch={branch}",
               "-f", "status=success", "-f", "per_page=20")["workflow_runs"]
    wanted = {name, "coverage-common", *(f"coverage-shard-{i}" for i in range(8))}
    for run in sorted(runs, key=lambda run: (run["run_number"], run["run_attempt"]), reverse=True):
        if (run["status"] != "completed" or run["conclusion"] != "success" or
                run["head_branch"] != branch or run["event"] not in ("push", "workflow_dispatch") or
                run["head_repository"]["full_name"].lower() != repository.lower()):
            continue
        artifacts = api(f"repos/{repository}/actions/runs/{run['id']}/artifacts", "-f", "per_page=100")["artifacts"]
        available = {artifact["name"] for artifact in artifacts if not artifact["expired"]}
        if wanted <= available:
            return run["id"], wanted
    return None


def restore(key, name):
    try:
        found = find_run(os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_REF_NAME"], name)
        if not found:
            print("No compatible successful workspace build", flush=True)
            return False
        run_id, names = found
        INPUTS.mkdir(parents=True, exist_ok=True)

        def download(artifact):
            subprocess.run(["gh", "run", "download", str(run_id), "--repo", os.environ["GITHUB_REPOSITORY"],
                            "--name", artifact, "--dir", str(INPUTS)],
                           check=True, capture_output=True)

        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(download, sorted(names)))
        subprocess.run(["bash", ".github/ci/unpack.sh", str(INPUTS)], check=True)
        for root in DIRECTORIES:
            if any(root.rglob("*.profraw")) or any(root.rglob("*.profdata")):
                raise ValueError("cached compiler artifacts contain profiles")
        restore_stamps(key)
        print(f"Reused compiler artifacts from successful Coverage {run_id}", flush=True)
        return True
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Workspace cache miss ({type(error).__name__}); compiling normally", flush=True)
        for root in DIRECTORIES:
            shutil.rmtree(root, ignore_errors=True)
        return False
    finally:
        shutil.rmtree(INPUTS, ignore_errors=True)


def compiler_key(native_key):
    manifests = "".join(f"{path}:{digest(path)}\n" for path in sorted(tracked())
                        if path.name == "Cargo.toml" or path == Path(".github/workflows/coverage.yml"))
    return hashlib.sha256((native_key + manifests).encode()).hexdigest()


def main():
    key = compiler_key(os.environ["COVERAGE_COMPILER_KEY"])
    name = f"compiler-workspace-{key}"
    if sys.argv[1] == "pack":
        pack(key)
    elif sys.argv[1] == "restore":
        hit = restore(key, name)
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"name={name}\nhit={str(hit).lower()}\n")
    else:
        raise ValueError("expected restore or pack")


if __name__ == "__main__":
    main()
