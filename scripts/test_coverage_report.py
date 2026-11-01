"""The coverage gate must not silently omit untested source or accept partial metrics."""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
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

    def audit_declarations(self):
        self.unused.write_text("pub trait Marker {}\n")
        relative = str(self.unused.relative_to(self.root))
        self.config["unmapped_sources"] = {relative: {
            "sha256": hashlib.sha256(self.unused.read_bytes()).hexdigest(),
            "reason": "reviewed trait declaration with no executable body",
        }}
        return relative

    def test_reviewed_declarations_are_reported_explicitly_instead_of_missing(self):
        relative = self.audit_declarations()
        self.entries.pop()
        self.args.require_target = True
        failed, summary = self.run_report()
        self.assertFalse(failed)
        crate = summary["crates"]["example"]
        self.assertEqual((crate["source_files"], crate["reported_files"]), (2, 1))
        self.assertEqual(crate["missing_source_files"], [])
        self.assertEqual(crate["audited_unmapped_files"][0]["file"], relative)
        self.assertEqual(crate["audited_unmapped_files"][0]["sha256"],
                         self.config["unmapped_sources"][relative]["sha256"])
        self.assertIn("reviewed without LLVM mapping 1", self.report_output)

    def test_changed_reviewed_source_cannot_hide_new_executable_code(self):
        self.audit_declarations()
        self.entries.pop()
        self.args.no_gate = True
        self.unused.write_text("pub fn newly_added() {}\n")
        with self.assertRaisesRegex(ValueError, "review is stale"):
            self.run_report()

    def test_changed_test_module_inclusion_invalidates_its_review(self):
        relative = self.audit_declarations()
        self.config["unmapped_sources"][relative]["context"] = {
            "crates/example/src/lib.rs": hashlib.sha256(self.used.read_bytes()).hexdigest(),
        }
        self.entries.pop()
        self.args.no_gate = True
        self.used.write_text("mod unused;\n")
        with self.assertRaisesRegex(ValueError, "review is stale.*lib.rs"):
            self.run_report()

    def test_audit_context_does_not_exempt_its_executable_file_from_mapping(self):
        relative = self.audit_declarations()
        self.config["unmapped_sources"][relative]["context"] = {
            "crates/example/src/lib.rs": hashlib.sha256(self.used.read_bytes()).hexdigest(),
        }
        self.entries.clear()
        self.args.no_gate = True
        failed, summary = self.run_report()
        self.assertTrue(failed)
        self.assertEqual(summary["crates"]["example"]["missing_source_files"],
                         ["crates/example/src/lib.rs"])

    def test_audited_mapping_still_counts_and_unreviewed_missing_source_still_fails(self):
        self.audit_declarations()
        # A supplied zero-hit mapping remains in the denominator even for a reviewed file.
        failed, summary = self.run_report()
        self.assertFalse(failed)
        self.assertEqual(summary["crates"]["example"]["percent"], dict.fromkeys(METRICS, 50))
        self.assertEqual(summary["crates"]["example"]["audited_unmapped_files"], [])
        new_source = self.source / "new.rs"
        new_source.write_text("pub fn untested() {}\n")
        self.args.no_gate = True
        failed, summary = self.run_report()
        self.assertTrue(failed)
        self.assertEqual(summary["crates"]["example"]["missing_source_files"],
                         ["crates/example/src/new.rs"])

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

    def test_target_gate_rejects_each_metric_just_below_ninety_despite_low_baselines(self):
        self.args.require_target = True
        for metric in METRICS:
            with self.subTest(metric=metric):
                self.entries = [self.entry(path, 10_000, 10_000) for path in [self.used, self.unused]]
                for entry in self.entries:
                    entry["summary"][metric]["covered"] = 8_999
                failed, summary = self.run_report()
                self.assertTrue(failed)
                expected = dict.fromkeys(METRICS, 100)
                expected[metric] = 89.99
                self.assertEqual(summary["crates"]["example"]["percent"], expected)
                self.assertIn(f"{metric} below 90%", self.report_output)
                for entry in self.entries:
                    entry["summary"][metric]["covered"] = 9_000
                self.assertFalse(self.run_report()[0], self.report_output)

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
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        for name in ["coverage.sh", "coverage-report.py"]:
            shutil.copy2(Path(__file__).with_name(name), scripts / name)
        source = self.root / "crates/example/src/lib.rs"
        source.parent.mkdir(parents=True)
        source.write_text("pub fn example() {}\n")
        (source.parent.parent / "Cargo.toml").write_text('[package]\nname = "example"\n')
        (self.root / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/example"]\n')
        (self.root / "coverage.toml").write_text(
            'target = 90\nmetrics = ["lines", "functions", "regions"]\n'
            '[excluded]\n[baselines]\nexample = { lines = 90, functions = 90, regions = 90 }\n')
        entry = CoverageGateTests.entry(source, 100, 100)
        (self.root / "llvm-fixture.json").write_text(json.dumps({
            "type": "llvm.coverage.json.export", "data": [{"files": [entry]}],
        }))
        binary = self.root / "bin/cargo"
        binary.parent.mkdir()
        binary.write_text('''#!/usr/bin/env python3
from pathlib import Path
import json
import os
import shutil
import sys
assert (Path(os.environ["CARGO_LLVM_COV_TARGET_DIR"]) / "CACHEDIR.TAG").read_text().startswith(
    "Signature: 8a477f597d28d172789f06886806bc55")
with Path("commands.jsonl").open("a") as commands:
    commands.write(json.dumps(sys.argv[1:]) + "\\n")
if sys.argv[1:] == ["llvm-cov", "clean", "--workspace"]:
    sys.exit(0)
if sys.argv[1:3] == ["llvm-cov", "report"]:
    if status := int(os.environ.get("SYNTHETIC_EXPORT_EXIT", "0")):
        sys.exit(status)
    shutil.copyfile("llvm-fixture.json", sys.argv[sys.argv.index("--output-path") + 1])
    sys.exit(0)
keys = ["PROTO_LAB_REQUIRE_CHROME", "GPUI_SMOKE_REQUIRE", "PROTO_LAB_LIVE",
        "TEAMS_MEDIA_TURN_SERVER", "TEAMS_MEDIA_TURN_USER", "TEAMS_MEDIA_TURN_PASSWORD",
        "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "LLVM_PROFILE_FILE_NAME",
        "CARGO_LLVM_COV_TARGET_DIR", "LK_CUSTOM_WEBRTC"]
Path("execution.json").write_text(json.dumps({
    "args": sys.argv[1:], "environment": {key: os.environ.get(key) for key in keys}}))
if status := int(os.environ.get("SYNTHETIC_TEST_EXIT", "0")):
    sys.exit(status)
shutil.copyfile("llvm-fixture.json", sys.argv[sys.argv.index("--output-path") + 1])
''')
        binary.chmod(0o755)
        self.environment = dict(os.environ, PATH=str(binary.parent) + os.pathsep + os.environ["PATH"])
        for key in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "LK_CUSTOM_WEBRTC"]:
            self.environment.pop(key, None)
        self.command = ["bash", str(scripts / "coverage.sh"), "-p", "example"]
        self.execution = self.root / "execution.json"

    def run_wrapper(self, *arguments):
        return subprocess.run(self.command + list(arguments), env=self.environment,
                              capture_output=True, text=True, check=False)

    def test_failed_tests_export_current_partial_coverage_without_losing_their_exit_status(self):
        self.environment["SYNTHETIC_TEST_EXIT"] = "101"
        for covered in [100, 89]:
            with self.subTest(covered=covered):
                source = self.root / "crates/example/src/lib.rs"
                (self.root / "llvm-fixture.json").write_text(json.dumps({
                    "type": "llvm.coverage.json.export", "data": [{
                        "files": [CoverageGateTests.entry(source, 100, covered)]}],
                }))
                result = self.run_wrapper("--require-target")
                self.assertEqual(result.returncode, 101, result.stdout + result.stderr)
                self.assertIn("report is partial", result.stderr)
                report = self.root / "target/coverage/continuous/all/summary.json"
                summary = json.loads(report.read_text())
                self.assertEqual(summary["crates"]["example"]["percent"], dict.fromkeys(METRICS, covered))
                if covered < 90:
                    self.assertIn("below 90%", result.stderr)
                commands = [json.loads(line) for line in (self.root / "commands.jsonl").read_text().splitlines()]
                self.assertEqual(commands[-1][:2], ["llvm-cov", "report"])
                self.assertIn("example", commands[-1])
                self.assertNotIn("--ignore-run-fail", json.loads(self.execution.read_text())["args"])

    def test_failed_export_cannot_reuse_a_previous_successful_report(self):
        result = self.run_wrapper("--require-target")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        output = self.root / "target/coverage/continuous/all"
        self.assertTrue((output / "summary.json").is_file())
        self.environment.update(SYNTHETIC_TEST_EXIT="101", SYNTHETIC_EXPORT_EXIT="17")
        result = self.run_wrapper("--require-target")
        self.assertEqual(result.returncode, 101, result.stdout + result.stderr)
        self.assertIn("no current summary", result.stderr)
        self.assertFalse((output / "summary.json").exists())
        self.assertFalse((output / "llvm.json").exists())

    def test_workspace_target_gate_exits_nonzero_for_each_low_metric_in_any_crate(self):
        source = self.root / "crates/other/src/lib.rs"
        source.parent.mkdir(parents=True)
        source.write_text("pub fn other() {}\n")
        (source.parent.parent / "Cargo.toml").write_text('[package]\nname = "other"\n')
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/example", "crates/other"]\n')
        (self.root / "coverage.toml").write_text(
            'target = 90\nmetrics = ["lines", "functions", "regions"]\n'
            '[excluded]\n[baselines]\n'
            'example = { lines = 50, functions = 50, regions = 50 }\n'
            'other = { lines = 0, functions = 0, regions = 0 }\n')
        self.command = self.command[:2]
        example = self.root / "crates/example/src/lib.rs"
        for metric in METRICS:
            with self.subTest(metric=metric):
                entries = [CoverageGateTests.entry(path, 10_000, 10_000) for path in [example, source]]
                for covered in [8_999, 9_000]:
                    entries[1]["summary"][metric]["covered"] = covered
                    (self.root / "llvm-fixture.json").write_text(json.dumps({
                        "type": "llvm.coverage.json.export", "data": [{"files": entries}],
                    }))
                    result = self.run_wrapper("--require-target")
                    if covered == 8_999:
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn(f"other: {metric} below 90%", result.stderr)
                    else:
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    execution = json.loads(self.execution.read_text())["args"]
                    selected = [execution[index + 1] for index, value in enumerate(execution) if value == "-p"]
                    self.assertCountEqual(selected, ["example", "other"])
                    report = self.root / "target/coverage/continuous/all/summary.json"
                    summary = json.loads(report.read_text())
                    self.assertEqual(summary["packages"], ["example", "other"])
                    self.assertEqual(summary["crates"]["example"]["percent"], dict.fromkeys(METRICS, 100))
                    self.assertEqual(summary["crates"]["other"]["percent"][metric], covered / 100)

    def test_measurement_prepares_cargos_cache_tag_and_refuses_an_invalid_existing_tag(self):
        result = self.run_wrapper()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(self.execution.exists())
        tag = self.root / "target/coverage-continuous-build/CACHEDIR.TAG"
        tag.write_text("unrecognized cache directory\n")
        self.execution.unlink()
        result = self.run_wrapper()
        self.assertEqual(result.returncode, 2)
        self.assertIn("invalid cache signature", result.stderr)
        self.assertEqual(tag.read_text(), "unrecognized cache directory\n")
        self.assertFalse(self.execution.exists())

    def test_measurement_is_serial_requires_dependencies_and_removes_live_configuration(self):
        self.environment.update({
            "PROTO_LAB_REQUIRE_CHROME": "0", "GPUI_SMOKE_REQUIRE": "0", "RUST_TEST_THREADS": "12",
            "PROTO_LAB_LIVE": "1", "TEAMS_MEDIA_TURN_SERVER": "synthetic-server",
            "TEAMS_MEDIA_TURN_USER": "synthetic-user", "TEAMS_MEDIA_TURN_PASSWORD": "synthetic-password",
        })
        result = self.run_wrapper()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        execution = json.loads(self.execution.read_text())
        self.assertEqual(execution["args"][-3:], ["--", "--test-threads", "1"])
        self.assertEqual({key: execution["environment"][key] for key in [
            "PROTO_LAB_REQUIRE_CHROME", "GPUI_SMOKE_REQUIRE", "PROTO_LAB_LIVE",
            "TEAMS_MEDIA_TURN_SERVER", "TEAMS_MEDIA_TURN_USER", "TEAMS_MEDIA_TURN_PASSWORD"]}, {
            "PROTO_LAB_REQUIRE_CHROME": "1", "GPUI_SMOKE_REQUIRE": "1", "PROTO_LAB_LIVE": None,
            "TEAMS_MEDIA_TURN_SERVER": None, "TEAMS_MEDIA_TURN_USER": None,
            "TEAMS_MEDIA_TURN_PASSWORD": None,
        })

    def test_continuous_flags_preserve_cargo_precedence_and_argument_boundaries(self):
        for encoded in [None, "", "--cfg\x1ffeature=\"synthetic value\""]:
            with self.subTest(encoded=encoded):
                self.environment["RUSTFLAGS"] = "--cfg synthetic_flag"
                if encoded is None:
                    self.environment.pop("CARGO_ENCODED_RUSTFLAGS", None)
                else:
                    self.environment["CARGO_ENCODED_RUSTFLAGS"] = encoded
                result = self.run_wrapper()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                environment = json.loads(self.execution.read_text())["environment"]
                self.assertEqual(environment["LLVM_PROFILE_FILE_NAME"], "%p-%m%c.profraw")
                if encoded is None:
                    self.assertEqual(environment["RUSTFLAGS"],
                                     "--cfg synthetic_flag -C llvm-args=-runtime-counter-relocation")
                else:
                    expected = (encoded + "\x1f" if encoded else "") + "-C\x1fllvm-args=-runtime-counter-relocation"
                    self.assertEqual(environment["CARGO_ENCODED_RUSTFLAGS"], expected)
                    self.assertEqual(environment["RUSTFLAGS"], "--cfg synthetic_flag")

    def test_modes_keep_builds_reports_and_report_only_separate(self):
        result = self.run_wrapper()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.execution.unlink()
        result = self.run_wrapper("--profile-mode", "ordinary", "--report-only")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.execution.exists())
        self.environment["LLVM_PROFILE_FILE_NAME"] = "inherited-%c.profraw"
        self.environment["RUSTFLAGS"] = "--cfg ordinary_flag"
        result = self.run_wrapper("--profile-mode", "ordinary")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        environment = json.loads(self.execution.read_text())["environment"]
        self.assertEqual(environment["LLVM_PROFILE_FILE_NAME"], "%p-%m.profraw")
        self.assertEqual(environment["RUSTFLAGS"], "--cfg ordinary_flag")
        self.assertEqual(environment["CARGO_LLVM_COV_TARGET_DIR"], str(self.root / "target/coverage-build"))
        for report in ["target/coverage/all/summary.json", "target/coverage/continuous/all/summary.json"]:
            self.assertTrue((self.root / report).is_file())
        for build in ["coverage-build", "coverage-continuous-build"]:
            self.assertTrue((self.root / "target" / build / "CACHEDIR.TAG").is_file())
        self.execution.unlink()
        for arguments in [(), ("--profile-mode", "ordinary")]:
            result = self.run_wrapper(*arguments, "--report-only")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(self.execution.exists())

    def test_sdk_cache_is_validated_forwarded_and_otherwise_preserved(self):
        self.environment["LK_CUSTOM_WEBRTC"] = "existing-sdk"
        result = self.run_wrapper()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(self.execution.read_text())["environment"]["LK_CUSTOM_WEBRTC"], "existing-sdk")
        sdk = self.root / "SDK with spaces"
        (sdk / "include").mkdir(parents=True)
        (sdk / "lib").mkdir()
        (sdk / "lib/libwebrtc.a").touch()
        result = self.run_wrapper("--webrtc-cache", str(sdk))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(self.execution.read_text())["environment"]["LK_CUSTOM_WEBRTC"], str(sdk))
        self.execution.unlink()
        for arguments in [("--webrtc-cache",), ("--webrtc-cache", ""),
                          ("--webrtc-cache", str(self.root)), ("--profile-mode",),
                          ("--profile-mode", "unknown")]:
            with self.subTest(arguments=arguments):
                result = self.run_wrapper(*arguments)
                self.assertEqual(result.returncode, 2)
                self.assertFalse(self.execution.exists())

    def test_relative_sdk_cache_uses_callers_directory_even_with_a_repository_decoy(self):
        caller = self.root / "caller"
        caller.mkdir()
        relative = "SDK with spaces"
        for sdk in [caller / relative, self.root / relative]:
            (sdk / "include").mkdir(parents=True)
            (sdk / "lib").mkdir()
            (sdk / "lib/libwebrtc.a").touch()
        result = subprocess.run(self.command + ["--webrtc-cache", relative], cwd=caller,
                                env=self.environment, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(self.execution.read_text())["environment"]["LK_CUSTOM_WEBRTC"],
                         str(caller / relative))

    def test_thread_override_is_forwarded_but_invalid_values_never_run_cargo(self):
        result = self.run_wrapper("--test-threads", "3")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(self.execution.read_text())["args"][-3:],
                         ["--", "--test-threads", "3"])
        self.execution.unlink()
        for arguments in [("--test-threads",), ("--test-threads", "0"), ("--test-threads", "-1"),
                          ("--test-threads", "many"), ("--test-threads", "1 --features live")]:
            with self.subTest(arguments=arguments):
                result = self.run_wrapper(*arguments)
                self.assertEqual(result.returncode, 2)
                self.assertIn("positive integer", result.stderr)
                self.assertFalse(self.execution.exists())


@unittest.skipUnless(sys.platform == "linux" and shutil.which("cargo") and
                     subprocess.run(["cargo", "llvm-cov", "--version"], stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL).returncode == 0,
                     "requires Linux and the coverage toolchain")
class CoverageArtifactTests(unittest.TestCase):
    def test_a_changed_library_is_not_measured_against_an_old_unselected_consumer(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts = root / "scripts"
            scripts.mkdir()
            for name in ["coverage.sh", "coverage-report.py"]:
                shutil.copy2(Path(__file__).with_name(name), scripts / name)
            (root / "Cargo.toml").write_text(
                '[workspace]\nresolver = "3"\nmembers = ["example", "consumer"]\n')
            for name in ["example", "consumer"]:
                source = root / name / "src"
                source.mkdir(parents=True)
                dependencies = '\n[dependencies]\nexample = { path = "../example" }\n' if name == "consumer" else ''
                (source.parent / "Cargo.toml").write_text(
                    f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2024"\n{dependencies}')
            library = root / "example/src/lib.rs"
            library.write_text('pub fn value() -> u32 {\n let mut n = 0;\n' + ' n += 1;\n' * 30 + ' n\n}\n')
            (root / "consumer/src/main.rs").write_text(
                'fn main() { assert_eq!(example::value(), 30); }\n'
                '#[test] fn uses_library() { main(); }\n')
            (root / "coverage.toml").write_text(
                'target = 90\nmetrics = ["lines", "functions", "regions"]\n'
                '[excluded]\n[baselines]\nexample = { lines = 90, functions = 90, regions = 90 }\n')
            environment = dict(os.environ)
            for name in ["CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR",
                         "CARGO_LLVM_COV_BUILD_DIR"]:
                environment.pop(name, None)
            sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
            environment.update(
                RUSTUP_TOOLCHAIN=Path(sysroot).name,
                CARGO_LLVM_COV_TARGET_DIR=str(root / "target/coverage-continuous-build"),
                RUSTFLAGS="-C llvm-args=-runtime-counter-relocation",
                LLVM_PROFILE_FILE_NAME="%p-%m%c.profraw",
            )

            def run(*args):
                return subprocess.run(args, cwd=root, env=environment, capture_output=True, text=True)

            locked = run("cargo", "generate-lockfile", "--offline")
            self.assertEqual(locked.returncode, 0, locked.stderr)
            warm = run("cargo", "llvm-cov", "-p", "consumer", "--locked", "--no-report")
            self.assertEqual(warm.returncode, 0, warm.stdout + warm.stderr)
            stale = [path for path in (root / "target/coverage-continuous-build/debug/deps").glob("consumer-*")
                     if path.suffix == "" and os.access(path, os.X_OK)]
            self.assertTrue(stale, "the old consumer must actually exist")
            library.write_text('pub fn value() -> u32 { 42 }\n'
                               '#[test] fn current_value() { assert_eq!(value(), 42); }\n')
            wrapper = scripts / "coverage.sh"
            fixed = wrapper.read_text()
            # Reproduce the previous selection-only cleanup with a real consumer binary and LLVM export.
            wrapper.write_text(fixed.replace('"${measurement_env[@]}" cargo llvm-cov clean --workspace', ':'))
            environment.pop("RUSTFLAGS")
            broken = run("bash", str(wrapper), "-p", "example", "--require-target")
            self.assertNotEqual(broken.returncode, 0, broken.stdout + broken.stderr)
            self.assertIn("below 90%", broken.stdout + broken.stderr)
            self.assertTrue(any(path.exists() for path in stale))
            wrapper.write_text(fixed)
            measured = run("bash", str(wrapper), "-p", "example", "--require-target")
            self.assertEqual(measured.returncode, 0, measured.stdout + measured.stderr)
            self.assertFalse(any(path.exists() for path in stale))
            summary = json.loads((root / "target/coverage/continuous/example/summary.json").read_text())
            self.assertEqual(summary["crates"]["example"]["percent"], dict.fromkeys(METRICS, 100))


if __name__ == "__main__":
    unittest.main()
