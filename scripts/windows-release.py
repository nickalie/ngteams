#!/usr/bin/env python3
"""Stage or verify the Windows release without building, installing or starting ngteams.

The package manifest covers every shipped byte. The media manifest stays intact,
so the same runtime can be compared with the separately validated lab runtime.
Every PE (including the app, sign-in worker and CLI) participates in the normal
and delay import closure. Nothing is resolved from the developer's PATH or SDK.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import zipfile

SPEC = importlib.util.spec_from_file_location("media", Path(__file__).with_name("windows-media-closure.py"))
media = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(media)
REPO = Path(__file__).resolve().parent.parent
BINARIES = ("ngteams.exe", "ngteams-webview.exe", "ngteamsctl.exe")
# The app's main-thread stack reserve, set by crates/ngteams/build.rs.
APP_STACK = 8 * 1024 * 1024


def stack_reserve(path: Path) -> int:
    """A PE32+ image's SizeOfStackReserve: the main thread's stack."""
    with path.open("rb") as image:
        header = image.read(4096)
    if header[:2] != b"MZ":
        raise ValueError(f"{path.name} is not a PE image")
    optional = int.from_bytes(header[0x3C:0x40], "little") + 24
    if header[optional - 24:optional - 20] != b"PE\0\0" or header[optional:optional + 2] != b"\x0b\x02":
        raise ValueError(f"{path.name} is not a PE32+ image")
    if len(header) < optional + 80:
        raise ValueError(f"{path.name} has a truncated main-thread stack reserve")
    return int.from_bytes(header[optional + 72:optional + 80], "little")


def inventory(root: Path) -> dict[str, str]:
    if any(p.is_symlink() for p in root.rglob("*")):
        raise ValueError("package contains a symbolic link")
    return {p.relative_to(root).as_posix(): media.digest(p) for p in sorted(root.rglob("*"))
            if p.is_file() and p != root / "PACKAGE-MANIFEST.json"}


def metadata(version: str, commit: str) -> None:
    if not re.fullmatch(r"\d+\.\d+\.\d+\.\d+", version):
        raise ValueError("version must be MAJOR.MINOR.PATCH.RUN")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("commit must be a full Git object ID")


def check(root: Path, reader: Path, version: str | None = None, commit: str | None = None) -> None:
    manifest = json.loads((root / "PACKAGE-MANIFEST.json").read_text(encoding="utf-8"))
    metadata(manifest["version"], manifest["commit"])
    if manifest["arch"] != "x86_64" or manifest["platform"] != "windows":
        raise ValueError("wrong package platform")
    if version is not None and manifest["version"] != version:
        raise ValueError("package version mismatch")
    if commit is not None and manifest["commit"] != commit:
        raise ValueError("package commit mismatch")
    if manifest["files"] != inventory(root):
        raise ValueError("package file inventory or hashes differ")
    required = {f"bin/{name}" for name in BINARIES} | {
        "Install.ps1", "Unregister.ps1", "README.txt", "MEDIA-MANIFEST.json", "LICENSE-MIT", "LICENSE-APACHE",
        "share/licenses/ngteams/THIRD-PARTY.txt",
    }
    if not required.issubset(manifest["files"]):
        raise ValueError("package is missing an executable, setup script, licence or notice")
    for relative in manifest["files"]:
        path = PurePosixPath(relative)
        if path.is_absolute() or ".." in path.parts or "\\" in relative or ":" in relative:
            raise ValueError("invalid package path")
    runtime = json.loads((root / "MEDIA-MANIFEST.json").read_text(encoding="utf-8"))
    if runtime["plugins"] != media.plugin_names():
        raise ValueError("stale media plugin inventory")
    if any(manifest["files"].get(p) != sha for p, sha in runtime["files"].items()):
        raise ValueError("package media differs from verified runtime")
    if stack_reserve(root / "bin/ngteams.exe") < APP_STACK:
        raise ValueError("ngteams.exe reserves less than 8 MiB for its main thread's stack")
    files = {root / p: Path(p) for p in manifest["files"] if Path(p).suffix.lower() in (".exe", ".dll")}
    media.closure(files, media.index(root / "bin"), reader)
    print(f"checked Windows {manifest['version']}: {len(files)} PE files, {len(manifest['files'])} hashed files")


def stage(args: argparse.Namespace, reader: Path) -> None:
    metadata(args.version, args.commit)
    if args.output.exists() or args.archive.exists():
        raise ValueError("output or archive already exists; use a new destination")
    binaries = [args.binaries / name for name in BINARIES]
    if any(not p.is_file() for p in binaries):
        raise ValueError("missing built Windows app, worker or CLI")
    media.check(args.runtime, reader, binaries)
    # Staging and archiving coexist. Bound the worst case rather than waiting for ENOSPC.
    size = sum(p.stat().st_size for p in args.runtime.rglob("*") if p.is_file())
    size += sum(p.stat().st_size for p in binaries)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.archive.parent.mkdir(parents=True, exist_ok=True)
    if shutil.disk_usage(args.output.parent).free < 2 * size + 1536 * 1024**2:
        raise ValueError(f"packaging needs {2 * size} bytes plus the 1.5 GiB free-space guard")
    try:
        shutil.copytree(args.runtime, args.output)
        for binary in binaries:
            shutil.copy2(binary, args.output / "bin" / binary.name)
        for name in ("Install.ps1", "Unregister.ps1", "README.txt"):
            shutil.copy2(REPO / "packaging/windows" / name, args.output / name)
        for name in ("LICENSE-MIT", "LICENSE-APACHE"):
            shutil.copy2(REPO / name, args.output / name)
        notices = args.output / "share/licenses/ngteams"
        notices.mkdir(parents=True)
        shutil.copytree(args.notices, notices / "dependencies")
        shutil.copy2(REPO / "packaging/windows/THIRD-PARTY.txt", notices / "THIRD-PARTY.txt")
        manifest = {"version": args.version, "commit": args.commit, "arch": "x86_64", "platform": "windows",
                    "files": inventory(args.output)}
        (args.output / "PACKAGE-MANIFEST.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        check(args.output, reader, args.version, args.commit)
        with zipfile.ZipFile(args.archive, "w", zipfile.ZIP_DEFLATED, compresslevel=6, strict_timestamps=False) as archive:
            for path in sorted(args.output.rglob("*")):
                if path.is_file():
                    archive.write(path, "ngteams/" + path.relative_to(args.output).as_posix())
    except BaseException:
        shutil.rmtree(args.output, ignore_errors=True)
        args.archive.unlink(missing_ok=True)
        raise


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--readobj", type=Path)
    sub = parser.add_subparsers(dest="command", required=True)
    build = sub.add_parser("stage")
    for name in ("binaries", "runtime", "output", "archive", "notices"):
        build.add_argument(f"--{name}", type=Path, required=True)
    for name in ("version", "commit"):
        build.add_argument(f"--{name}", required=True)
    verify = sub.add_parser("check")
    verify.add_argument("root", type=Path)
    verify.add_argument("--version")
    verify.add_argument("--commit")
    args = parser.parse_args()
    try:
        reader = args.readobj or media.readobj_path()
        if args.command == "stage":
            stage(args, reader)
        else:
            check(args.root, reader, args.version, args.commit)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
