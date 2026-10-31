"""The coverage gate must not silently omit untested source or accept partial metrics."""

import contextlib
import importlib.util
import io
import json
from pathlib import Path
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
        self.assertTrue(self.run_report()[0])

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


if __name__ == "__main__":
    unittest.main()
