#!/usr/bin/env python3
"""The web engine runtime's GitHub release, `web-engine-<key>`: reuse a published one, publish a new one.

A shipped app pins its runtime's size and hash, so a published archive is never replaced: `publish` fails when one
differs. `gh` reads GH_TOKEN and GH_REPO from the environment.

  fetch KEY DIR                print `reuse` after downloading KEY's published runtime into DIR, `upload` when there is none
  publish DIR COMMIT KEY       ensure DIR's runtime for KEY is on `web-engine-KEY` (a prerelease, never latest), created at COMMIT
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def archive_name(key: str) -> str:
    return f"ngteams-web-engine-{key}-linux-x86_64.tar.zst"


def sha256(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def gh(*args: str) -> str:
    result = subprocess.run(["gh", *args], capture_output=True, text=True, check=False)
    if result.returncode:
        raise ValueError(f"gh {' '.join(args[:3])}: {result.stderr.strip()}")
    return result.stdout


def published_assets(tag: str) -> dict[str, dict] | None:
    """The assets of release TAG by name, None without a release. Only gh's own "release not found" counts as absent:
    a failure to read must not look like a runtime to build and publish."""
    result = subprocess.run(["gh", "release", "view", tag, "--json", "assets"],
                            capture_output=True, text=True, check=False)
    if result.returncode:
        if "release not found" in result.stderr:
            return None
        raise ValueError(f"gh release view {tag}: {result.stderr.strip()}")
    return {asset["name"]: asset for asset in json.loads(result.stdout)["assets"]}


def fetch(key: str, directory: Path) -> str:
    name, tag = archive_name(key), f"web-engine-{key}"
    published = published_assets(tag)
    if published is None or name not in published or published[name].get("state") != "uploaded":
        return "upload"
    digest = published[name].get("digest") or ""
    if not digest.startswith("sha256:"):
        raise ValueError(f"{tag}/{name}: GitHub reports no sha256 digest")
    directory.mkdir(parents=True, exist_ok=True)
    gh("release", "download", tag, "--pattern", name, "--dir", str(directory), "--clobber")
    actual = sha256(directory / name)
    if f"sha256:{actual}" != digest:
        raise ValueError(f"{tag}/{name}: downloaded sha256 {actual}, but GitHub reports {digest}")
    return "reuse"


def publish(directory: Path, commit: str, key: str) -> None:
    name, tag = archive_name(key), f"web-engine-{key}"
    archive = directory / name
    if not archive.is_file():
        raise ValueError(f"{archive}: no such runtime")
    want = f"sha256:{sha256(archive)}"
    published = published_assets(tag)
    asset = published.get(name) if published is not None else None
    if asset is not None and asset.get("state") == "uploaded":
        digest = asset.get("digest") or ""
        if digest != want:
            raise ValueError(f"{tag}/{name} is published with {digest or 'no digest'}, not this build's {want}: "
                             "shipped apps pin the published one")
        return
    if published is None:
        gh("release", "create", tag, "--target", commit, "--prerelease", "--latest=false",
           "--title", f"ngteams web engine {key[:12]}",
           "--notes", "The WPE WebKit runtime ngteams downloads for its sign-in window on Linux. "
                      "Never delete it while a shipped version pins it.")
    else:
        gh("release", "edit", tag, "--prerelease", "--latest=false")
        if asset is not None:
            # A failed upload can leave a `starter` asset: it was never downloadable or pinned by an app.
            gh("release", "delete-asset", tag, name, "--yes")
    gh("release", "upload", tag, "--", str(archive))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    fetched = sub.add_parser("fetch")
    fetched.add_argument("key")
    fetched.add_argument("directory", type=Path)
    published = sub.add_parser("publish")
    published.add_argument("directory", type=Path)
    published.add_argument("commit")
    published.add_argument("key")
    args = parser.parse_args()
    try:
        if args.command == "fetch":
            print(fetch(args.key, args.directory))
        else:
            publish(args.directory, args.commit, args.key)
    except (ValueError, OSError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
