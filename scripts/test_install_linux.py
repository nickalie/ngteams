"""packaging/linux/install.sh: a release's tree installed for one user, with no root, on a temporary HOME."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "packaging/linux/install.sh"
APP_ID = "com.nickalie.ngteams"
COMPLETIONS = (
    "bash-completion/completions/ngteamsctl",
    "fish/vendor_completions.d/ngteamsctl.fish",
    "zsh/site-functions/_ngteamsctl",
)


def tree(root: Path, version: str, marker: str) -> Path:
    """A release tree as scripts/package-release.sh lays it out, its programs small scripts, with the installer."""
    (root / "usr/bin").mkdir(parents=True)
    for name in ("ngteams", "ngteamsctl", "ngteams-web"):
        program = root / "usr/bin" / name
        program.write_text(f"#!/bin/sh\necho {name} {version}\n")
        program.chmod(0o755)
    (root / "AppRun").write_text('#!/bin/sh\nexec "$(dirname "$0")/usr/bin/ngteams" "$@"\n')
    (root / "AppRun").chmod(0o755)
    (root / "usr/share/ngteams").mkdir(parents=True)
    (root / "usr/share/ngteams/release").write_text(f"{version}\n")
    (root / "usr/share/ngteams/update-format").write_text(f"{marker}\n")
    (root / "usr/share/applications").mkdir(parents=True)
    (root / f"usr/share/applications/{APP_ID}.desktop").write_bytes((REPO / f"packaging/{APP_ID}.desktop").read_bytes())
    icon = root / f"usr/share/icons/hicolor/256x256/apps/{APP_ID}.png"
    icon.parent.mkdir(parents=True)
    icon.write_bytes(b"synthetic icon")
    for completion in COMPLETIONS:
        path = root / "usr/share" / completion
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"# ngteamsctl completion {version}\n")
    installer = root / "install.sh"
    installer.write_bytes(SCRIPT.read_bytes())
    installer.chmod(0o755)
    return root


class InstallLinux(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.temp = Path(directory.name)
        self.home = self.temp / "home"
        self.home.mkdir()
        self.target = self.home / ".local/opt/ngteams"
        self.data = self.home / ".local/share"

    def run_script(self, installer: Path, *args: str) -> subprocess.CompletedProcess:
        env = {
            "HOME": str(self.home),
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        }
        return subprocess.run(
            ["bash", str(installer), *args], env=env, text=True, capture_output=True, timeout=60, check=False
        )

    def installed(self, installer: Path) -> subprocess.CompletedProcess:
        result = self.run_script(installer)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def test_an_unpacked_archive_goes_to_the_users_own_folder_with_its_menu_entry(self) -> None:
        unpacked = tree(self.temp / "Downloads/ngteams", "0.1.0.7", "linux-archive")
        result = self.installed(unpacked / "install.sh")
        self.assertIn(str(self.target), result.stdout)
        self.assertEqual((self.target / "usr/share/ngteams/release").read_text(), "0.1.0.7\n")
        self.assertEqual((self.target / "usr/share/ngteams/update-format").read_text(), "linux-archive\n")
        # the folder the update replaces is the user's own, and so is the folder beside it, where its stage goes
        self.assertTrue(os.access(self.target.parent, os.W_OK))
        entry = (self.data / f"applications/{APP_ID}.desktop").read_text()
        self.assertIn(f'Exec="{self.target}/AppRun" %u\n', entry)
        self.assertIn("MimeType=x-scheme-handler/msteams;", entry)
        self.assertNotIn("Exec=ngteams", entry)
        self.assertEqual((self.data / f"icons/hicolor/256x256/apps/{APP_ID}.png").read_bytes(), b"synthetic icon")
        for name in ("ngteams", "ngteamsctl"):
            link = self.home / ".local/bin" / name
            self.assertEqual(os.readlink(link), str(self.target / "usr/bin" / name))
        for completion in COMPLETIONS:
            self.assertEqual(os.readlink(self.data / completion), str(self.target / "usr/share" / completion))
        # what was unpacked stays where it was
        self.assertTrue((unpacked / "usr/bin/ngteams").is_file())

    def test_a_system_package_or_an_extracted_appimage_becomes_a_per_user_copy(self) -> None:
        for marker in ("linux-pacman", "linux-deb", "linux-rpm", "linux-appimage"):
            with self.subTest(marker=marker):
                source = tree(self.temp / marker / "ngteams", "0.1.0.7", marker)
                self.installed(source / "install.sh")
                self.assertEqual((self.target / "usr/share/ngteams/update-format").read_text(), "linux-archive\n")
                self.assertEqual((source / "usr/share/ngteams/update-format").read_text(), f"{marker}\n")

    def test_a_newer_release_replaces_the_per_user_copy_and_a_second_run_changes_nothing(self) -> None:
        self.installed(tree(self.temp / "old/ngteams", "0.1.0.5", "linux-archive") / "install.sh")
        (self.target / "usr/bin/stale-from-0.1.0.5").write_text("old")
        self.installed(tree(self.temp / "new/ngteams", "0.1.0.7", "linux-archive") / "install.sh")
        self.assertEqual((self.target / "usr/share/ngteams/release").read_text(), "0.1.0.7\n")
        self.assertFalse((self.target / "usr/bin/stale-from-0.1.0.5").exists())
        # run from the installed copy itself: the entries are written again, the copy is kept
        self.installed(self.target / "install.sh")
        self.assertEqual((self.target / "usr/share/ngteams/release").read_text(), "0.1.0.7\n")
        self.assertEqual(
            sorted(path.name for path in self.target.parent.iterdir()),
            ["ngteams"],
            "no temporary folder is left beside the copy",
        )

    def test_a_folder_that_is_not_ngteams_is_left_alone(self) -> None:
        self.target.mkdir(parents=True)
        (self.target / "notes.txt").write_text("someone else's")
        result = self.run_script(tree(self.temp / "ngteams", "0.1.0.7", "linux-archive") / "install.sh")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not an ngteams release", result.stderr)
        self.assertEqual((self.target / "notes.txt").read_text(), "someone else's")

    def test_uninstall_removes_the_copy_and_its_entries_and_keeps_the_data(self) -> None:
        self.installed(tree(self.temp / "ngteams", "0.1.0.7", "linux-archive") / "install.sh")
        accounts = self.data / "ngteams/accounts.toml"
        accounts.parent.mkdir(parents=True)
        accounts.write_text("synthetic")
        other = self.home / ".local/bin/other"
        other.symlink_to("/bin/true")
        result = self.run_script(self.target / "install.sh", "--uninstall")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.target.exists())
        self.assertFalse((self.data / f"applications/{APP_ID}.desktop").exists())
        self.assertFalse((self.data / f"icons/hicolor/256x256/apps/{APP_ID}.png").exists())
        for name in ("ngteams", "ngteamsctl"):
            self.assertFalse((self.home / ".local/bin" / name).is_symlink())
        for completion in COMPLETIONS:
            self.assertFalse((self.data / completion).is_symlink())
        self.assertEqual(accounts.read_text(), "synthetic")
        self.assertTrue(other.is_symlink())

    def test_links_to_another_build_are_replaced_only_on_install_and_kept_on_uninstall(self) -> None:
        source_build = self.home / ".local/lib/ngteams/ngteams"
        source_build.parent.mkdir(parents=True)
        source_build.write_text("#!/bin/sh\n")
        link = self.home / ".local/bin/ngteams"
        link.parent.mkdir(parents=True)
        link.symlink_to(source_build)
        self.installed(tree(self.temp / "ngteams", "0.1.0.7", "linux-archive") / "install.sh")
        self.assertEqual(os.readlink(link), str(self.target / "usr/bin/ngteams"))
        link.unlink()
        link.symlink_to(source_build)
        result = self.run_script(self.target / "install.sh", "--uninstall")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(os.readlink(link), str(source_build))


if __name__ == "__main__":
    unittest.main()
