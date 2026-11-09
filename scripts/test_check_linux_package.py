"""The app package's ELF boundaries, on tiny real fixtures with no release build or installed WPE engine."""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

REPO = Path(__file__).resolve().parent.parent
CHECK = REPO / "scripts/check-linux-package.py"
BINARIES = ("ngteams", "ngteamsctl", "ngteams-web")
EXTENSION = "libteams_signin_extension.so"


def run(*args, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run([str(arg) for arg in args], text=True, capture_output=True, **kwargs)


@unittest.skipUnless(all(shutil.which(tool) for tool in ("cc", "patchelf", "ldd")), "ELF fixtures require cc, patchelf and ldd")
class CheckLinuxPackage(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.temp = Path(directory.name)
        self.root = self.temp / "app"
        (self.root / "usr/bin").mkdir(parents=True)
        for name in BINARIES:
            self.program(self.root / "usr/bin" / name)
        self.shared(self.root / "usr/bin" / EXTENSION, "libteams_signin_extension.so")

    def program(self, path: Path, *flags: str) -> None:
        result = run("cc", "-x", "c", "-", "-o", path, "-Wl,--no-as-needed", *flags, input="int main(void) { return 0; }")
        self.assertEqual(result.returncode, 0, result.stderr)

    def shared(self, path: Path, soname: str) -> None:
        result = run("cc", "-shared", "-fPIC", "-x", "c", "-", "-o", path, f"-Wl,-soname,{soname}",
                     input="int fixture(void) { return 7; }")
        self.assertEqual(result.returncode, 0, result.stderr)

    def check(self, *args: str) -> subprocess.CompletedProcess:
        return run(sys.executable, CHECK, *args, self.root)

    def link(self, binary: str, soname: str) -> None:
        # Outside the app tree: a DT_NEEDED check must catch even an unbundled engine.
        library = self.temp / soname
        self.shared(library, soname)
        self.program(self.root / "usr/bin" / binary, "-x", "none", str(library))

    def test_a_clean_tree_passes(self) -> None:
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_the_app_and_ctl_must_not_link_the_engine(self) -> None:
        for soname in ("libWPEWebKit-2.0.so.1", "libWPEPlatform-2.0.so.1", "libsoup-3.0.so.0",
                       "libjavascriptcoregtk-4.1.so.0", "libJavaScriptCore-2.0.so.1"):
            for binary in ("ngteams", "ngteamsctl"):
                with self.subTest(binary=binary, soname=soname):
                    self.link(binary, soname)
                    result = self.check()
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(f"{binary} needs web engine library: {soname}", result.stderr)
                    self.program(self.root / "usr/bin" / binary)

    def test_a_needed_library_must_be_on_the_allowed_list(self) -> None:
        self.link("ngteams", "libnotes-1.0.so.0")
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ngteams needs a library not in the allowed list: libnotes-1.0.so.0", result.stderr)

    def test_ngteams_may_link_gstreamer(self) -> None:
        self.link("ngteams", "libgstreamer-1.0.so.0")
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_ctl_may_not_link_gstreamer(self) -> None:
        self.link("ngteamsctl", "libgstreamer-1.0.so.0")
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ngteamsctl needs a library not in the allowed list", result.stderr)

    def test_no_shipped_file_may_have_a_search_path(self) -> None:
        for name in ("ngteams-web", "ngteams", "ngteamsctl", EXTENSION):
            for flag in ([], ["--force-rpath"]):
                with self.subTest(name=name, flag=flag):
                    path = self.root / "usr/bin" / name
                    self.assertEqual(run("patchelf", *flag, "--set-rpath", "/build/wpe/lib", path).returncode, 0)
                    result = self.check()
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(f"{name} must have no RPATH or RUNPATH: /build/wpe/lib", result.stderr)
                    self.assertEqual(run("patchelf", "--remove-rpath", path).returncode, 0)

    def test_preparation_clears_only_the_workers_search_path(self) -> None:
        worker = self.root / "usr/bin/ngteams-web"
        self.assertEqual(run("patchelf", "--set-rpath", "/build/wpe/lib", worker).returncode, 0)
        result = self.check("--prepare")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(run("patchelf", "--print-rpath", worker).stdout.strip(), "")

    def test_preparation_leaves_the_app_search_path_for_the_check_to_reject(self) -> None:
        app = self.root / "usr/bin/ngteams"
        self.assertEqual(run("patchelf", "--set-rpath", "/build/wpe/lib", app).returncode, 0)
        self.assertNotEqual(self.check("--prepare").returncode, 0)

    def test_a_missing_file_is_an_error(self) -> None:
        (self.root / "usr/bin" / EXTENSION).unlink()
        result = self.check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"missing app file: {EXTENSION}", result.stderr)

    def test_engine_payload_cannot_enter_the_tree(self) -> None:
        for payload in ("usr/lib/libWPEWebKit-2.0.so.1", "usr/lib/libgstapp-1.0.so.0", "usr/lib/libsoup-3.0.so.0",
                        "usr/lib/gstreamer-1.0/plugin.so", "usr/lib/wpe-webkit-2.0/WPEWebProcess",
                        "usr/share/ngteams/bundled-wpe"):
            with self.subTest(payload=payload):
                path = self.root / payload
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("not an engine, just a name\n")
                result = self.check()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("web engine payload in app package", result.stderr)
                path.unlink()

    def test_no_library_can_be_bundled(self) -> None:
        for relative in ("usr/lib/libglib-2.0.so.0", "usr/bin/libstuff.so.1"):
            with self.subTest(relative=relative):
                path = self.root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                self.shared(path, "libstuff.so.1")
                result = self.check()
                self.assertNotEqual(result.returncode, 0)
                self.assertRegex(result.stderr, "ships no libraries|unexpected ELF file")
                path.unlink()

    def test_a_tree_of_non_elf_payload_files_is_fine(self) -> None:
        (self.root / "usr/share/icons/hicolor/48x48/apps").mkdir(parents=True)
        (self.root / "usr/share/icons/hicolor/48x48/apps/com.nickalie.ngteams.png").write_bytes(b"\x89PNG")
        (self.root / "AppRun").write_text("#!/usr/bin/env bash\n")
        result = self.check()
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
