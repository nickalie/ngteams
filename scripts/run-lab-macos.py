#!/usr/bin/env python3
"""The macOS lab runner: the Mac's logged-in console is the macOS port's GUI place, as the win11 VM's console is
Windows', by the user's allowance (not yet written into AGENTS.md's sandbox rule, which names only the UI sandbox and
win11). Lab-marked builds only, one alias per run, short runs.

    scripts/run-lab-macos.py --build-only              lab-marked build of ngteams and ngteams-webview in target/lab
    scripts/run-lab-macos.py <alias> [--timeout 600]   run `ngteams --lab <alias>` until it quits or the deadline
    scripts/run-lab-macos.py <alias> --app <ngteams.app>   the same from a packaged lab build's bundle, on its own
                                                       GStreamer runtime (`scripts/package-macos.sh` over target/lab)
    scripts/run-lab-macos.py <alias> --shot <name>     each of the run's windows to <run>/shots/<name>-<n>.png
    scripts/run-lab-macos.py <alias> --status          whether the run is up
    scripts/run-lab-macos.py <alias> --stop            stop the run (TERM, then KILL after 10 s)

The run starts the alias's credential broker (`ngteams secrets --lab <alias>`) and the app, each with its end of one
private socket pair, as `ui-sandbox-run serve` does on Linux; the app's window is allowed on macOS only with the
runner's marker (`NGTEAMS_MACOS_CONSOLE_RUNNER`). The alias's files are `~/Library/Application Support/ngteams-lab/
<alias>/`; the run's logs and shots are in its `.runs/<alias>/` beside them. Builds need GStreamer's runtime
(`scripts/macos-gstreamer.sh`): `NGTEAMS_GSTREAMER` names its prefix (default `~/ngteams-agent/gstreamer`);
`NGTEAMS_LAB_TARGET` the target dir (default `target/lab`).

Never type credentials into a lab window from a script. Screenshots need the Screen Recording permission for the
process that runs this script (System Settings > Privacy & Security > Screen Recording).
"""

from __future__ import annotations

import argparse
import os
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LAB_HOME = Path.home() / "Library" / "Application Support" / "ngteams-lab"
MARKER = "NGTEAMS_MACOS_CONSOLE_RUNNER"
WINDOWS = """
import CoreGraphics
let wanted = Set(CommandLine.arguments.dropFirst().compactMap { Int32($0) })
let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
for window in windows {
    guard let pid = window[kCGWindowOwnerPID as String] as? Int32, wanted.contains(pid),
          (window[kCGWindowLayer as String] as? Int) == 0,
          let id = window[kCGWindowNumber as String] as? Int else { continue }
    print(id)
}
"""


def alias_ok(alias: str) -> bool:
    return bool(alias) and all(c.isascii() and (c.isalnum() or c in "_-") for c in alias)


def run_dir(alias: str) -> Path:
    path = LAB_HOME / ".runs" / alias
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    return path


def lab_target() -> Path:
    return Path(os.environ.get("NGTEAMS_LAB_TARGET", str(ROOT / "target" / "lab")))


def target() -> Path:
    return lab_target() / "debug"


def gstreamer() -> Path:
    return Path(os.environ.get("NGTEAMS_GSTREAMER", str(Path.home() / "ngteams-agent" / "gstreamer")))


def build() -> None:
    env = dict(os.environ)
    env.pop("TEAMS_PERSONAL_INSTALL", None)
    env["TEAMS_LAB_BUILD"] = "1"
    env["PKG_CONFIG_PATH"] = str(gstreamer() / "lib" / "pkgconfig")
    command = ["cargo", "build", "--locked", "-p", "ngteams", "-p", "teams-signin", "--bin", "ngteams",
               "--bin", "ngteams-webview", "--target-dir", str(lab_target())]
    subprocess.run(command, cwd=ROOT, env=env, check=True, timeout=3600)


def holder(alias: str) -> int | None:
    try:
        pid = int((run_dir(alias) / "holder.pid").read_text().split()[0])
    except (OSError, ValueError, IndexError):
        return None
    try:
        os.kill(pid, 0)
    except OSError:
        return None
    return pid


def pids(alias: str) -> list[int]:
    try:
        return [int(field) for field in (run_dir(alias) / "holder.pid").read_text().split()[1:]]
    except (OSError, ValueError):
        return []


def descendants(pid: int) -> list[int]:
    found = subprocess.run(["pgrep", "-P", str(pid)], capture_output=True, text=True, check=False).stdout.split()
    children = [int(child) for child in found]
    return children + [grandchild for child in children for grandchild in descendants(child)]


def stopped(process: subprocess.Popen, grace: float) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def serve(alias: str, timeout: int, bundle: Path | None = None) -> int:
    if holder(alias) is not None:
        print(f"lab alias {alias} is already running: --stop it first", file=sys.stderr)
        return 1
    programs = bundle / "Contents" / "MacOS" if bundle else target()
    app = programs / "ngteams"
    if not app.is_file() or not (programs / "ngteams-webview").is_file():
        print("no lab build: run scripts/run-lab-macos.py --build-only", file=sys.stderr)
        return 1
    here = run_dir(alias)
    # GStreamer's libraries are named `@rpath/...`; a development binary has no rpath to the private prefix, a bundle
    # finds its own
    libraries = {} if bundle else {"DYLD_FALLBACK_LIBRARY_PATH": str(gstreamer() / "lib")}
    broker_end, app_end = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    with open(here / "broker.log", "wb") as broker_log, open(here / "app.log", "wb") as app_log:
        broker = subprocess.Popen(
            [str(app), "secrets", "--lab", alias, "--fd", str(broker_end.fileno())],
            pass_fds=[broker_end.fileno()], env=dict(os.environ, **libraries), stdin=subprocess.DEVNULL,
            stdout=broker_log, stderr=broker_log)
        broker_end.close()
        env = dict(os.environ, **libraries, **{MARKER: "1"})
        child = subprocess.Popen(
            [str(app), "--lab", alias, "--secrets-fd", str(app_end.fileno())],
            pass_fds=[app_end.fileno()], env=env, stdin=subprocess.DEVNULL, stdout=app_log, stderr=app_log)
        app_end.close()
    (here / "holder.pid").write_text(f"{os.getpid()} {child.pid} {broker.pid}\n")
    ending = []
    signal.signal(signal.SIGTERM, lambda *_: ending.append(True))
    signal.signal(signal.SIGINT, lambda *_: ending.append(True))
    deadline = time.monotonic() + timeout
    while child.poll() is None and not ending and time.monotonic() < deadline:
        time.sleep(0.2)
    # the app's sign-in worker ends with it; anything else it started is stopped too
    leftovers = descendants(child.pid) if child.poll() is None else []
    stopped(child, 10)
    for pid in leftovers:
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
    stopped(broker, 10)
    (here / "holder.pid").unlink(missing_ok=True)
    print(f"lab alias {alias} stopped (app exit {child.returncode}); logs in {here}")
    return 0


def shot(alias: str, name: str) -> int:
    if holder(alias) is None:
        print(f"lab alias {alias} isn't running", file=sys.stderr)
        return 1
    running = pids(alias)[:1]
    owners = running + [pid for app in running for pid in descendants(app)]
    listed = subprocess.run(["xcrun", "swift", "-e", WINDOWS, *map(str, owners)], capture_output=True, text=True,
                            check=False, timeout=120)
    windows = listed.stdout.split()
    if not windows:
        print(f"no window of the run is on screen {listed.stderr.strip()}", file=sys.stderr)
        return 1
    shots = run_dir(alias) / "shots"
    shots.mkdir(exist_ok=True, mode=0o700)
    for index, window in enumerate(windows):
        path = shots / f"{name}-{index}.png"
        subprocess.run(["screencapture", "-x", "-o", f"-l{window}", str(path)], check=True, timeout=30)
        print(path)
    return 0


def stop(alias: str) -> int:
    pid = holder(alias)
    if pid is None:
        print(f"lab alias {alias} isn't running")
        return 0
    os.kill(pid, signal.SIGTERM)
    for _ in range(150):
        if holder(alias) is None:
            print(f"lab alias {alias} stopped")
            return 0
        time.sleep(0.2)
    print(f"lab alias {alias} didn't stop within 30 s (holder {pid})", file=sys.stderr)
    return 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("alias", nargs="?")
    parser.add_argument("--build-only", action="store_true")
    parser.add_argument("--timeout", type=int, default=600)
    parser.add_argument("--shot")
    parser.add_argument("--status", action="store_true")
    parser.add_argument("--stop", action="store_true")
    parser.add_argument("--app", type=Path, help="a packaged lab build's ngteams.app to run instead of target/lab")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("this runner is the macOS one: use scripts/run-lab.sh (Linux) or scripts/run-lab.ps1 (Windows)")
    if args.build_only:
        build()
        return 0
    if not args.alias or not alias_ok(args.alias):
        parser.error("give a lab alias: ASCII letters, digits, '_' and '-'")
    if args.stop:
        return stop(args.alias)
    if args.status:
        up = holder(args.alias) is not None
        print(f"lab alias {args.alias} is {'running' if up else 'stopped'}")
        return 0 if up else 1
    if args.shot:
        return shot(args.alias, args.shot)
    return serve(args.alias, args.timeout, args.app.resolve() if args.app else None)


if __name__ == "__main__":
    sys.exit(main())
