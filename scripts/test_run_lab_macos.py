#!/usr/bin/env python3
"""The macOS lab runner's bundle guard, offline: synthetic bundles only, nothing is started."""

import importlib.util
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location("run_lab_macos", Path(__file__).with_name("run-lab-macos.py"))
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


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
        for name in ("ngteams", "ngteams-webview"):
            (programs / name).touch()
        ended = mock.Mock(**{"poll.return_value": 0, "returncode": 0})
        with mock.patch.object(RUNNER, "LAB_HOME", self.root / "lab"), \
                mock.patch.object(RUNNER.subprocess, "Popen", return_value=ended) as popen, \
                mock.patch.object(RUNNER.subprocess, "run"):
            self.assertEqual(RUNNER.serve("synthetic", 1, self.root / "ngteams.app"), 0)
        app = popen.call_args_list[1].args[0]
        # teams_ctl's macOS lab path: ~/Library/Application Support/ngteams-lab/<alias>/ctl.sock
        socket = self.root / "lab" / "synthetic" / "ctl.sock"
        self.assertEqual(app[app.index("--ctl-socket") + 1], str(socket))
        self.assertEqual(socket.parent.stat().st_mode & 0o777, 0o700)
        broker = popen.call_args_list[0].args[0]
        self.assertNotIn("--ctl-socket", broker)


if __name__ == "__main__":
    unittest.main()
