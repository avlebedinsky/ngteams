"""The Nix flake's release file, written from a release's SHA256SUMS."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
