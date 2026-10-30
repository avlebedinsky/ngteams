#!/usr/bin/env bash
# Report the complexity debt and refuse inner lint exceptions, over every .rs file under crates/ and spikes/.
# Taken from agterm-rs (scripts/lint-report.sh); only the scanned roots and the exempt-crate list differ.
#
# - Lists each outer #[expect(...)] that lifts a complexity lint (cognitive_complexity, too_many_lines,
#   excessive_nesting, too_many_arguments, type_complexity, fn_params_excessive_bools, struct_excessive_bools)
#   with its file, line, item, lint and reason, then the count, an expect wrapped in #[cfg_attr(..., expect(...))]
#   included. Markdown, so CI can append it to the job summary.
# - Fails (exit 1) on any inner #![allow(...)] or #![expect(...)], or an inner #![cfg_attr(...)] carrying one,
#   outside generated -sys crates that set their own lints (listed in EXEMPT_CRATES): an exception must be
#   an outer #[expect] on the one item, with a reason.
#
# Usage: scripts/lint-report.sh [ROOT]   (default: the repository root)
set -euo pipefail

root="${1:-$(cd "$(dirname "$0")/.." && pwd)}"
[[ -d "$root/crates" || -d "$root/spikes" ]] || { echo "no crates/ or spikes/ under $root" >&2; exit 2; }

exec python3 - "$root" <<'PY'
import re
import sys
from pathlib import Path

COMPLEXITY = {
    "cognitive_complexity",
    "too_many_lines",
    "excessive_nesting",
    "too_many_arguments",
    "type_complexity",
    "fn_params_excessive_bools",
    "struct_excessive_bools",
}
# generated -sys crates (bindgen/gir output), by exact name; their safe wrappers stay under every lint
EXEMPT_CRATES: set[str] = {"wpe-webkit-sys", "wpe-platform-sys", "wpe-javascriptcore-sys"}
# An attribute that is, or may carry (cfg_attr), a lint level; group 1 is "!" for an inner one.
ATTRIBUTE = re.compile(r"#(!?)\[\s*(allow|expect|cfg_attr)\s*\(")
LEVEL = re.compile(r"\b(allow|expect)\s*\(")
LINT = re.compile(r"clippy::(\w+)")
REASON = re.compile(r'reason\s*=\s*"((?:[^"\\]|\\.)*)"', re.S)
ITEM = re.compile(r"\b(fn|struct|enum|union|trait|impl|mod|type|const|static)\b\s*(<[^>]*>\s*)?([A-Za-z_][\w:<>, ]*)?")


def balanced(text: str, start: int) -> int:
    """Index just past the parenthesis closing the one opening at `start`, skipping string literals."""
    depth, index, in_string = 0, start, False
    while index < len(text):
        char = text[index]
        if in_string:
            if char == "\\":
                index += 1
            elif char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return index + 1
        index += 1
    return len(text)


def item_after(text: str, end: int) -> str:
    """The item an attribute ending at `end` applies to: the next line that is not an attribute or a comment."""
    for line in text[end:].splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith(("#[", "//", "]", ")")):
            continue
        match = ITEM.search(stripped)
        if match:
            return f"{match.group(1)} {(match.group(3) or '').strip()}".strip()
        return stripped[:60]
    return "?"


def code_only(line: str) -> bool:
    return not line.lstrip().startswith("//")


def levels(text: str, match: re.Match[str], end: int) -> list[tuple[str, str]]:
    """The (level, body) pairs an attribute sets: itself, or each allow/expect inside a cfg_attr."""
    body = text[match.end() : end - 1]
    if match.group(2) != "cfg_attr":
        return [(match.group(2), body)]
    found = []
    for level in LEVEL.finditer(body):
        close = balanced(body, level.end() - 1)
        found.append((level.group(1), body[level.end() : close - 1]))
    return found


root = Path(sys.argv[1])
expects: list[tuple[str, int, str, str, str]] = []
inner: list[tuple[str, int, str]] = []
for path in sorted(p for base in ("crates", "spikes") for p in (root / base).rglob("*.rs")):
    relative = path.relative_to(root)
    if "target" in relative.parts:
        continue
    text = path.read_text(encoding="utf-8")
    exempt = relative.parts[1] in EXEMPT_CRATES
    for match in ATTRIBUTE.finditer(text):
        line_start = text.rfind("\n", 0, match.start()) + 1
        if not code_only(text[line_start : match.start() + 1]):
            continue
        end = balanced(text, match.end() - 1)
        number = text.count("\n", 0, match.start()) + 1
        set_levels = levels(text, match, end)
        if match.group(1):
            if not exempt and set_levels:
                line_end = text.find("\n", match.start())
                inner.append((str(relative), number, text[line_start : None if line_end < 0 else line_end].strip()))
            continue
        body = " ".join(inside for level, inside in set_levels if level == "expect")
        lints = [lint for lint in LINT.findall(body) if lint in COMPLEXITY]
        if not lints:
            continue
        found = REASON.search(body)
        reason = found.group(1).replace('\\"', '"') if found else "(no reason)"
        item = item_after(text, end)
        expects += [(str(relative), number, item, lint, reason) for lint in lints]

print(f"### complexity expects: {len(expects)}")
print()
for file, number, item, lint, reason in expects:
    print(f"- `{file}:{number}` `{item}` `clippy::{lint}`: {reason}")
if inner:
    print()
    print(f"### refused inner lint attributes: {len(inner)}")
    print()
    for file, number, line in inner:
        print(f"- `{file}:{number}` `{line}`")
    sys.exit(1)
PY
