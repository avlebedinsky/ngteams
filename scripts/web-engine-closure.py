#!/usr/bin/env python3
"""Package the release WPE runtime downloaded at first sign-in.

Only the Ubuntu release runner builds real archives. --system-root and
--multiarch let the tests supply a tiny distro tree of real ELF fixtures.
HOST-LIBRARIES records the external SONAMEs checked against runtime-libraries.txt;
PACKAGES records the exact Ubuntu package versions contributing copied files.
"""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile


REPO = Path(__file__).resolve().parent.parent
GLIBC = {"libc.so.6", "libm.so.6", "libmvec.so.1", "libresolv.so.2", "libpthread.so.0",
         "libdl.so.2", "librt.so.1", "libutil.so.1", "libanl.so.1", "ld-linux-x86-64.so.2"}
GIO = ("libgiognutls.so", "libgiolibproxy.so", "libgiognomeproxy.so", "libdconfsettings.so")
ARCH = "x86_64"


def run(*args: str | Path, env: dict | None = None) -> str:
    return subprocess.check_output([str(arg) for arg in args], env=env, text=True).strip()


def names(path: Path) -> list[str]:
    return [line.split("#", 1)[0].strip() for line in path.read_text().splitlines()
            if line.split("#", 1)[0].strip()]


def elf(path: Path) -> bool:
    if not path.is_file() or path.is_symlink():
        return False
    with path.open("rb") as source:
        return source.read(4) == b"\x7fELF"


def dependencies(path: Path, env: dict) -> list[tuple[str, Path]]:
    result = subprocess.run(["ldd", str(path)], env=env, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True, check=False)
    if result.returncode or "not found" in result.stdout:
        raise ValueError(f"unresolved dependencies: {path}\n{result.stdout}")
    # Some ldd versions print the interpreter as /lib64/ld-linux... => /usr/...
    # rather than a standalone path. It is a host glibc component either way.
    return [(Path(name).name, Path(resolved)) for name, resolved in
            re.findall(r"\s+(\S+)\s+=>\s+(/\S+)", result.stdout)]


def host_allowed(allowed_file: Path) -> set[str]:
    return set(names(allowed_file)) | GLIBC


def patch_and_check(root: Path, library: Path, allowed_file: Path, *,
                    exclude: tuple[Path, ...] = ()) -> set[str]:
    """Give every ELF not in exclude a relative RUNPATH; reject undeclared host fallbacks.

    exclude contains resolved paths skipped by both patching and the dependency
    closure check, including the rejection of undeclared host libraries.
    Host packages bring their own dependency closure (Wayland's libffi, for
    example). Preserve the release packager's rule permitting those dependencies.
    """
    root, library = root.resolve(), library.resolve()
    files = sorted(path for path in root.rglob("*") if elf(path) and path not in exclude)
    for path in files:
        relative = os.path.relpath(library, path.parent)
        rpath = "$ORIGIN" if relative == "." else f"$ORIGIN:$ORIGIN/{relative}"
        subprocess.run(["patchelf", "--set-rpath", rpath, str(path)], check=True)
    environment = os.environ.copy()
    environment.pop("LD_LIBRARY_PATH", None)
    needed = {path: dependencies(path, environment) for path in files}
    allowed = host_allowed(allowed_file)
    system = {resolved for entries in needed.values() for name, resolved in entries
              if name in allowed and not resolved.resolve().is_relative_to(root)}
    for path in sorted(system):
        allowed.update(name for name, _ in dependencies(path, environment))
    external = set()
    for path, entries in needed.items():
        for name, resolved in entries:
            if not resolved.resolve().is_relative_to(root):
                if name not in allowed:
                    raise ValueError(f"undeclared host library: {name} ({path})")
                external.add(name)
    return external


def seeds(prefix: Path, system: Path, multiarch: str) -> dict[Path, Path]:
    """Sources mapped to their unique runtime location, including dlopen roots."""
    library = system / "usr/lib" / multiarch
    found = {path: Path("lib") / path.name for path in sorted((prefix / "lib").glob("*.so*"))}
    helpers = prefix / "lib/wpe-webkit-2.0"
    found.update({path: Path("lib/wpe-webkit-2.0") / path.relative_to(helpers)
                  for path in sorted(helpers.rglob("*")) if path.is_file()})
    for plugin in names(REPO / "packaging/linux/gstreamer-plugins.txt"):
        if not re.fullmatch(r"[a-z0-9]+", plugin):
            raise ValueError(f"invalid plugin name: {plugin}")
        found[library / f"gstreamer-1.0/libgst{plugin}.so"] = Path(f"lib/gstreamer-1.0/libgst{plugin}.so")
    for module in GIO:
        found[library / "gio/modules" / module] = Path("lib/gio/modules") / module
    scanner = "gstreamer-1.0/gst-plugin-scanner"
    found[library / "gstreamer1.0" / scanner] = Path("libexec") / scanner
    required = [prefix / "lib/libWPEWebKit-2.0.so.1", helpers / "WPEWebProcess",
                helpers / "WPENetworkProcess"]
    for path in [*found, *required]:
        if not path.is_file():
            raise ValueError(f"missing runtime input: {path}")
    if not (helpers / "injected-bundle").is_dir():
        raise ValueError(f"missing injected bundle: {helpers}")
    return found


def closure(inputs: dict[Path, Path], prefix: Path) -> dict[Path, Path]:
    environment = os.environ.copy()
    environment["LD_LIBRARY_PATH"] = str(prefix / "lib")
    allowed = host_allowed(REPO / "packaging/linux/runtime-libraries.txt")
    found = dict(inputs)
    for path in inputs:
        if not elf(path.resolve()):
            continue
        for name, resolved in dependencies(path, environment):
            if name not in allowed:
                found.setdefault(resolved, Path("lib") / name)
    return found


def package_versions(files: list[Path], prefix: Path) -> dict[str, str]:
    packages = set()
    for path in sorted({path.resolve() for path in files if not path.resolve().is_relative_to(prefix)}):
        # Ubuntu's usrmerge packages may still record /lib instead of /usr/lib.
        result = subprocess.run(["dpkg-query", "-S", str(path)], text=True, capture_output=True, check=False)
        if result.returncode and str(path).startswith("/usr/"):
            result = subprocess.run(["dpkg-query", "-S", str(path)[4:]], text=True, capture_output=True, check=False)
        if result.returncode:
            raise ValueError(f"no Ubuntu package owns runtime input: {path}")
        packages.update(line.rsplit(": ", 1)[0] for line in result.stdout.splitlines())
    versions = {}
    for package in sorted(packages):
        name, version = run("dpkg-query", "-W", "-f=${binary:Package}\t${Version}\n", package).split("\t")
        versions[name] = version
    return versions


def pinned_digest() -> str:
    pins = (REPO / "packaging/release/pins.env").read_text()
    match = re.search(r'^RELEASE_WPE_DIGEST="?([^"\n]+)', pins, re.M)
    if not match:
        raise ValueError("no RELEASE_WPE_DIGEST in packaging/release/pins.env")
    return match[1]


def runtime_key(prefix: Path, versions: dict[str, str]) -> str:
    digest_pin = pinned_digest()
    stamp = prefix / ".release-wpe-digest"
    if not stamp.is_file() or stamp.read_text().strip() != digest_pin:
        raise ValueError("release WPE prefix is stale; run scripts/release-wpe-pull.sh first")
    digest = hashlib.sha256()
    for item in (digest_pin, ARCH, package_list(versions)):
        digest.update(item.encode() + b"\0")
    for name in ("packaging/linux/gstreamer-plugins.txt", "packaging/linux/runtime-libraries.txt",
                 "scripts/package-web-engine.sh", "scripts/web-engine-closure.py"):
        digest.update(name.encode() + b"\0" + (REPO / name).read_bytes() + b"\0")
    return digest.hexdigest()


def wpe_version(prefix: Path) -> str:
    match = re.search(r"^Version:\s*(\S+)", (prefix / "lib/pkgconfig/wpe-webkit-2.0.pc").read_text(), re.M)
    if not match:
        raise ValueError(f"no Version in {prefix}/lib/pkgconfig/wpe-webkit-2.0.pc")
    return match[1]


def package_list(versions: dict[str, str]) -> str:
    return "".join(f"{name}\t{version}\n" for name, version in sorted(versions.items()))


def copy_unique(inputs: dict[Path, Path], root: Path) -> None:
    copied: dict[Path, Path] = {}
    destinations: dict[Path, Path] = {}
    for source, relative in inputs.items():
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError(f"unsafe runtime destination: {relative}")
        original = source.resolve()
        target = root / relative
        if target in destinations:
            if destinations[target] != original:
                raise ValueError(f"conflicting runtime library: {relative}")
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        # Store the real file once; all SONAME/unversioned aliases are relative
        # symlinks, including any reference to a nested dlopen module.
        if original not in copied:
            canonical = target.parent / original.name
            if canonical in destinations and destinations[canonical] != original:
                raise ValueError(f"conflicting runtime library: {canonical}")
            shutil.copy2(original, canonical)
            canonical.chmod(canonical.stat().st_mode & ~0o6000)
            copied[original] = canonical
            destinations[canonical] = original
        canonical = copied[original]
        if target != canonical:
            target.symlink_to(os.path.relpath(canonical, target.parent))
        destinations[target] = original


def licenses(root: Path, prefix: Path, system: Path, versions: dict[str, str]) -> None:
    shutil.copytree(prefix / "share/doc/wpewebkit", root / "share/doc/wpewebkit", symlinks=False)
    for package in versions:
        name = package.split(":", 1)[0]
        copyright_file = system / "usr/share/doc" / name / "copyright"
        if not copyright_file.is_file():
            raise ValueError(f"missing copyright for {package}: {copyright_file}")
        target = root / "share/doc" / name
        target.mkdir(parents=True, exist_ok=True)
        shutil.copy2(copyright_file, target / "copyright")
    shutil.copytree(system / "usr/share/common-licenses", root / "share/common-licenses", symlinks=False)


def package(args: argparse.Namespace) -> None:
    prefix = args.prefix.resolve()
    if platform.machine() != ARCH:
        raise ValueError(f"the release runtime is built for {ARCH}, not {platform.machine()}")
    multiarch = args.multiarch or run("gcc", "-print-multiarch")
    inputs = closure(seeds(prefix, args.system_root, multiarch), prefix)
    common = args.system_root / "usr/share/common-licenses"
    versions = package_versions([*inputs, *(p for p in common.rglob("*") if p.is_file())], prefix)
    key = runtime_key(prefix, versions)
    if args.print_key:
        print(key)
        return
    if args.output is None:
        raise ValueError("an output directory or --print-key is required")
    args.output.mkdir(parents=True, exist_ok=True)
    # A throwaway tree is a build intermediate; the output archive is durable.
    with tempfile.TemporaryDirectory(prefix="ngteams-web-engine-") as work:
        root = Path(work)
        copy_unique(inputs, root)
        licenses(root, prefix, args.system_root, versions)
        hosts = patch_and_check(root, root / "lib", REPO / "packaging/linux/runtime-libraries.txt")
        (root / "HOST-LIBRARIES").write_text("".join(f"{name}\n" for name in sorted(hosts)))
        (root / "PACKAGES").write_text(package_list(versions))
        (root / "MANIFEST").write_text(f"key={key}\narch={ARCH}\nwpe_version={wpe_version(prefix)}\n")
        archive = args.output.resolve() / f"ngteams-web-engine-{key}-linux-{ARCH}.tar.zst"
        subprocess.run(["tar", "--sort=name", "--mtime=@0", "--owner=0", "--group=0", "--numeric-owner",
                        "-I", "zstd -19 --long=27 -T1", "-cf", str(archive), "-C", str(root), "."], check=True)
    with archive.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    print(f"{archive}\nsize={archive.stat().st_size}\nsha256={digest}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    check = sub.add_parser("check", help="patch RUNPATHs and reject undeclared host libraries")
    check.add_argument("root", type=Path)
    check.add_argument("library", type=Path)
    check.add_argument("allowed", type=Path)
    build = sub.add_parser("package")
    build.add_argument("--prefix", type=Path, required=True)
    build.add_argument("--system-root", type=Path, default=Path("/"))
    build.add_argument("--multiarch")
    build.add_argument("--print-key", action="store_true")
    build.add_argument("output", type=Path, nargs="?")
    args = parser.parse_args()
    try:
        if args.command == "check":
            patch_and_check(args.root, args.library, args.allowed)
        else:
            package(args)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
