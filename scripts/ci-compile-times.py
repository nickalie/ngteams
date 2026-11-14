#!/usr/bin/env python3
"""Measure fresh Cargo units from invocation/completion stamps without changing compiler flags."""

import argparse
import json
import os
from pathlib import Path
import tomllib


ROOT = Path(__file__).resolve().parent.parent


def collect(target, started, workspace):
    units = []
    for stamp in sorted((target / "debug/.fingerprint").glob("*/invoked.timestamp")):
        markers = list(stamp.parent.glob("*.json"))
        if len(markers) != 1:
            continue
        begin, end = stamp.stat().st_mtime, markers[0].stat().st_mtime
        package = stamp.parent.name.rsplit("-", 1)[0]
        fresh = begin >= started
        if fresh and end < begin:
            continue
        units.append({
            "package": package, "unit": markers[0].stem,
            "category": "workspace" if package in workspace else "dependencies",
            "fresh": fresh, "started": begin,
            "seconds": round(end - begin, 3) if fresh else None,
        })
    summary = {}
    for category in ("dependencies", "workspace"):
        selected = [unit for unit in units if unit["category"] == category]
        fresh = [unit for unit in selected if unit["fresh"]]
        summary[category] = {
            "fresh_units": len(fresh), "cached_units": len(selected) - len(fresh),
            "unit_seconds": round(sum(unit["seconds"] for unit in fresh), 3),
        }
    fresh = [unit for unit in units if unit["fresh"]]
    span = (max(unit["started"] + unit["seconds"] for unit in fresh)
            - min(unit["started"] for unit in fresh)) if fresh else 0
    return {"summary": summary, "compiler_span_seconds": round(span, 3), "units": units}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", type=Path, required=True)
    parser.add_argument("--started", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    members = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["members"]
    workspace = {tomllib.loads((ROOT / member / "Cargo.toml").read_text())["package"]["name"]
                 for member in members}
    data = collect(args.target, float(args.started.read_text()), workspace)
    data["runner"] = {"logical_cpus": os.cpu_count(),
                      "memory_bytes": os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")}
    args.output.write_text(json.dumps(data, indent=2) + "\n")
    print(f"Runner: {data['runner']['logical_cpus']} logical CPUs, "
          f"{data['runner']['memory_bytes'] / 2**30:.1f} GiB RAM")
    print("Compile unit durations overlap; their sum is not workflow wall time.")
    for category, counts in data["summary"].items():
        print(f"{category}: {counts['fresh_units']} fresh, {counts['cached_units']} cached, "
              f"{counts['unit_seconds']:.3f} aggregate unit seconds")
    print(f"Instrumented compiler span: {data['compiler_span_seconds']:.3f} seconds")


if __name__ == "__main__":
    main()
