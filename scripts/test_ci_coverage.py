"""Protect the target inventory and exact-commit release gate."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import shutil
import tempfile
import types
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parent.parent


def load(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


TARGETS = load("ci_targets", ".github/ci/targets.py")
RELEASE = load("release_coverage", "scripts/release-coverage.py")


class EnvironmentTests(unittest.TestCase):
    def test_native_dependencies_are_required_and_live_settings_are_removed(self):
        keys = ["PROTO_LAB_REQUIRE_CHROME", "GPUI_SMOKE_REQUIRE", "WGPU_BACKEND", "PROTO_LAB_LIVE",
                "TEAMS_MEDIA_TURN_SERVER", "TEAMS_MEDIA_TURN_USER", "TEAMS_MEDIA_TURN_PASSWORD"]
        environment = dict(os.environ, **dict.fromkeys(keys[3:], "synthetic"))
        result = subprocess.run([
            "bash", "-c", 'source "$1"; exec python3 -c "$2"', "test",
            str(ROOT / ".github/ci/environment.sh"),
            f"import os,json; print(json.dumps([os.environ.get(key) for key in {keys!r}]))",
        ], env=environment, capture_output=True, text=True, check=True)
        self.assertEqual(json.loads(result.stdout), ["1", "1", "vulkan", None, None, None, None])


class TargetTests(unittest.TestCase):
    @staticmethod
    def artifact(name, package="example"):
        return {"reason": "compiler-artifact", "executable": f"/workspace/target/{name}",
                "manifest_path": f"/workspace/{package}/Cargo.toml", "profile": {"test": True},
                "target": {"kind": ["test"], "name": name}}

    def test_all_targets_once_and_long_target_alone(self):
        messages = [self.artifact(f"test{i}") for i in range(20)]
        shards = TARGETS.partition(messages, {"/workspace/example/Cargo.toml": "example"},
                                   {"example/test0": 1000})
        self.assertEqual(len(shards), 8)
        self.assertEqual([test["target"] for test in shards[0]], ["test0"])
        self.assertEqual(sorted(test["target"] for shard in shards for test in shard),
                         sorted(f"test{i}" for i in range(20)))

    def test_missing_package_or_duplicate_target_fails(self):
        packages = {"/workspace/example/Cargo.toml": "example", "/workspace/other/Cargo.toml": "other"}
        with self.assertRaisesRegex(ValueError, "every runtime package"):
            TARGETS.partition([self.artifact("one")], packages, {})
        with self.assertRaisesRegex(ValueError, "duplicate test target"):
            TARGETS.partition([self.artifact("one")] * 2, packages, {})

    def test_runner_preserves_failure_and_runs_remaining_targets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "target").mkdir()
            manifest = root / "shards.json"
            tests = [{"id": name, "package": "example", "target": name, "executable": name,
                      "directory": directory} for name in ["first", "second"]]
            manifest.write_text(json.dumps([tests]))
            with contextlib.chdir(root), patch.object(TARGETS.subprocess, "run") as run, \
                    contextlib.redirect_stdout(io.StringIO()):
                run.side_effect = [types.SimpleNamespace(returncode=1), types.SimpleNamespace(returncode=0)]
                self.assertEqual(TARGETS.run(manifest, 0), 1)
                self.assertEqual(run.call_count, 2)
                self.assertEqual(run.call_args.kwargs["env"]["CARGO_MANIFEST_DIR"], directory)
                results = json.loads((root / "target/shard-0.json").read_text())
                self.assertEqual([result["status"] for result in results], [1, 0])

    @unittest.skipUnless(shutil.which("zstd"), "zstd unavailable")
    def test_bundles_contain_each_test_once_and_share_runtime_helpers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = ["target/coverage-continuous-build/debug/deps/first-123",
                     "target/coverage-continuous-build/debug/deps/second-456",
                     "target/coverage-continuous-build/debug/helper",
                     "target/coverage-continuous-build/debug/deps/library.so",
                     "target/ci-bootstrap-build/debug/proto-lab"]
            for name in paths:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("synthetic executable\n")
                path.chmod(0o755)
            excluded = root / "target/coverage-continuous-build/debug/deps/library.rlib"
            excluded.write_text("synthetic dependency\n")
            manifest = [[{"executable": f"/workspace/{path}"}] for path in paths[:2]]
            (root / "target/test-shards.json").write_text(json.dumps(manifest))
            subprocess.run(["bash", str(ROOT / ".github/ci/bundle.sh"), "pack"],
                           cwd=root, capture_output=True, text=True, check=True)

            def files(name):
                result = subprocess.run(["tar", "-I", "zstd", "-tf", f"target/coverage-{name}.tar.zst"],
                                        cwd=root, capture_output=True, text=True, check=True)
                return set(result.stdout.splitlines())

            self.assertEqual(files("shard-0"), {paths[0]})
            self.assertEqual(files("shard-1"), {paths[1]})
            self.assertEqual(files("common"), {*paths[2:], "target/test-shards.json"})

    @unittest.skipUnless(shutil.which("cargo"), "Cargo unavailable")
    def test_shared_executables_merge_to_the_same_coverage_as_cargo(self):
        available = subprocess.run(["cargo", "llvm-cov", "--version"], capture_output=True, check=False)
        if available.returncode:
            self.skipTest("cargo-llvm-cov unavailable")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "tests").mkdir()
            (root / "Cargo.toml").write_text(
                '[package]\nname="example"\nversion="0.1.0"\nedition="2021"\n')
            (root / "src/lib.rs").write_text(
                'pub fn value(flag: bool) -> u32 {\n if flag { 1 } else { 2 }\n}\n')
            for name, flag, expected in [("first", "true", 1), ("second", "false", 2)]:
                (root / f"tests/{name}.rs").write_text(
                    f'#[test] fn {name}() {{ assert_eq!(example::value({flag}), {expected}); }}\n')
            environment = dict(os.environ)
            for key in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_TARGET_DIR"]:
                environment.pop(key, None)
            environment.update(CARGO_LLVM_COV_TARGET_DIR=str(root / "target/build"),
                               CARGO_TARGET_DIR=str(root / "target/build"),
                               RUSTFLAGS="-C llvm-args=-runtime-counter-relocation",
                               LLVM_PROFILE_FILE_NAME="%p-%m%c.profraw")

            def command(*arguments, env=environment):
                result = subprocess.run(arguments, cwd=root, env=env, capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                return result.stdout

            command("cargo", "generate-lockfile", "--offline")
            messages = command("bash", "-c", 'eval "$(cargo llvm-cov show-env --sh)"; '
                               'cargo test --locked --tests --no-run --message-format=json')
            shards = TARGETS.partition([json.loads(line) for line in messages.splitlines()],
                                       {str(root / "Cargo.toml"): "example"}, {}, count=2)
            manifest = root / "shards.json"
            manifest.write_text(json.dumps(shards))
            for index in range(2):
                child = dict(environment, LLVM_PROFILE_FILE_NAME=f"shard-{index}-%p-%m%c.profraw")
                command("bash", "-c", 'eval "$(cargo llvm-cov show-env --sh)"; '
                        'exec python3 "$1" run "$2" "$3"', "test", str(Path(TARGETS.__file__)),
                        str(manifest), str(index), env=child)

            def counts(output):
                data = json.loads(output)
                return next(entry["summary"] for unit in data["data"] for entry in unit["files"]
                            if entry["filename"] == str(root / "src/lib.rs"))

            merged = counts(command("cargo", "llvm-cov", "report", "--json"))
            for path in (root / "target/build").rglob("*.profraw"):
                path.unlink()
            serial = counts(command("cargo", "llvm-cov", "--no-clean", "--json"))
            self.assertEqual(merged, serial)
            self.assertEqual(merged["regions"]["percent"], 100)


class ReleaseGateTests(unittest.TestCase):
    @staticmethod
    def workflow_run(number=1, conclusion="success", **fields):
        return {"head_sha": "abc", "head_branch": "main", "head_repository": {"full_name": "owner/repo"},
                "event": "push", "run_number": number, "run_attempt": 1, "status": "completed",
                "conclusion": conclusion, "html_url": "https://github.com/owner/repo/actions/runs/1", **fields}

    def check_runs(self, runs, timeout=0):
        response = types.SimpleNamespace(stdout=json.dumps([{"workflow_runs": runs}]))
        with patch.object(RELEASE.subprocess, "run", return_value=response), \
                contextlib.redirect_stdout(io.StringIO()):
            return RELEASE.wait_for_coverage("owner/repo", "abc", "main", timeout)

    def test_exact_sha_branch_repository_and_standalone_event_required(self):
        for fields in [{"head_sha": "wrong"}, {"head_branch": "other"}, {"event": "pull_request"},
                       {"head_repository": {"full_name": "fork/repo"}}]:
            with self.subTest(fields=fields), self.assertRaisesRegex(ValueError, "timeout"):
                self.check_runs([self.workflow_run(**fields)])

    def test_success_reused_but_newer_failure_or_cancellation_blocks(self):
        self.check_runs([self.workflow_run()])
        for conclusion in ["failure", "cancelled", "skipped", "timed_out", "action_required"]:
            with self.subTest(conclusion=conclusion), self.assertRaisesRegex(ValueError, "did not pass"):
                self.check_runs([self.workflow_run(), self.workflow_run(2, conclusion)])

    def test_newest_rerun_attempt_is_authoritative(self):
        with self.assertRaisesRegex(ValueError, "did not pass"):
            self.check_runs([self.workflow_run(), self.workflow_run(conclusion="failure", run_attempt=2)])

    def test_pending_run_is_polled_until_success(self):
        pages = [[{"workflow_runs": [self.workflow_run(status="in_progress", conclusion=None)]}],
                 [{"workflow_runs": [self.workflow_run()]}]]
        with patch.object(RELEASE.subprocess, "run", side_effect=[
                types.SimpleNamespace(stdout=json.dumps(page)) for page in pages]), \
                patch.object(RELEASE.time, "sleep") as sleep, contextlib.redirect_stdout(io.StringIO()):
            RELEASE.wait_for_coverage("owner/repo", "abc", "main", 30)
            sleep.assert_called_once()

    def test_api_error_fails_closed(self):
        with patch.object(RELEASE.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "gh")), \
                self.assertRaises(subprocess.CalledProcessError):
            RELEASE.wait_for_coverage("owner/repo", "abc", "main", 0)


if __name__ == "__main__":
    unittest.main()
