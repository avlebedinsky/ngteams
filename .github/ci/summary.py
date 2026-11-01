#!/usr/bin/env python3
"""Display the measured crate coverage; the coverage wrapper owns all pass/fail decisions."""

import json
import os
from pathlib import Path

report = Path("target/coverage/continuous/all/summary.json")
print("## Runtime coverage\n")
if not report.is_file():
    print("No coverage report was generated. Check the environment, build and test logs.")
else:
    data = json.loads(report.read_text())
    if os.environ.get("COVERAGE_TEST_OUTCOME") != "success":
        print("**The measurement step failed. These results may be partial; they do not indicate a passing run.**\n")
    print(f"Required: {data['target']}% for each metric in every runtime crate.\n")
    print("| Crate | Lines | Functions | Regions | Threshold |")
    print("| --- | ---: | ---: | ---: | --- |")
    for name in data["packages"]:
        crate = data["crates"][name]
        rates = [crate["percent"][metric] for metric in ("lines", "functions", "regions")]
        rendered = ["unmeasured" if rate is None else f"{rate:.2f}%" for rate in rates]
        passed = not crate["missing_source_files"] and all(
            rate is not None and rate >= data["target"] for rate in rates
        )
        print(f"| {name} | {' | '.join(rendered)} | {'Pass' if passed else 'Fail'} |")
