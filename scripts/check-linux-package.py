#!/usr/bin/env python3
"""Keep the Linux app packages free of the separately installed web engine.

The app and ngteamsctl must not link the engine and need only the host libraries
named here; the worker must not carry a library search path, as it picks the
system's engine or a downloaded runtime when it starts. --prepare strips the worker's
build-time RUNPATH before checking; a plain run checks a finished tree unchanged.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import subprocess


WORKER = "ngteams-web"
EXTENSION = "libteams_signin_extension.so"
APP_BINARIES = ("ngteams", "ngteamsctl")
SHIPPED_FILES = {*APP_BINARIES, WORKER, EXTENSION}
ENGINE_LIBRARIES = ("libwpe", "libsoup", "libjavascriptcore")
GLIBC = {"libc.so.6", "libm.so.6", "libdl.so.2", "libpthread.so.0", "librt.so.1",
         "libresolv.so.2", "ld-linux-x86-64.so.2"}
# Every library ngteams may need from the host: the GUI's and the media stack's. Anything else must be a deliberate
# addition here and in packaging/linux/nfpm.yaml's dependencies.
ALLOWED_NEEDED = {
    "ngteams": GLIBC | {"libgcc_s.so.1", "libxkbcommon.so.0", "libgio-2.0.so.0", "libgobject-2.0.so.0",
                        "libglib-2.0.so.0", "libgstreamer-1.0.so.0", "libgstbase-1.0.so.0", "libgstapp-1.0.so.0",
                        "libpipewire-0.3.so.0"},
    "ngteamsctl": GLIBC | {"libgcc_s.so.1"},
}


def output(*args: str | Path) -> str:
    return subprocess.check_output([str(arg) for arg in args], text=True).strip()


def is_elf(path: Path) -> bool:
    if path.is_symlink() or not path.is_file():
        return False
    with path.open("rb") as source:
        return source.read(4) == b"\x7fELF"


def engine_library(name: str) -> bool:
    return name.lower().startswith(ENGINE_LIBRARIES)


def check_needed(root: Path, binary: str) -> None:
    for library in output("patchelf", "--print-needed", root / "usr/bin" / binary).splitlines():
        if engine_library(library):
            raise ValueError(f"{binary} needs web engine library: {library}")
        if library not in ALLOWED_NEEDED[binary]:
            raise ValueError(f"{binary} needs a library not in the allowed list: {library}")


def check(root: Path) -> None:
    for name in sorted(SHIPPED_FILES):
        if not (root / "usr/bin" / name).is_file():
            raise ValueError(f"missing app file: {name}")
    for binary in APP_BINARIES:
        check_needed(root, binary)
    # Reject both RUNPATH and legacy RPATH, including absolute build prefixes.
    for name in sorted(SHIPPED_FILES):
        rpath = output("patchelf", "--print-rpath", root / "usr/bin" / name)
        if rpath:
            raise ValueError(f"{name} must have no RPATH or RUNPATH: {rpath}")
    for path in sorted(root.rglob("*")):
        name = path.name
        if engine_library(name) or name.startswith("libgst") or name in ("gstreamer-1.0", "wpe-webkit-2.0", "bundled-wpe"):
            raise ValueError(f"web engine payload in app package: {path.relative_to(root)}")
        if is_elf(path) and path.parent != root / "usr/bin":
            raise ValueError(f"the app package ships no libraries: {path.relative_to(root)}")
        if is_elf(path) and name not in SHIPPED_FILES:
            raise ValueError(f"unexpected ELF file in the app package: {path.relative_to(root)}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare", action="store_true")
    parser.add_argument("root", type=Path)
    args = parser.parse_args()
    try:
        root = args.root.resolve()
        if args.prepare:
            subprocess.run(["patchelf", "--remove-rpath", str(root / "usr/bin" / WORKER)], check=True)
        check(root)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
