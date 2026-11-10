"""Windows package boundary tests with real x64 PE normal/delay imports, no app or tenant."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import unittest
from unittest.mock import patch
import zipfile

import test_windows_media_closure as pe

SPEC = importlib.util.spec_from_file_location("release", Path(__file__).with_name("windows-release.py"))
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


@unittest.skipUnless(shutil.which("clang") and shutil.which("rustc"), "PE fixtures need clang and Rust LLD")
class WindowsRelease(unittest.TestCase):
    dll = pe.WindowsMediaClosure.dll
    # Reuse the real PE constructor, not the media suite's test inventory.
    def setUp(self):
        pe.WindowsMediaClosure.setUp(self)
        self.runtime = self.root / "runtime"
        (self.runtime / "bin").mkdir(parents=True)
        for name in ("Leaf.dll", "Middle.dll"):
            shutil.copy2(self.root / name, self.runtime / "bin" / name)
        manifest = {"plugins": release.media.plugin_names(), "system_imports": [],
                    "files": {f"bin/{name}": release.media.digest(self.runtime / "bin" / name)
                              for name in ("Leaf.dll", "Middle.dll")}}
        (self.runtime / "MEDIA-MANIFEST.json").write_text(json.dumps(manifest))
        self.binaries = self.root / "binaries"
        self.binaries.mkdir()
        for name in release.BINARIES:
            shutil.copy2(self.root / "Top.dll", self.binaries / name)
        self.notices = self.root / "notices"
        self.notices.mkdir()
        (self.notices / "license.txt").write_text("Synthetic package license fixture\n")
        self.args = argparse.Namespace(version="0.1.0.123", commit="a" * 40, runtime=self.runtime,
            binaries=self.binaries, output=self.root / "package", archive=self.root / "app.zip", notices=self.notices)

    def test_archive_contains_the_verified_tree_and_setup(self):
        # Cargo registry notice files can have the reproducible Unix epoch timestamp.
        os.utime(self.notices / "license.txt", (0, 0))
        release.stage(self.args, self.reader)
        release.check(self.args.output, self.reader, self.args.version, self.args.commit)
        with zipfile.ZipFile(self.args.archive) as archive:
            names = set(archive.namelist())
            self.assertIn("ngteams/bin/ngteams-webview.exe", names)
            self.assertIn("ngteams/Install.ps1", names)
            self.assertIn("ngteams/share/licenses/ngteams/dependencies/license.txt", names)
            for path in self.args.output.rglob("*"):
                if path.is_file():
                    self.assertEqual(archive.read("ngteams/" + path.relative_to(self.args.output).as_posix()), path.read_bytes())
        for name, value in (("version", "0.1.0.124"), ("commit", "b" * 40)):
            with self.assertRaisesRegex(ValueError, f"{name} mismatch"):
                release.check(self.args.output, self.reader, **{name: value})
        (self.args.output / "bin/ngteams-webview.exe").write_bytes(b"corrupt")
        with self.assertRaisesRegex(ValueError, "inventory or hashes"):
            release.check(self.args.output, self.reader)

    def test_missing_app_or_transitive_import_refuses_public_archive(self):
        (self.binaries / "ngteams-webview.exe").unlink()
        with self.assertRaisesRegex(ValueError, "missing built"):
            release.stage(self.args, self.reader)
        self.assertFalse(self.args.archive.exists())
        shutil.copy2(self.root / "Top.dll", self.binaries / "ngteams-webview.exe")
        # The runtime checks on its own, but the extra application needs Middle.dll.
        (self.runtime / "bin/Middle.dll").unlink()
        manifest = json.loads((self.runtime / "MEDIA-MANIFEST.json").read_text())
        del manifest["files"]["bin/Middle.dll"]
        (self.runtime / "MEDIA-MANIFEST.json").write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "unresolved dependency middle.dll"):
            release.stage(self.args, self.reader)
        self.assertFalse(self.args.archive.exists())
        self.assertFalse(self.args.output.exists())

    def test_low_space_and_invalid_metadata_refuse_before_copy(self):
        with patch.object(release.shutil, "disk_usage", return_value=shutil._ntuple_diskusage(100, 99, 1)):
            with self.assertRaisesRegex(ValueError, "1.5 GiB"):
                release.stage(self.args, self.reader)
        self.assertFalse(self.args.output.exists())
        self.args.version = "../../invalid"
        with self.assertRaisesRegex(ValueError, "version must"):
            release.stage(self.args, self.reader)

    def test_existing_output_is_never_overwritten(self):
        self.args.archive.write_text("keep")
        with self.assertRaisesRegex(ValueError, "already exists"):
            release.stage(self.args, self.reader)
        self.assertEqual(self.args.archive.read_text(), "keep")


# Importing the shared test class would discover its tests twice. Select only this suite's new contracts; its
# inherited constructors/support remain available, and the media suite runs separately in the workflow.
if __name__ == "__main__":
    suite = unittest.TestSuite(WindowsRelease(name) for name in WindowsRelease.__dict__ if name.startswith("test_"))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    raise SystemExit(not result.wasSuccessful())
