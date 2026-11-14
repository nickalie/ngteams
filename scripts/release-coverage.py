#!/usr/bin/env python3
"""Require the newest standalone Coverage run for this exact repository, branch and commit."""

import argparse
import json
import os
import subprocess
import sys
import time


def matching_runs(runs, repository, sha, branch):
    return sorted((run for run in runs if
                   run["head_sha"] == sha and run["head_branch"] == branch and
                   run["head_repository"]["full_name"].lower() == repository.lower() and
                   run["event"] in ("push", "workflow_dispatch")),
                  key=lambda run: (run["run_number"], run["run_attempt"]), reverse=True)


def wait_for_coverage(repository, sha, branch, timeout, interval=30):
    deadline = time.monotonic() + timeout
    while True:
        result = subprocess.run([
            "gh", "api", "--method", "GET", "--paginate", "--slurp",
            f"repos/{repository}/actions/workflows/coverage.yml/runs",
            "-f", f"head_sha={sha}", "-f", "per_page=100",
        ], capture_output=True, text=True, check=True)
        runs = matching_runs([run for page in json.loads(result.stdout) for run in page["workflow_runs"]],
                             repository, sha, branch)
        if runs:
            run = runs[0]
            print(f"Coverage {run['html_url']}: {run['status']} / {run['conclusion']}", flush=True)
            if run["status"] == "completed":
                if run["conclusion"] != "success":
                    raise ValueError(f"Coverage did not pass for {sha}: {run['conclusion']}")
                summary = os.environ.get("GITHUB_STEP_SUMMARY")
                if summary:
                    with open(summary, "a") as output:
                        output.write(f"Reused [Coverage]({run['html_url']}) for `{sha}`.\n")
                return
        else:
            print(f"Waiting for Coverage on {branch} at {sha}", flush=True)
        if time.monotonic() >= deadline:
            raise ValueError(f"no successful completed Coverage run before timeout for {sha}")
        time.sleep(min(interval, max(0, deadline - time.monotonic())))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY"), required=False)
    parser.add_argument("--sha", default=os.environ.get("GITHUB_SHA"), required=False)
    parser.add_argument("--branch", default=os.environ.get("GITHUB_REF_NAME"), required=False)
    parser.add_argument("--timeout", type=int, default=9000)
    args = parser.parse_args()
    if not all((args.repository, args.sha, args.branch)) or args.timeout < 0:
        parser.error("repository, sha, branch and a nonnegative timeout are required")
    wait_for_coverage(args.repository, args.sha, args.branch, args.timeout)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"release coverage gate: {error}")
