"""The Nix flake's release file, written from a release's SHA256SUMS."""

from __future__ import annotations

import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

REPO = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("release_nix", REPO / "scripts/release-nix.py")
assert SPEC and SPEC.loader
release_nix = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release_nix)

HEX = "6f79ba410e0a74246eebcee082326180197f0820b9eb6c3c14d809382f2672c7"
SUMS = (
    f"{'0' * 64}  ngteams-0.1.0.142-linux-x86_64.AppImage\n"
    f"{HEX}  ngteams-0.1.0.142-linux-x86_64.tar.gz\n"
    f"{'1' * 64}  ngteams_0.1.0.142-1_amd64.deb\n"
)


class ReleaseNix(unittest.TestCase):
    def test_the_archive_url_and_its_sri_hash(self) -> None:
        self.assertEqual(
            release_nix.release("0.1.0.142", SUMS),
            {
                "version": "0.1.0.142",
                "x86_64-linux": {
                    "url": "https://github.com/nickalie/ngteams/releases/download/v0.1.0.142/"
                    "ngteams-0.1.0.142-linux-x86_64.tar.gz",
                    "hash": "sha256-b3m6QQ4KdCRu687ggjJhgBl/CCC562w8FNgJOC8mcsc=",
                },
            },
        )

    def test_a_binary_mode_line_and_upper_case_hex_count(self) -> None:
        sums = f"{HEX.upper()} *ngteams-0.1.0.142-linux-x86_64.tar.gz\n"
        self.assertEqual(release_nix.checksum(sums, "ngteams-0.1.0.142-linux-x86_64.tar.gz"), HEX)

    def test_a_missing_doubled_or_bad_line_is_refused(self) -> None:
        name = "ngteams-0.1.0.142-linux-x86_64.tar.gz"
        for sums in ("", f"{HEX}  {name}\n{HEX}  {name}\n", f"{'z' * 64}  {name}\n", f"{HEX}  {name}.sig\n"):
            with self.subTest(sums=sums), self.assertRaises(ValueError):
                release_nix.checksum(sums, name)

    def test_a_version_of_another_form_is_refused(self) -> None:
        for version in ("0.1.0", "v0.1.0.142", "0.1.0.142-1"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                release_nix.release(version, SUMS)

    def test_main_writes_the_file_only_when_every_check_passes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sums, output = Path(directory, "SHA256SUMS"), Path(directory, "release.json")
            sums.write_text(SUMS)
            self.assertEqual(release_nix.main(["x", "0.1.0.143", str(sums), str(output)]), 1)
            self.assertFalse(output.exists())
            self.assertEqual(release_nix.main(["x", "0.1.0.142", str(sums), str(output)]), 0)
            self.assertEqual(json.loads(output.read_text())["version"], "0.1.0.142")
            self.assertEqual(release_nix.main(["x"]), 2)

    def test_the_file_of_the_same_or_a_newer_release_stays(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            sums, output = Path(directory, "SHA256SUMS"), Path(directory, "release.json")
            output.write_text(json.dumps({"version": "0.1.0.142"}))
            for version in ("0.1.0.142", "0.1.0.99"):
                sums.write_text(SUMS.replace("0.1.0.142", version))
                self.assertEqual(release_nix.main(["x", version, str(sums), str(output)]), 0)
                self.assertEqual(json.loads(output.read_text()), {"version": "0.1.0.142"})
            # a file that holds no release is replaced
            output.write_text("not json")
            self.assertEqual(release_nix.main(["x", "0.1.0.99", str(sums), str(output)]), 0)
            self.assertEqual(json.loads(output.read_text())["version"], "0.1.0.99")


class ReleaseNixAsTheJobRunsIt(unittest.TestCase):
    """The script as the release's `nix` job runs it, on a copy of the committed release file (QA of task 580)."""

    def run_script(self, version: str, sums: str, output: Path) -> subprocess.CompletedProcess[str]:
        manifest = output.with_name("SHA256SUMS")
        manifest.write_text(sums)
        return subprocess.run(
            [sys.executable, "-B", str(REPO / "scripts/release-nix.py"), version, str(manifest), str(output)],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_a_newer_release_moves_the_flake_and_an_older_one_after_it_does_not(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory, "release.json")
            shutil.copy(REPO / "packaging/nix/release.json", output)
            committed = json.loads(output.read_text())["version"]
            newer = committed.rsplit(".", 1)[0] + "." + str(int(committed.rsplit(".", 1)[1]) + 1000)
            archive = f"ngteams-{newer}-linux-x86_64.tar.gz"
            digest = hashlib.sha256(b"made-up archive").hexdigest()
            sums = f"{'0' * 64}  ngteams-{newer}-linux-x86_64.AppImage\n{digest}  {archive}\n"
            done = self.run_script(newer, sums, output)
            self.assertEqual(done.returncode, 0, done.stderr)
            written = json.loads(output.read_text())
            self.assertEqual(written["version"], newer)
            self.assertEqual(
                written["x86_64-linux"]["url"],
                f"https://github.com/nickalie/ngteams/releases/download/v{newer}/{archive}",
            )
            self.assertEqual(
                written["x86_64-linux"]["hash"],
                "sha256-" + base64.b64encode(hashlib.sha256(b"made-up archive").digest()).decode(),
            )
            moved = output.read_text()
            # the run of the committed release finishes late: the flake stays on the newer one
            older = f"{digest}  ngteams-{committed}-linux-x86_64.tar.gz\n"
            done = self.run_script(committed, older, output)
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertIn("not changed", done.stdout)
            self.assertEqual(output.read_text(), moved)

    def test_a_release_without_its_linux_archive_fails_and_names_it(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory, "release.json")
            shutil.copy(REPO / "packaging/nix/release.json", output)
            before = output.read_text()
            sums = f"{'0' * 64}  ngteams-9.9.9.9-linux-x86_64.AppImage\n{'1' * 64}  ngteams_9.9.9.9-1_amd64.deb\n"
            done = self.run_script("9.9.9.9", sums, output)
            self.assertEqual(done.returncode, 1)
            self.assertIn("ngteams-9.9.9.9-linux-x86_64.tar.gz", done.stderr)
            self.assertNotIn("Traceback", done.stderr)
            self.assertEqual(output.read_text(), before)


if __name__ == "__main__":
    unittest.main()
