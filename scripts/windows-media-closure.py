#!/usr/bin/env python3
"""Assemble a private Windows x64 GStreamer runtime, or check an assembled tree.

Uses LLVM's PE reader (including delay imports), from Rust's llvm-tools-preview
component or --readobj. Never searches PATH/System32 for a dependency. The only
external imports permitted are declared Windows 11 DLLs and OS API-set contracts.
The SDK plugins and helper programs are explicit dlopen roots; all their non-OS
imports are copied recursively. All upstream license notices and exact component
versions travel with the runtime. App binaries may be checked with --binary.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PACKAGING = REPO / "packaging/windows"


def names(path: Path) -> list[str]:
    return [value for line in path.read_text(encoding="utf-8").splitlines()
            if (value := line.split("#", 1)[0].strip())]


def readobj_path() -> Path:
    if found := shutil.which("llvm-readobj"):
        return Path(found)
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], encoding="utf-8", cwd=REPO).strip()
    found = list((Path(sysroot) / "lib/rustlib").glob("*/bin/llvm-readobj*"))
    if len(found) != 1:
        raise ValueError("install rustup component llvm-tools-preview or pass --readobj")
    return found[0]


def imports(path: Path, reader: Path) -> list[str]:
    output = subprocess.check_output([str(reader), "--coff-imports", str(path)], encoding="utf-8")
    if "Format: COFF-x86-64" not in output:
        raise ValueError(f"not a Windows x64 PE: {path}")
    # LLVM prints both Import and DelayImport blocks with a Name field. Symbols have no Name fields.
    return [name.lower() for name in re.findall(r"^  Name: (\S+)\s*$", output, re.M)]


def index(directory: Path) -> dict[str, Path]:
    result = {}
    for path in sorted(directory.glob("*")):
        if path.is_file() and path.suffix.lower() == ".dll":
            key = path.name.lower()
            if key in result:
                raise ValueError(f"ambiguous DLL name: {path}")
            result[key] = path
    return result


def is_system(name: str) -> bool:
    return name in set(names(PACKAGING / "system-dlls.txt")) or bool(
        re.fullmatch(r"(?:api|ext)-ms-win-[a-z0-9-]+\.dll", name))


def closure(roots: dict[Path, Path], libraries: dict[str, Path], reader: Path) -> tuple[dict[Path, Path], set[str]]:
    found, system = dict(roots), set()
    pending = list(roots)
    while pending:
        path = pending.pop()
        for name in imports(path, reader):
            if is_system(name):
                system.add(name)
                continue
            if name not in libraries:
                raise ValueError(f"unresolved dependency {name} (imported by {path.name})")
            dependency = libraries[name]
            if dependency not in found:
                found[dependency] = Path("bin") / dependency.name
                pending.append(dependency)
    return found, system


def plugin_names() -> list[str]:
    plugins = names(PACKAGING / "gstreamer-plugins.txt")
    if len(set(plugins)) != len(plugins) or any(not re.fullmatch("[a-z0-9]+", p) for p in plugins):
        raise ValueError("invalid/duplicate plugin name")
    return plugins


def seed(sdk: Path) -> dict[Path, Path]:
    relative = [Path(f"lib/gstreamer-1.0/gst{name}.dll") for name in plugin_names()]
    relative += [Path(f"bin/{name}.dll") for name in
                 ("gstreamer-1.0-0", "gstapp-1.0-0", "gstaudio-1.0-0", "gstvideo-1.0-0")]
    relative += [Path("libexec/gstreamer-1.0/gst-plugin-scanner.exe"),
                 Path("bin/gst-inspect-1.0.exe"), Path("bin/gst-launch-1.0.exe")]
    for path in relative:
        if not (sdk / path).is_file():
            raise ValueError(f"missing runtime root: {path}")
    return {sdk / path: path for path in relative}


def digest(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def check(root: Path, reader: Path, binaries: list[Path]) -> None:
    manifest = json.loads((root / "MEDIA-MANIFEST.json").read_text(encoding="utf-8"))
    if manifest["plugins"] != plugin_names():
        raise ValueError("stale runtime: plugin inventory changed")
    expected = manifest["files"]
    actual = {str(p.relative_to(root).as_posix()) for p in root.rglob("*") if p.is_file()
              and p.name != "MEDIA-MANIFEST.json"}
    if set(expected) != actual:
        raise ValueError(f"runtime file inventory differs: {sorted(set(expected) ^ actual)}")
    for relative, sha256 in expected.items():
        if digest(root / relative) != sha256:
            raise ValueError(f"runtime hash mismatch: {relative}")
    files = {root / name: Path(name) for name in expected if Path(name).suffix.lower() in (".dll", ".exe")}
    files.update({path: Path("bin") / path.name for path in binaries})
    _, system = closure(files, index(root / "bin"), reader)
    if not system.issubset(set(manifest["system_imports"])) and not binaries:
        raise ValueError("runtime imports differ from manifest")
    print(f"checked {len(files)} PE files, {len(expected)} hashed runtime/license files")


def package(args: argparse.Namespace, reader: Path) -> None:
    sdk, root = args.sdk.resolve(), args.output.resolve()
    if root.exists():
        raise ValueError(f"output already exists (use a new destination): {root}")
    versions = (sdk / "share/versions.txt").read_text(encoding="utf-8")
    pinned = json.loads((PACKAGING / "media-pins.json").read_text(encoding="utf-8"))["gstreamer_version"]
    if not versions.startswith(f"cerbero {pinned}\n"):
        raise ValueError(f"expected the pinned GStreamer {pinned} SDK")
    libraries = index(sdk / "bin")
    for name, path in index(args.crt).items():
        if name in libraries:
            raise ValueError(f"SDK/CRT DLL conflict: {name}")
        libraries[name] = path
    files, system = closure(seed(sdk), libraries, reader)
    # Resolve/check extra app dependencies without copying the app (lab builds stay in their runner's tree).
    extra, extra_system = closure({p: Path("bin") / p.name for p in args.binary}, libraries, reader)
    files.update({p: dest for p, dest in extra.items() if p not in args.binary})
    system.update(extra_system)
    notices = sdk / "share/licenses"
    if not (notices / "gstreamer-1.0/LGPL-2.0-or-later.txt").is_file():
        raise ValueError("missing SDK license texts")
    if not args.crt_license.is_file():
        raise ValueError("missing Microsoft VC runtime license")
    files.update({p: Path("share/licenses") / p.relative_to(notices) for p in notices.rglob("*") if p.is_file()})
    files[sdk / "share/versions.txt"] = Path("share/versions.txt")
    files[args.crt_license] = Path("share/licenses/microsoft-vc-runtime") / args.crt_license.name
    size = sum(p.stat().st_size for p in files)
    if shutil.disk_usage(root.parent).free - size < 1536 * 1024**2:
        raise ValueError(f"runtime needs {size} bytes plus the 1.5 GiB free-space guard")
    if args.plan:
        print(f"{len(files)} files, {size} bytes, {len(system)} Windows system imports")
        return
    try:
        for source, relative in sorted(files.items()):
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
        manifest = {"gstreamer": pinned, "arch": "x86_64", "plugins": plugin_names(),
                    "source_recipes": f"https://gitlab.freedesktop.org/gstreamer/cerbero/-/tree/{pinned}/recipes",
                    "system_imports": sorted(system),
                    "files": {dest.as_posix(): digest(source) for source, dest in sorted(files.items())}}
        (root / "MEDIA-MANIFEST.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        check(root, reader, args.binary)
    except BaseException:
        shutil.rmtree(root)
        raise
    print(f"packaged {size} bytes in {root}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--readobj", type=Path)
    sub = parser.add_subparsers(dest="command", required=True)
    build = sub.add_parser("package")
    build.add_argument("--sdk", type=Path, required=True)
    build.add_argument("--crt", type=Path, required=True, help="VS Redist x64/Microsoft.VC143.CRT")
    build.add_argument("--crt-license", type=Path, required=True)
    build.add_argument("--plan", action="store_true")
    build.add_argument("--binary", type=Path, action="append", default=[])
    build.add_argument("output", type=Path)
    verify = sub.add_parser("check")
    verify.add_argument("root", type=Path)
    verify.add_argument("--binary", type=Path, action="append", default=[])
    args = parser.parse_args()
    try:
        reader = args.readobj or readobj_path()
        if args.command == "package":
            package(args, reader)
        else:
            check(args.root, reader, args.binary)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
