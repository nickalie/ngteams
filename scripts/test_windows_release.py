"""Windows package boundary tests with real x64 PE normal/delay imports, no app or tenant."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import unittest
from unittest.mock import patch
import zipfile

import test_windows_media_closure as pe

SPEC = importlib.util.spec_from_file_location("release", Path(__file__).with_name("windows-release.py"))
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


@unittest.skipUnless(shutil.which("clang") and shutil.which("rustc"), "PE fixtures need clang and Rust LLD")
class WindowsRelease(unittest.TestCase):
    dll = pe.WindowsMediaClosure.dll
    # Reuse the real PE constructor, not the media suite's test inventory.
    def setUp(self):
        pe.WindowsMediaClosure.setUp(self)
        self.runtime = self.root / "runtime"
        (self.runtime / "bin").mkdir(parents=True)
        for name in ("Leaf.dll", "Middle.dll"):
            shutil.copy2(self.root / name, self.runtime / "bin" / name)
        manifest = {"plugins": release.media.plugin_names(), "system_imports": [],
                    "files": {f"bin/{name}": release.media.digest(self.runtime / "bin" / name)
                              for name in ("Leaf.dll", "Middle.dll")}}
        (self.runtime / "MEDIA-MANIFEST.json").write_text(json.dumps(manifest))
        self.binaries = self.root / "binaries"
        self.binaries.mkdir()
        for name in release.BINARIES:
            shutil.copy2(self.root / "Top.dll", self.binaries / name)
        self.dll("App", "__declspec(dllimport) int middle(void); __declspec(dllexport) int top(void) { return middle(); }"
                 "void *__delayLoadHelper2(void *a, void *b) { return 0; }", "Middle.lib", "/delayload:Middle.dll",
                 f"/stack:{release.APP_STACK}")
        shutil.copy2(self.root / "App.dll", self.binaries / "ngteams.exe")
        self.notices = self.root / "notices"
        self.notices.mkdir()
        (self.notices / "license.txt").write_text("Synthetic package license fixture\n")
        self.args = argparse.Namespace(version="0.1.0.123", commit="a" * 40, runtime=self.runtime,
            binaries=self.binaries, output=self.root / "package", archive=self.root / "app.zip", notices=self.notices)

    def test_archive_contains_the_verified_tree_and_setup(self):
        # Cargo registry notice files can have the reproducible Unix epoch timestamp.
        os.utime(self.notices / "license.txt", (0, 0))
        release.stage(self.args, self.reader)
        release.check(self.args.output, self.reader, self.args.version, self.args.commit)
        with zipfile.ZipFile(self.args.archive) as archive:
            names = set(archive.namelist())
            self.assertIn("ngteams/bin/ngteams-webview.exe", names)
            self.assertIn("ngteams/Install.ps1", names)
            self.assertIn("ngteams/LICENSE-MIT", names)
            self.assertIn("ngteams/LICENSE-APACHE", names)
            self.assertIn("ngteams/share/licenses/ngteams/dependencies/license.txt", names)
            for path in self.args.output.rglob("*"):
                if path.is_file():
                    self.assertEqual(archive.read("ngteams/" + path.relative_to(self.args.output).as_posix()), path.read_bytes())
        for name, value in (("version", "0.1.0.124"), ("commit", "b" * 40)):
            with self.assertRaisesRegex(ValueError, f"{name} mismatch"):
                release.check(self.args.output, self.reader, **{name: value})
        (self.args.output / "bin/ngteams-webview.exe").write_bytes(b"corrupt")
        with self.assertRaisesRegex(ValueError, "inventory or hashes"):
            release.check(self.args.output, self.reader)

    def test_missing_app_or_transitive_import_refuses_public_archive(self):
        (self.binaries / "ngteams-webview.exe").unlink()
        with self.assertRaisesRegex(ValueError, "missing built"):
            release.stage(self.args, self.reader)
        self.assertFalse(self.args.archive.exists())
        shutil.copy2(self.root / "Top.dll", self.binaries / "ngteams-webview.exe")
        # The runtime checks on its own, but the extra application needs Middle.dll.
        (self.runtime / "bin/Middle.dll").unlink()
        manifest = json.loads((self.runtime / "MEDIA-MANIFEST.json").read_text())
        del manifest["files"]["bin/Middle.dll"]
        (self.runtime / "MEDIA-MANIFEST.json").write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "unresolved dependency middle.dll"):
            release.stage(self.args, self.reader)
        self.assertFalse(self.args.archive.exists())
        self.assertFalse(self.args.output.exists())

    def test_an_app_with_the_default_1_mib_main_stack_refuses_public_archive(self):
        self.assertEqual(release.stack_reserve(self.binaries / "ngteams.exe"), release.APP_STACK)
        self.assertEqual(release.stack_reserve(self.root / "Top.dll"), 1024 * 1024)
        shutil.copy2(self.root / "Top.dll", self.binaries / "ngteams.exe")
        with self.assertRaisesRegex(ValueError, "less than 8 MiB"):
            release.stage(self.args, self.reader)
        self.assertFalse(self.args.archive.exists())
        self.assertFalse(self.args.output.exists())

    def test_low_space_and_invalid_metadata_refuse_before_copy(self):
        with patch.object(release.shutil, "disk_usage", return_value=shutil._ntuple_diskusage(100, 99, 1)):
            with self.assertRaisesRegex(ValueError, "1.5 GiB"):
                release.stage(self.args, self.reader)
        self.assertFalse(self.args.output.exists())
        self.args.version = "../../invalid"
        with self.assertRaisesRegex(ValueError, "version must"):
            release.stage(self.args, self.reader)

    def test_the_installer_knows_every_top_level_name_of_the_stage(self):
        release.stage(self.args, self.reader)
        names = {path.name for path in self.args.output.iterdir()}
        self.assertLessEqual(names, set(installer_names()))

    def test_existing_output_is_never_overwritten(self):
        self.args.archive.write_text("keep")
        with self.assertRaisesRegex(ValueError, "already exists"):
            release.stage(self.args, self.reader)
        self.assertEqual(self.args.archive.read_text(), "keep")


REPO = Path(__file__).resolve().parent.parent
SCRIPT = (REPO / "packaging/windows/ngteams.iss").read_text(encoding="utf-8").replace("\r\n", "\n")


def installer_names():
    """The stage's top-level names scripts/package-release-windows.ps1 accepts for the installer."""
    text = (REPO / "scripts/package-release-windows.ps1").read_text(encoding="utf-8")
    listed = re.search(r"\$installerNames = @\((.*?)\)", text, re.S).group(1)
    return re.findall(r"'([^']+)'", listed)


def iss_define(name):
    return re.search(rf'^#define {name} "([^"]*)"', SCRIPT, re.M).group(1)


def iss_setup(name):
    return re.search(rf"^{name}=(.*)$", SCRIPT, re.M).group(1)


class WindowsInstallerScript(unittest.TestCase):
    """packaging/windows/ngteams.iss agrees with the packaging script, the app and the updater."""

    def test_an_upgrade_replaces_exactly_what_the_installer_ships(self):
        excluded = re.search(r'Excludes: "([^"]*)"', SCRIPT).group(1).replace("\\", "").split(",")
        replaced = iss_define("ProgramItems").split("|")
        self.assertEqual(sorted(replaced + excluded), sorted(installer_names()))
        # nothing is deleted before the previous version is safely aside
        self.assertNotIn("[InstallDelete]", SCRIPT)
        self.assertEqual(set(excluded), {"Install.ps1", "Unregister.ps1", "README.txt", "PACKAGE-MANIFEST.json"})

    def test_its_names_and_ids_are_the_apps(self):
        digest = hashlib.sha256(b"ngteams.toast.activator:com.nickalie.ngteams").hexdigest()[:32].upper()
        guid = f"{digest[:8]}-{digest[8:12]}-{digest[12:16]}-{digest[16:20]}-{digest[20:]}"
        self.assertEqual(iss_define("ToastActivator"), guid)
        self.assertEqual(iss_define("AppUserModelId"), "com.nickalie.ngteams")
        for key in ("AppName", "AppPublisher", "UninstallDisplayName", "VersionInfoProductName"):
            self.assertEqual(iss_setup(key), "ngteams")
        self.assertNotRegex(SCRIPT.split("[Code]")[0].replace("(Microsoft Teams, say)", ""), "Microsoft Teams|MS Teams")
        # the asset name the updater asks for (crates/teams-update Format::WindowsSetup)
        self.assertEqual(iss_setup("OutputBaseFilename"), "ngteams-{#AppVersion}-windows-x86_64-setup")
        updater = (REPO / "crates/teams-update/src/lib.rs").read_text(encoding="utf-8")
        self.assertIn('format!("ngteams-{version}-windows-x86_64-setup.exe")', updater)
        self.assertIn('"windows-setup" => Some(Self::WindowsSetup)', updater)

    def test_the_compiler_is_pinned_by_version_and_hash(self):
        pins = json.loads((REPO / "packaging/windows/installer-pins.json").read_text(encoding="utf-8"))
        self.assertRegex(pins["innosetup_sha256"], "^[0-9a-f]{64}$")
        tag = "is-" + pins["innosetup_version"].replace(".", "_")
        self.assertEqual(pins["innosetup_url"], "https://github.com/jrsoftware/issrc/releases/download/"
                         f"{tag}/innosetup-{pins['innosetup_version']}-x64.exe")


# Importing the shared test class would discover its tests twice. Select only this suite's new contracts; its
# inherited constructors/support remain available, and the media suite runs separately in the workflow.
if __name__ == "__main__":
    suite = unittest.TestSuite(case(name) for case in (WindowsRelease, WindowsInstallerScript)
                               for name in case.__dict__ if name.startswith("test_"))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(not result.wasSuccessful())
