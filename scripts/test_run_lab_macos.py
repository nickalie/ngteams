#!/usr/bin/env python3
"""The macOS lab runner's guards, offline: synthetic bundles and programs only, nothing is started or built."""

import importlib.util
import os
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("run_lab_macos", Path(__file__).with_name("run-lab-macos.py"))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


def program(path, magic=b"\xcf\xfa\xed\xfe", size=RUNNER.MIN_PROGRAM):
    """A synthetic program: `magic`, then zeros to `size` bytes (sparse), executable."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "wb") as file:
        file.write(magic)
        file.truncate(size)
    path.chmod(0o755)
    return path


class LabBundleTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def bundle(self, info):
        bundle = self.root / "ngteams.app"
        (bundle / "Contents" / "MacOS").mkdir(parents=True, exist_ok=True)
        if info is not None:
            with open(bundle / "Contents" / "Info.plist", "wb") as file:
                plistlib.dump(info, file)
        return bundle

    def test_a_lab_bundle_has_its_own_id_and_no_url_scheme(self):
        self.assertIsNone(RUNNER.lab_bundle(self.bundle({"CFBundleIdentifier": "com.nickalie.ngteams.lab"})))

    def test_the_personal_id_a_url_scheme_or_no_info_is_refused(self):
        msteams = [{"CFBundleURLSchemes": ["msteams"]}]
        for info, reason in [
            ({"CFBundleIdentifier": "com.nickalie.ngteams", "CFBundleURLTypes": msteams}, "not a lab bundle"),
            ({"CFBundleIdentifier": "com.nickalie.ngteams"}, "not a lab bundle"),
            ({"CFBundleIdentifier": "com.nickalie.ngteams.lab", "CFBundleURLTypes": msteams}, "URL scheme"),
            (None, "no readable"),
        ]:
            with self.subTest(info=info):
                (self.root / "ngteams.app" / "Contents" / "Info.plist").unlink(missing_ok=True)
                self.assertIn(reason, RUNNER.lab_bundle(self.bundle(info)))

    def test_a_run_from_a_personal_bundle_starts_nothing(self):
        bundle = self.bundle({"CFBundleIdentifier": "com.nickalie.ngteams"})
        with mock.patch.object(RUNNER, "LAB_HOME", self.root / "lab"), \
                mock.patch.object(RUNNER.subprocess, "Popen") as popen:
            self.assertEqual(RUNNER.serve("synthetic", 1, bundle), 1)
        popen.assert_not_called()

    def test_the_app_serves_its_control_socket_where_ngteamsctl_looks(self):
        programs = self.bundle({"CFBundleIdentifier": "com.nickalie.ngteams.lab"}) / "Contents" / "MacOS"
        for name in RUNNER.PROGRAMS:
            program(programs / name)
        ended = mock.Mock(**{"poll.return_value": 0, "returncode": 0})
        with mock.patch.object(RUNNER, "LAB_HOME", self.root / "lab"), \
                mock.patch.object(RUNNER.subprocess, "Popen", return_value=ended) as popen, \
                mock.patch.object(RUNNER.subprocess, "run") as run:
            self.assertEqual(RUNNER.serve("synthetic", 1, self.root / "ngteams.app"), 0)
        app = popen.call_args_list[1].args[0]
        # teams_ctl's macOS lab path: ~/Library/Application Support/ngteams-lab/<alias>/ctl.sock
        socket = self.root / "lab" / "synthetic" / "ctl.sock"
        self.assertEqual(app[app.index("--ctl-socket") + 1], str(socket))
        self.assertEqual(socket.parent.stat().st_mode & 0o777, 0o700)
        broker = popen.call_args_list[0].args[0]
        self.assertNotIn("--ctl-socket", broker)
        # the lab bundle's window animations off for the run (a locked console's window list), its setting back after
        commands = [call.args[0] for call in run.call_args_list]
        self.assertEqual(commands[0], ["defaults", "write", "com.nickalie.ngteams.lab",
                                       "NSAutomaticWindowAnimationsEnabled", "-bool", "false"])
        self.assertIn(["defaults", "delete", "com.nickalie.ngteams.lab", "NSAutomaticWindowAnimationsEnabled"],
                      commands[1:])

    def test_a_broken_program_in_the_bundle_starts_nothing(self):
        programs = self.bundle({"CFBundleIdentifier": "com.nickalie.ngteams.lab"}) / "Contents" / "MacOS"
        program(programs / "ngteams", magic=b"!<ar")
        program(programs / "ngteams-webview")
        with mock.patch.object(RUNNER, "LAB_HOME", self.root / "lab"), \
                mock.patch.object(RUNNER.subprocess, "Popen") as popen, \
                mock.patch.object(RUNNER.subprocess, "run") as run:
            self.assertEqual(RUNNER.serve("synthetic", 1, self.root / "ngteams.app"), 1)
        popen.assert_not_called()
        run.assert_not_called()


class BuiltProgramTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def test_only_an_executable_mach_o_of_a_sane_size_is_a_program(self):
        self.assertIsNone(RUNNER.broken_program(program(self.root / "thin")))
        self.assertIsNone(RUNNER.broken_program(program(self.root / "universal", magic=b"\xca\xfe\xba\xbe")))
        # what a strip cut short by a full disk left: 2.5 KB, not a Mach-O
        self.assertIn("not a Mach-O", RUNNER.broken_program(program(self.root / "cut", b"\x00" * 4, 2560)))
        self.assertIn("too small", RUNNER.broken_program(program(self.root / "small", size=2560)))
        self.assertIn("missing", RUNNER.broken_program(self.root / "none"))
        plain = program(self.root / "plain")
        plain.chmod(0o644)
        self.assertIn("not executable", RUNNER.broken_program(plain))

    def built(self, broken):
        debug = self.root / "lab" / "debug"
        for name in RUNNER.PROGRAMS:
            if name == broken:
                program(debug / name, b"\x00" * 4, 2560)
            else:
                program(debug / name)
            program(debug / "deps" / f"{name.replace('-', '_')}-0123456789abcdef")
            (debug / "deps" / f"{name.replace('-', '_')}-0123456789abcdef.dSYM").mkdir()
        program(debug / "deps" / "ngteamsctl-0123456789abcdef")
        with mock.patch.dict(os.environ, {"NGTEAMS_LAB_TARGET": str(self.root / "lab")}), \
                mock.patch.object(RUNNER.subprocess, "run") as run:
            code = RUNNER.build()
        run.assert_called_once()
        return code, debug

    def test_a_build_that_left_a_broken_program_fails_and_removes_it_for_a_new_link(self):
        code, debug = self.built("ngteams")
        self.assertEqual(code, 1)
        self.assertFalse((debug / "ngteams").exists())
        self.assertEqual(sorted(path.name for path in (debug / "deps").iterdir()),
                         ["ngteams_webview-0123456789abcdef", "ngteams_webview-0123456789abcdef.dSYM",
                          "ngteamsctl-0123456789abcdef"])
        self.assertIsNone(RUNNER.broken_program(debug / "ngteams-webview"))

    def test_a_whole_build_passes(self):
        code, debug = self.built(None)
        self.assertEqual(code, 0)
        self.assertEqual(len(list((debug / "deps").iterdir())), 5)


if __name__ == "__main__":
    unittest.main()
