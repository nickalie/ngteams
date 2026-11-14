"""Synthetic Cargo stamps must distinguish restored dependencies from new workspace units."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest


SPEC = importlib.util.spec_from_file_location("compile_times", Path(__file__).with_name("ci-compile-times.py"))
TIMES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TIMES)


class CompileTimesTests(unittest.TestCase):
    def test_cached_dependencies_and_overlapping_workspace_units(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for package, unit, begin, end in [
                ("third-party", "lib_third_party", 10, 14),
                ("example", "lib_example", 102, 105),
                ("example", "test_lib_example", 103, 110),
                ("new-dependency", "lib_new_dependency", 101, 103),
            ]:
                folder = root / "debug/.fingerprint" / f"{package}-{unit}"
                folder.mkdir(parents=True)
                stamp = folder / "invoked.timestamp"
                marker = folder / f"{unit}.json"
                stamp.touch()
                marker.write_text("{}")
                os.utime(stamp, (begin, begin))
                os.utime(marker, (end, end))
            data = TIMES.collect(root, 100, {"example"})
            self.assertEqual(data["summary"]["dependencies"],
                             {"fresh_units": 1, "cached_units": 1, "unit_seconds": 2})
            self.assertEqual(data["summary"]["workspace"],
                             {"fresh_units": 2, "cached_units": 0, "unit_seconds": 10})
            self.assertEqual(data["compiler_span_seconds"], 9)

    def test_incomplete_fingerprint_does_not_report_a_negative_duration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            folder = root / "debug/.fingerprint/example-123"
            folder.mkdir(parents=True)
            (folder / "invoked.timestamp").touch()
            marker = folder / "lib-example.json"
            marker.write_text("{}")
            os.utime(marker, (10, 10))
            self.assertEqual(TIMES.collect(root, 100, {"example"})["units"], [])


if __name__ == "__main__":
    unittest.main()
