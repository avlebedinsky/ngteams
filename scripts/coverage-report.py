#!/usr/bin/env python3
"""Inventory selected Rust sources, gate per-crate LLVM coverage and rank gaps."""

import argparse
import hashlib
import json
from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parent.parent


def configuration():
    config = tomllib.loads((ROOT / "coverage.toml").read_text())
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
    crates = {}
    for member in workspace["members"]:
        manifest = tomllib.loads((ROOT / member / "Cargo.toml").read_text())
        name = manifest["package"]["name"]
        if name not in config["excluded"]:
            crates[name] = ROOT / member
    return config, crates


def select(crates, requested):
    unknown = set(requested) - crates.keys()
    if unknown:
        raise ValueError(f"unknown or excluded packages: {', '.join(sorted(unknown))}")
    return sorted(set(requested) if requested else crates)


def selection_directory(crates, selected):
    if set(selected) == set(crates):
        return "all"
    name = "+".join(selected)
    if len(name.encode()) <= 200:
        return name
    return "selection-" + hashlib.sha256(name.encode()).hexdigest()[:16]


def percentage(counts):
    # A crate with no instrumentable code cannot establish a coverage gate.
    return 100 * counts["covered"] / counts["count"] if counts["count"] else None


def reviewed_sources(config, expected):
    """Accept only individually reviewed, unchanged sources with no LLVM mapping."""
    reviewed = {}
    for relative, audit in config.get("unmapped_sources", {}).items():
        path = (ROOT / relative).resolve()
        if path not in expected:
            continue
        reason = audit["reason"]
        if not isinstance(reason, str) or not reason.strip():
            raise ValueError(f"missing unmapped-source review reason: {relative}")
        checks = [(relative, audit["sha256"]), *audit.get("context", {}).items()]
        for checked, digest in checks:
            if hashlib.sha256((ROOT / checked).read_bytes()).hexdigest() != digest:
                raise ValueError(f"unmapped-source review is stale; inspect this file again: {checked}")
        reviewed[path] = {"file": relative, **audit}
    return reviewed


def collect(report, expected, metrics):
    files = {}
    for unit in report["data"]:
        for entry in unit["files"]:
            path = Path(entry["filename"])
            path = (path if path.is_absolute() else ROOT / path).resolve()
            if path not in expected:
                continue
            if path in files:
                raise ValueError(f"duplicate file in LLVM report: {path}")
            summary = {}
            for metric in metrics:
                counts = entry["summary"][metric]
                count, covered = counts["count"], counts["covered"]
                if type(count) is not int or type(covered) is not int or not 0 <= covered <= count:
                    raise ValueError(f"invalid {metric} counts for {path}")
                summary[metric] = {"count": count, "covered": covered}
            files[path] = summary
    return files


def summarize(args, config, crates, selected):
    metrics = config["metrics"]
    inventory = {
        name: sorted(path.resolve() for path in (crates[name] / "src").rglob("*.rs"))
        for name in selected
    }
    expected = {path for paths in inventory.values() for path in paths}
    reviewed = reviewed_sources(config, expected)
    report = json.loads(args.report.read_text())
    if report.get("type") != "llvm.coverage.json.export":
        raise ValueError("expected LLVM coverage JSON export")
    files = collect(report, expected, metrics)
    result = {"target": config["target"], "packages": selected, "crates": {}, "gaps": []}
    failures = []
    for name in selected:
        totals = {metric: {"count": 0, "covered": 0} for metric in metrics}
        missing = []
        audited_unmapped = []
        for path in inventory[name]:
            relative = str(path.relative_to(ROOT))
            if path not in files:
                if path in reviewed:
                    audited_unmapped.append(reviewed[path])
                else:
                    missing.append(relative)
                continue
            summary = files[path]
            for metric in metrics:
                for key in ("count", "covered"):
                    totals[metric][key] += summary[metric][key]
            gaps = {metric: counts["count"] - counts["covered"]
                    for metric, counts in summary.items()}
            result["gaps"].append({"file": relative, "counts": summary,
                                   "percent": {m: percentage(c) for m, c in summary.items()}, "uncovered": gaps,
                                   "score": sum(gaps.values())})
        rates = {metric: percentage(counts) for metric, counts in totals.items()}
        baseline = config["baselines"].get(name)
        result["crates"][name] = {
            "counts": totals, "percent": rates, "baseline": baseline,
            "source_files": len(inventory[name]),
            "reported_files": len(inventory[name]) - len(missing) - len(audited_unmapped),
            "missing_source_files": missing,
            "audited_unmapped_files": audited_unmapped,
        }
        rendered = ", ".join(f"{metric} {rate:.2f}%" if rate is not None else f"{metric} unmeasured"
                             for metric, rate in rates.items())
        mapped = result["crates"][name]["reported_files"]
        print(f"{name}: {rendered}; sources {mapped}/{len(inventory[name])}; "
              f"reviewed without LLVM mapping {len(audited_unmapped)}")
        if missing:
            failures.append(f"{name}: sources absent from LLVM export (feature-gated or without instrumentable code): "
                            + ", ".join(missing))
        if not args.no_gate:
            if baseline is None and not args.require_target:
                failures.append(f"{name}: baseline not measured; use --no-gate to establish it")
            else:
                for metric, rate in rates.items():
                    floor = config["target"] if args.require_target else baseline[metric]
                    if rate is None or rate < floor:
                        failures.append(f"{name}: {metric} below {floor}%")
    result["gaps"].sort(key=lambda gap: (-gap["score"], gap["file"]))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print("\nLargest gaps (uncovered lines / functions / regions):")
    for gap in result["gaps"][:args.top]:
        print(f"  {gap['file']}: " + " / ".join(str(gap["uncovered"][m]) for m in metrics))
    for failure in failures:
        print(f"ERROR: {failure}", file=sys.stderr)
    return bool(failures)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("list").add_argument("packages", nargs="*")
    commands.add_parser("directory").add_argument("packages", nargs="*")
    report = commands.add_parser("report")
    report.add_argument("report", type=Path)
    report.add_argument("--output", type=Path, required=True)
    report.add_argument("--packages", nargs="+", required=True)
    report.add_argument("--top", type=int, default=20)
    gate = report.add_mutually_exclusive_group()
    gate.add_argument("--no-gate", action="store_true")
    gate.add_argument("--require-target", action="store_true")
    args = parser.parse_args()
    config, crates = configuration()
    selected = select(crates, args.packages)
    if args.command == "list":
        print("\n".join(selected))
        return 0
    if args.command == "directory":
        print(selection_directory(crates, selected))
        return 0
    return summarize(args, config, crates, selected)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, OSError) as error:
        sys.exit(f"coverage: {error}")
