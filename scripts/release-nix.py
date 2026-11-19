#!/usr/bin/env python3
"""Write the Nix flake's release file (packaging/nix/release.json) for one published release.

    python3 scripts/release-nix.py VERSION SHA256SUMS OUTPUT
    python3 scripts/release-nix.py newest RELEASES

VERSION is the release's four-part version. SHA256SUMS is the release's own checksum file. The flake fetches the
Linux archive, so the file holds its URL and its hash in the SRI form that Nix reads. The script writes nothing if a
check fails, and it does not replace a file of the same or a newer release.

`newest` prints the version of the newest published app release in RELEASES, the JSON of
`gh release list --json tagName,isDraft,isPrerelease`. Drafts, prereleases (the `web-engine-<key>` runtimes) and
other tags do not count.
"""

from __future__ import annotations

import base64
import json
import re
import sys
from pathlib import Path

REPOSITORY = "nickalie/ngteams"
# The Nix system name of each Linux archive that the release publishes.
ARCHIVES = {"x86_64-linux": "ngteams-{version}-linux-x86_64.tar.gz"}


def checksum(manifest: str, name: str) -> str:
    hashes = [line[:64] for line in manifest.splitlines() if line[66:] == name and line[64:66] in ("  ", " *")]
    if len(hashes) != 1 or not re.fullmatch(r"[0-9a-fA-F]{64}", hashes[0]):
        raise ValueError(f"SHA256SUMS has no single SHA-256 line for {name}")
    return hashes[0].lower()


def release(version: str, manifest: str) -> dict:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError(f"expected MAJOR.MINOR.PATCH.RUN, got {version!r}")
    entry: dict = {"version": version}
    for system, pattern in ARCHIVES.items():
        name = pattern.format(version=version)
        digest = bytes.fromhex(checksum(manifest, name))
        entry[system] = {
            "url": f"https://github.com/{REPOSITORY}/releases/download/v{version}/{name}",
            "hash": "sha256-" + base64.b64encode(digest).decode(),
        }
    return entry


def numbers(version: str) -> tuple[int, ...]:
    return tuple(int(part) for part in version.split("."))


def newest(releases: str) -> str:
    try:
        entries = json.loads(releases)
    except ValueError as error:
        raise ValueError(f"the release list is not JSON: {error}") from error
    if not isinstance(entries, list):
        raise ValueError("the release list is not a JSON list")
    versions = [
        entry["tagName"][1:]
        for entry in entries
        if isinstance(entry, dict)
        and entry.get("isDraft") is False
        and entry.get("isPrerelease") is False
        and isinstance(entry.get("tagName"), str)
        and re.fullmatch(r"v[0-9]+(\.[0-9]+){3}", entry["tagName"])
    ]
    if not versions:
        raise ValueError("the release list has no published app release")
    return max(versions, key=numbers)


def current(output: Path) -> str | None:
    """The release that `output` holds now, if it holds one."""
    try:
        version = json.loads(output.read_text()).get("version")
    except (OSError, ValueError, AttributeError):
        return None
    return version if isinstance(version, str) and re.fullmatch(r"[0-9]+(\.[0-9]+){3}", version) else None


def main(argv: list[str]) -> int:
    if len(argv) == 3 and argv[1] == "newest":
        try:
            print(newest(Path(argv[2]).read_text()))
        except (OSError, ValueError) as error:
            print(f"error: {error}", file=sys.stderr)
            return 1
        return 0
    if len(argv) != 4:
        print("usage: scripts/release-nix.py VERSION SHA256SUMS OUTPUT | newest RELEASES", file=sys.stderr)
        return 2
    output = Path(argv[3])
    try:
        entry = release(argv[1], Path(argv[2]).read_text())
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    held = current(output)
    # a run of an older release that finishes late must not move the flake back
    if held is not None and numbers(held) >= numbers(argv[1]):
        print(f"{output} holds {held} already: not changed")
        return 0
    output.write_text(json.dumps(entry, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
