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


if __name__ == "__main__":
    unittest.main()
