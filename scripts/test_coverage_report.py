"""The coverage gate must not silently omit untested source or accept partial metrics."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import types
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "coverage_report", Path(__file__).with_name("coverage-report.py")
)
REPORTER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORTER)
METRICS = ["lines", "functions", "regions"]


class CoverageGateTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.source = self.root / "crates/example/src"
        self.source.mkdir(parents=True)
        self.used = self.source / "lib.rs"
        self.used.write_text("pub fn used() {}\n")
        self.unused = self.source / "unused.rs"
        self.unused.write_text("pub fn unused() {}\n")
        self.config = {
            "target": 90,
            "metrics": METRICS,
            "baselines": {"example": {metric: 50 for metric in METRICS}},
        }
        self.args = types.SimpleNamespace(
            report=self.root / "llvm.json",
            output=self.root / "summary.json",
            no_gate=False,
            require_target=False,
            top=20,
        )
        self.entries = [self.entry(self.used, 10, 10), self.entry(self.unused, 10, 0)]

    @staticmethod
    def entry(path, count, covered):
        return {
            "filename": str(path),
            "summary": {metric: {"count": count, "covered": covered} for metric in METRICS},
        }

    def run_report(self):
        self.args.report.write_text(json.dumps({
            "type": "llvm.coverage.json.export", "data": [{"files": self.entries}],
        }))
        output = io.StringIO()
        with patch.object(REPORTER, "ROOT", self.root), contextlib.redirect_stdout(output), \
                contextlib.redirect_stderr(output):
            failed = REPORTER.summarize(
                self.args, self.config, {"example": self.source.parent}, ["example"]
            )
        self.report_output = output.getvalue()
        return failed, json.loads(self.args.output.read_text())

    def test_zero_hit_source_counts_against_every_metric_and_ranks_first(self):
        failed, summary = self.run_report()
        self.assertFalse(failed)
        self.assertEqual(summary["crates"]["example"]["percent"], dict.fromkeys(METRICS, 50))
        self.assertEqual(summary["gaps"][0]["file"], "crates/example/src/unused.rs")

    def test_missing_source_fails_even_without_a_threshold_gate(self):
        self.entries.pop()
        self.args.no_gate = True
        failed, summary = self.run_report()
        self.assertTrue(failed)
        self.assertEqual(summary["crates"]["example"]["missing_source_files"],
                         ["crates/example/src/unused.rs"])

    def test_one_failing_metric_fails_the_gate(self):
        self.config["baselines"]["example"]["regions"] = 51
        self.assertTrue(self.run_report()[0])

    def test_target_gate_uses_ninety_instead_of_baseline(self):
        self.args.require_target = True
        self.config["baselines"]["example"] = dict.fromkeys(METRICS, 95)
        self.entries = [self.entry(path, 100, 90) for path in [self.used, self.unused]]
        self.assertFalse(self.run_report()[0])
        for entry in self.entries:
            entry["summary"]["regions"]["covered"] = 89
        self.assertTrue(self.run_report()[0])
        self.assertIn("regions below 90%", self.report_output)

    def test_unequal_files_use_weighted_counts_and_each_metrics_own_rate(self):
        self.config["baselines"]["example"] = dict.fromkeys(METRICS, 0)
        self.entries[0]["summary"] = {
            "lines": {"count": 10, "covered": 10},
            "functions": {"count": 2, "covered": 2},
            "regions": {"count": 8, "covered": 8},
        }
        self.entries[1]["summary"] = {
            "lines": {"count": 30, "covered": 0},
            "functions": {"count": 8, "covered": 0},
            "regions": {"count": 2, "covered": 0},
        }
        failed, summary = self.run_report()
        self.assertFalse(failed)
        self.assertEqual(summary["crates"]["example"]["percent"],
                         {"lines": 25, "functions": 20, "regions": 80})
        self.assertEqual(summary["packages"], ["example"])
        self.config["baselines"]["example"]["functions"] = 21
        self.assertTrue(self.run_report()[0])
        self.assertIn("functions below 21%", self.report_output)

    def test_workspace_and_long_partial_selections_have_bounded_directory_names(self):
        _, crates = REPORTER.configuration()
        all_packages = REPORTER.select(crates, [])
        self.assertEqual(REPORTER.selection_directory(crates, all_packages), "all")
        self.assertEqual(REPORTER.selection_directory(crates, ["teams-store"]), "teams-store")
        large_names = ["package-" + str(index) + "-" + "x" * 70 for index in range(10)]
        large_crates = dict.fromkeys(large_names)
        partial = REPORTER.select(large_crates, large_names[:-1])
        directory = REPORTER.selection_directory(large_crates, partial)
        self.assertLessEqual(len(directory.encode()), 200)
        # Ordering the same selection differently must reuse its report directory.
        self.assertEqual(directory, REPORTER.selection_directory(
            large_crates, REPORTER.select(large_crates, list(reversed(partial)))))
        other = REPORTER.selection_directory(large_crates, large_names[1:])
        self.assertNotEqual(directory, other)

    def test_no_baseline_does_not_claim_a_passing_gate(self):
        self.config["baselines"].clear()
        self.assertTrue(self.run_report()[0])
        self.args.no_gate = True
        self.assertFalse(self.run_report()[0])

    def test_unrelated_tests_and_vendor_do_not_dilute_source_counts(self):
        self.entries.extend([
            self.entry(self.root / "crates/example/tests/test.rs", 100, 100),
            self.entry(self.root / "vendor/lib.rs", 100, 100),
        ])
        failed, summary = self.run_report()
        self.assertFalse(failed)
        self.assertEqual(summary["crates"]["example"]["counts"]["lines"]["count"], 20)

    def test_empty_metrics_are_unmeasured_and_fail(self):
        self.entries = [self.entry(path, 0, 0) for path in [self.used, self.unused]]
        failed, summary = self.run_report()
        self.assertTrue(failed)
        self.assertIsNone(summary["crates"]["example"]["percent"]["lines"])

    def test_invalid_or_duplicate_counts_are_rejected(self):
        for counts in [{"count": 10, "covered": 11}, {"count": 10, "covered": True}]:
            with self.subTest(counts=counts):
                self.entries[0]["summary"]["lines"] = counts
                with self.assertRaisesRegex(ValueError, "invalid lines"):
                    self.run_report()
        self.entries = [self.entry(self.used, 10, 10)] * 2
        with self.assertRaisesRegex(ValueError, "duplicate file"):
            self.run_report()

    def test_unknown_and_excluded_packages_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "unknown or excluded"):
            REPORTER.select({"example": self.source.parent}, ["wpe-platform-sys"])


class CoverageWrapperTests(unittest.TestCase):
    def test_measurement_prepares_cargos_cache_tag_and_refuses_an_invalid_existing_tag(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            scripts = root / "scripts"
            scripts.mkdir()
            for name in ["coverage.sh", "coverage-report.py"]:
                shutil.copy2(Path(__file__).with_name(name), scripts / name)
            source = root / "crates/example/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("pub fn example() {}\n")
            (source.parent.parent / "Cargo.toml").write_text('[package]\nname = "example"\n')
            (root / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/example"]\n')
            (root / "coverage.toml").write_text(
                'target = 90\nmetrics = ["lines", "functions", "regions"]\n'
                '[excluded]\n[baselines]\nexample = { lines = 90, functions = 90, regions = 90 }\n')
            entry = CoverageGateTests.entry(source, 100, 100)
            (root / "llvm-fixture.json").write_text(json.dumps({
                "type": "llvm.coverage.json.export", "data": [{"files": [entry]}],
            }))
            binary = root / "bin/cargo"
            binary.parent.mkdir()
            binary.write_text('''#!/usr/bin/env python3
from pathlib import Path
import shutil
import sys
assert Path("target/coverage-build/CACHEDIR.TAG").read_text().startswith(
    "Signature: 8a477f597d28d172789f06886806bc55")
Path("cargo-invoked").touch()
shutil.copyfile("llvm-fixture.json", sys.argv[sys.argv.index("--output-path") + 1])
''')
            binary.chmod(0o755)
            environment = dict(os.environ, PATH=str(binary.parent) + os.pathsep + os.environ["PATH"])
            command = ["bash", str(scripts / "coverage.sh"), "-p", "example"]
            result = subprocess.run(command, env=environment, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue((root / "cargo-invoked").exists())
            tag = root / "target/coverage-build/CACHEDIR.TAG"
            tag.write_text("unrecognized cache directory\n")
            (root / "cargo-invoked").unlink()
            result = subprocess.run(command, env=environment, capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 2)
            self.assertIn("invalid cache signature", result.stderr)
            self.assertEqual(tag.read_text(), "unrecognized cache directory\n")
            self.assertFalse((root / "cargo-invoked").exists())


if __name__ == "__main__":
    unittest.main()
