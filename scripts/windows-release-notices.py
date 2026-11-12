#!/usr/bin/env python3
"""Collect notices from the exact resolved Windows Cargo graph and libwebrtc build.

No compilation, application launch or account access. Cargo metadata must already
be available after a locked build. Never copy captures or arbitrary target files.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess

REPO = Path(__file__).resolve().parent.parent


def collect(output: Path, target: Path) -> None:
    if output.exists():
        raise ValueError("notice output already exists")
    graph = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"],
        cwd=REPO, encoding="utf-8"))
    packages = {p["id"]: p for p in graph["packages"]}
    nodes = {p["id"]: p for p in graph["resolve"]["nodes"]}
    pending = [p["id"] for p in packages.values() if p["name"] in ("ngteams", "teams-signin", "teams-ctl")]
    selected = set()
    while pending:
        key = pending.pop()
        if key in selected:
            continue
        selected.add(key)
        pending.extend(d["pkg"] for d in nodes[key]["deps"] if any(k["kind"] != "dev" for k in d["dep_kinds"]))
    output.mkdir(parents=True)
    rows = []
    for key in sorted(selected):
        package = packages[key]
        source = Path(package["manifest_path"]).parent
        destination = output / f"{package['name']}-{package['version']}"
        texts = [p for p in source.iterdir() if p.is_file() and
                 p.name.lower().startswith(("license", "licence", "copying", "notice", "copyright"))]
        if package.get("license_file"):
            texts.append(source / package["license_file"])
        for path in set(texts):
            destination.mkdir(exist_ok=True)
            shutil.copy2(path, destination / path.name)
        rows.append({k: package.get(k) for k in ("name", "version", "license", "repository", "source")})
    (output / "cargo-dependencies.json").write_text(json.dumps(rows, indent=2) + "\n", encoding="utf-8")
    # LiveKit's build crate extracts this upstream consolidated notice alongside its pinned static library.
    notices = list((target / "build").glob("scratch-*/out/livekit_webrtc/livekit/win-x64-release-*/win-x64-release/LICENSE.md"))
    if not notices or len({p.read_bytes() for p in notices}) != 1:
        raise ValueError("missing or ambiguous libwebrtc third-party notice")
    shutil.copy2(notices[0], output / "libwebrtc-LICENSE.md")
    for relative in ("crates/teams-ui/assets/icons/LICENSE-LUCIDE", "crates/teams-ui/assets/reactions/LICENSE-FLUENT-EMOJI",
                     "crates/teams-media/assets/segmentation/LICENSE-APACHE"):
        destination = output / "assets"
        destination.mkdir(exist_ok=True)
        shutil.copy2(REPO / relative, destination / Path(relative).name)
    print(f"collected notices for {len(rows)} resolved Cargo packages and libwebrtc")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", type=Path, required=True, help="Cargo profile output, e.g. target/release")
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    collect(args.output, args.target)
