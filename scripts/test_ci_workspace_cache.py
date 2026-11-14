"""Check artifact provenance, clean profiles, and Cargo freshness after a content-checked restore."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("workspace_cache", ROOT / ".github/ci/workspace-cache.py")
CACHE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CACHE)


class ProvenanceTests(unittest.TestCase):
    @staticmethod
    def run_record(number=1, **changes):
        return {"id": number, "run_number": number, "run_attempt": 1, "status": "completed",
                "conclusion": "success", "head_branch": "main", "event": "push",
                "head_repository": {"full_name": "owner/repo"}, **changes}

    def test_only_a_complete_successful_same_branch_build_is_reused(self):
        names = {"compiler-key", "coverage-common", *(f"coverage-shard-{i}" for i in range(8))}
        artifacts = [{"name": name, "expired": False} for name in sorted(names)]
        incompatible = [self.run_record(20, head_branch="other"), self.run_record(19, conclusion="failure"),
                        self.run_record(18, status="in_progress"), self.run_record(17, event="pull_request"),
                        self.run_record(16, head_repository={"full_name": "other/repo"})]

        def api(path, *arguments):
            if path.endswith("/runs"):
                return {"workflow_runs": [*incompatible, self.run_record(2), self.run_record()]}
            self.assertIn("/runs/2/", path)
            return {"artifacts": artifacts}

        with patch.object(CACHE, "api", side_effect=api):
            self.assertEqual(CACHE.find_run("owner/repo", "main", "compiler-key"), (2, names))
        for broken in [[artifact for artifact in artifacts if artifact["name"] != "coverage-shard-7"],
                       [dict(artifact, expired=artifact["name"] == "compiler-key") for artifact in artifacts]]:
            with patch.object(CACHE, "api", side_effect=[{"workflow_runs": [self.run_record()]},
                                                        {"artifacts": broken}]):
                self.assertIsNone(CACHE.find_run("owner/repo", "main", "compiler-key"))

    def test_a_partial_or_profile_bearing_restore_is_discarded(self):
        for with_profile in [False, True]:
            with self.subTest(profile=with_profile), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                with contextlib.chdir(root):
                    cache = CACHE.DIRECTORIES[0]
                    cache.mkdir(parents=True)
                    (cache / "partial-output").write_text("synthetic compiler artifact\n")
                    if with_profile:
                        (cache / "old.profraw").write_text("synthetic old counters\n")
                    failed = subprocess.CalledProcessError(1, ["gh"])
                    with patch.dict(os.environ, {"GITHUB_REPOSITORY": "owner/repo", "GITHUB_REF_NAME": "main"}), \
                            patch.object(CACHE, "find_run", return_value=(1, ["compiler-key"])), \
                            patch.object(CACHE.subprocess, "run", side_effect=None if with_profile else failed), \
                            contextlib.redirect_stdout(io.StringIO()):
                        self.assertFalse(CACHE.restore("key", "compiler-key"))
                    self.assertFalse(cache.exists())
                    self.assertFalse(CACHE.INPUTS.exists())


@unittest.skipUnless(shutil.which("git") and shutil.which("zstd"), "Git or zstd unavailable")
class CompilerStateTests(unittest.TestCase):
    @staticmethod
    def command(root, *arguments, env=None):
        return subprocess.run(arguments, cwd=root, env=env, capture_output=True, text=True, check=True).stdout

    def initialize(self, root):
        self.command(root, "git", "init", "--quiet")
        (root / ".gitignore").write_text("/target/\n/.ci-cache/\n")

    def test_metadata_archive_excludes_profiles_and_separately_bundled_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.initialize(root)
            (root / "source.rs").write_text("synthetic source\n")
            self.command(root, "git", "add", ".")
            build = root / CACHE.DIRECTORIES[0]
            build.mkdir(parents=True)
            for name in ["counter.profraw", "merged.profdata", "test-executable", "fingerprint"]:
                (build / name).write_text("synthetic compiler input\n")
            (root / "target/bundle-common-files").write_text(str(CACHE.DIRECTORIES[0] / "test-executable") + "\n")
            with contextlib.chdir(root), contextlib.redirect_stdout(io.StringIO()):
                CACHE.pack("key")
            listing = self.command(root, "tar", "-I", "zstd --long=30", "-tf", "target/compiler-workspace.tar.zst")
            self.assertNotIn(".profraw", listing)
            self.assertNotIn(".profdata", listing)
            self.assertNotIn("test-executable", listing)
            self.assertIn("fingerprint", listing)
            with contextlib.chdir(root):
                with self.assertRaisesRegex(ValueError, "incompatible"):
                    CACHE.restore_stamps("other-key")

    def test_native_configuration_manifests_and_workflow_bound_the_cache_key(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.initialize(root)
            (root / "Cargo.toml").write_text("synthetic manifest\n")
            workflow = root / ".github/workflows/coverage.yml"
            workflow.parent.mkdir(parents=True)
            workflow.write_text("synthetic workflow\n")
            (root / "source.rs").write_text("synthetic source\n")
            self.command(root, "git", "add", ".")
            with contextlib.chdir(root):
                original = CACHE.compiler_key("native-image-and-flags")
                (root / "source.rs").write_text("synthetic changed source\n")
                self.assertEqual(CACHE.compiler_key("native-image-and-flags"), original)
                self.assertNotEqual(CACHE.compiler_key("different-native-image"), original)
                workflow.write_text("synthetic different workflow\n")
                self.assertNotEqual(CACHE.compiler_key("native-image-and-flags"), original)
                workflow.write_text("synthetic workflow\n")
                (root / "Cargo.toml").write_text("synthetic different features\n")
                self.assertNotEqual(CACHE.compiler_key("native-image-and-flags"), original)

    @unittest.skipUnless(shutil.which("cargo"), "Cargo unavailable")
    def test_restored_unchanged_crate_stays_fresh_and_changed_crate_rebuilds_with_new_behavior(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.initialize(root)
            (root / "Cargo.toml").write_text('[workspace]\nmembers=["first", "second"]\nresolver="3"\n')
            for name, value, expected in [("first", 1, 3), ("second", 2, 2)]:
                crate = root / name
                (crate / "src").mkdir(parents=True)
                (crate / "Cargo.toml").write_text(f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2024"\n')
                (crate / "src/lib.rs").write_text(f"pub fn value() -> u32 {{ {value} }}\n")
                (crate / "tests").mkdir()
                (crate / "tests/value.rs").write_text(
                    f"#[test] fn current_value() {{ assert_eq!({name}::value(), {expected}); }}\n")
            environment = dict(os.environ)
            for name in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_TARGET_DIR"]:
                environment.pop(name, None)
            environment.update(CARGO_TARGET_DIR=str(root / CACHE.DIRECTORIES[0]), CARGO_INCREMENTAL="0")
            cargo = ["cargo", "test", "--workspace", "--offline", "--no-run", "--message-format=json"]
            self.command(root, *cargo, env=environment)
            self.command(root, "git", "add", ".")
            with contextlib.chdir(root), contextlib.redirect_stdout(io.StringIO()):
                CACHE.pack("key")
                shutil.rmtree(CACHE.DIRECTORIES[0])
                for source in CACHE.tracked():
                    os.utime(source, None)
                (root / "first/src/lib.rs").write_text("pub fn value() -> u32 { 3 }\n")
                self.command(root, "tar", "-I", "zstd --long=30", "-xf", "target/compiler-workspace.tar.zst")
                CACHE.restore_stamps("key")
            rebuilt = [json.loads(line) for line in self.command(root, *cargo, env=environment).splitlines()]
            units = [message for message in rebuilt if message["reason"] == "compiler-artifact"]
            for message in units:
                self.assertEqual(message["fresh"], Path(message["manifest_path"]).parent.name == "second")
            self.assertEqual({message["target"]["name"] for message in units}, {"first", "second", "value"})
            self.command(root, "cargo", "test", "--workspace", "--offline", env=environment)
