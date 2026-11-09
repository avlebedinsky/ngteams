"""The runtime packager on tiny ELF fixtures, with no WPE build or GUI."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

REPO = Path(__file__).resolve().parent.parent
DIGEST = "sha256:" + "ab" * 32


def run(*args, **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run([str(arg) for arg in args], text=True, capture_output=True, **kwargs)


def shared(path: Path, source: str = "int fixture(void) { return 7; }", *flags: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    result = run("cc", "-shared", "-fPIC", "-x", "c", "-", "-o", path, *flags, input=source)
    if result.returncode:
        raise AssertionError(result.stderr)


@unittest.skipUnless(all(shutil.which(tool) for tool in ("cc", "ldd", "patchelf", "zstd")),
                     "ELF fixtures require cc, ldd, patchelf and zstd")
class WebEngineClosure(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name) / "checkout"
        for name in ("scripts/package-web-engine.sh", "scripts/web-engine-closure.py", "packaging/release/pins.env",
                     "packaging/linux/gstreamer-plugins.txt", "packaging/linux/runtime-libraries.txt"):
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(REPO / name, target)
        pins = self.root / "packaging/release/pins.env"
        pins.write_text(re.sub(r'^RELEASE_WPE_DIGEST=.*$', f'RELEASE_WPE_DIGEST="{DIGEST}"', pins.read_text(), flags=re.M))
        (self.root / "packaging/linux/gstreamer-plugins.txt").write_text("playback\nlibav\n")
        prefix = self.root / "wpe"
        library = prefix / "lib"
        shared(library / "libWPEWebKit-2.0.so.1.14.0")
        (library / "libWPEWebKit-2.0.so.1").symlink_to("libWPEWebKit-2.0.so.1.14.0")
        (library / "libWPEWebKit-2.0.so").symlink_to("libWPEWebKit-2.0.so.1")
        (library / "pkgconfig").mkdir()
        (library / "pkgconfig/wpe-webkit-2.0.pc").write_text("Name: wpe-webkit-2.0\nVersion: 2.54.1\n")
        helpers = library / "wpe-webkit-2.0"
        helpers.mkdir()
        for name in ("WPEWebProcess", "WPENetworkProcess"):
            shutil.copy2("/bin/true", helpers / name)
        shared(helpers / "injected-bundle/libWPEInjectedBundle.so")
        docs = prefix / "share/doc/wpewebkit"
        docs.mkdir(parents=True)
        (docs / "LICENSE").write_text("WPE fixture license\n")
        system = self.root / "system/usr"
        lib = system / "lib/fixture"
        dependency = lib / "libengine-dependency.so.1.2"
        shared(dependency, "int dependency(void) { return 42; }", "-Wl,-soname,libengine-dependency.so.1")
        (lib / "libengine-dependency.so.1").symlink_to(dependency.name)
        (lib / "libengine-dependency.so").symlink_to(dependency.name)
        for name in ("playback", "libav", "offlist"):
            shared(lib / f"gstreamer-1.0/libgst{name}.so",
                   "extern int dependency(void); int plugin(void) { return dependency(); }",
                   f"-L{lib}", "-lengine-dependency", f"-Wl,-rpath,{lib}")
        for name in ("libgiognutls", "libgiolibproxy", "libgiognomeproxy", "libdconfsettings"):
            shared(lib / f"gio/modules/{name}.so")
        scanner = lib / "gstreamer1.0/gstreamer-1.0/gst-plugin-scanner"
        scanner.parent.mkdir(parents=True)
        shutil.copy2("/bin/true", scanner)
        (system / "share/doc/fixture-package").mkdir(parents=True)
        (system / "share/doc/fixture-package/copyright").write_text("Fixture copyright\n")
        (system / "share/common-licenses").mkdir()
        (system / "share/common-licenses/LGPL-2.1").write_text("Fixture LGPL\n")
        tools = self.root / "tools"
        tools.mkdir()
        (tools / "dpkg-query").write_text(
            "#!/bin/sh\ncase \"$1\" in\n"
            "-S) printf 'fixture-package: %s\\n' \"$2\" ;;\n"
            "-W) printf 'fixture-package\\t%s\\n' \"${FIXTURE_PACKAGE_VERSION:-1.0}\" ;;\nesac\n"
        )
        (tools / "dpkg-query").chmod(0o755)
        (prefix / ".release-wpe-digest").write_text(DIGEST + "\n")

    def package(self, *args, **environment: str) -> subprocess.CompletedProcess:
        env = {key: value for key, value in os.environ.items() if not key.startswith("NGTEAMS_")}
        env.update(NGTEAMS_RELEASE_WPE=str(self.root / "wpe"), PATH=f"{self.root / 'tools'}:{env['PATH']}")
        env.update(environment)
        return run("bash", self.root / "scripts/package-web-engine.sh", "--system-root", self.root / "system",
                   "--multiarch", "fixture", *args, env=env)

    def test_archive_has_only_selected_plugins_and_one_dependency_copy(self) -> None:
        # --print-key resolves inputs without copying or patching, so it must not call patchelf.
        patchelf = self.root / "tools/patchelf"
        patchelf.write_text("#!/bin/sh\nexit 1\n")
        patchelf.chmod(0o755)
        before = self.package("--print-key")
        self.assertEqual(before.returncode, 0, before.stderr)
        key = before.stdout.strip()
        self.assertEqual(len(key), 64)
        patchelf.unlink()
        result = self.package(self.root / "dist")
        self.assertEqual(result.returncode, 0, result.stderr)
        archive = next((self.root / "dist").glob("*.tar.zst"))
        self.assertEqual(archive.name, f"ngteams-web-engine-{key}-linux-x86_64.tar.zst")
        self.assertIn(f"size={archive.stat().st_size}", result.stdout)
        self.assertIn(f"sha256={hashlib.sha256(archive.read_bytes()).hexdigest()}", result.stdout)
        tar = self.root / "runtime.tar"
        with tar.open("wb") as output:
            subprocess.run(["zstd", "-dc", str(archive)], stdout=output, check=True)
        extracted = self.root / "extracted"
        with tarfile.open(tar) as contents:
            contents.extractall(extracted, filter="data")
        plugins = extracted / "lib/gstreamer-1.0"
        self.assertEqual({path.name for path in plugins.iterdir()}, {"libgstplayback.so", "libgstlibav.so"})
        dependency = extracted / "lib/libengine-dependency.so.1"
        self.assertTrue(dependency.is_symlink() and dependency.is_file())
        self.assertEqual(len([path for path in extracted.rglob("libengine-dependency*") if not path.is_symlink()]), 1)
        self.assertTrue((extracted / "lib/libWPEWebKit-2.0.so.1").is_file())
        self.assertEqual(len([path for path in extracted.rglob("libWPEWebKit*") if not path.is_symlink()]), 1)
        self.assertTrue((extracted / "lib/wpe-webkit-2.0/injected-bundle/libWPEInjectedBundle.so").is_file())
        self.assertTrue((extracted / "libexec/gstreamer-1.0/gst-plugin-scanner").is_file())
        self.assertEqual(len(list((extracted / "lib/gio/modules").glob("*.so"))), 4)
        self.assertEqual((extracted / "share/doc/fixture-package/copyright").read_text(), "Fixture copyright\n")
        self.assertTrue((extracted / "share/doc/wpewebkit/LICENSE").is_file())
        self.assertTrue((extracted / "share/common-licenses/LGPL-2.1").is_file())
        self.assertEqual((extracted / "PACKAGES").read_text(), "fixture-package\t1.0\n")
        self.assertIn("libc.so.6", (extracted / "HOST-LIBRARIES").read_text())
        manifest = (extracted / "MANIFEST").read_text()
        self.assertEqual(manifest, f"key={key}\narch=x86_64\nwpe_version=2.54.1\n")
        # The original plugin search path can disappear: the bundled closure is used.
        shutil.rmtree(self.root / "system/usr/lib")
        env = {name: value for name, value in os.environ.items() if name != "LD_LIBRARY_PATH"}
        found = run("ldd", plugins / "libgstplayback.so", env=env)
        self.assertEqual(found.returncode, 0, found.stdout)
        resolved = re.search(r"libengine-dependency.so.1 => (\S+)", found.stdout)[1]
        self.assertEqual(Path(resolved).resolve(), dependency.resolve())
        self.assertEqual(run("patchelf", "--print-rpath", plugins / "libgstplayback.so").stdout.strip(), "$ORIGIN:$ORIGIN/..")

    def test_key_tracks_runtime_inputs(self) -> None:
        for changed in ("allowlist", "hostlibs", "wrapper", "closure", "digest", "package"):
            with self.subTest(changed=changed):
                self.setUp()
                old = self.package("--print-key")
                self.assertEqual(old.returncode, 0, old.stderr)
                env = {}
                if changed in ("allowlist", "hostlibs", "wrapper", "closure"):
                    name = {"allowlist": "packaging/linux/gstreamer-plugins.txt",
                            "hostlibs": "packaging/linux/runtime-libraries.txt",
                            "wrapper": "scripts/package-web-engine.sh", "closure": "scripts/web-engine-closure.py"}[changed]
                    path = self.root / name
                    path.write_text(path.read_text() + ("offlist\n" if changed == "allowlist" else "\n# changed runtime recipe\n"))
                elif changed == "digest":
                    newer = "sha256:" + "cd" * 32
                    pins = self.root / "packaging/release/pins.env"
                    pins.write_text(pins.read_text().replace(DIGEST, newer))
                    (self.root / "wpe/.release-wpe-digest").write_text(newer + "\n")
                else:
                    env = {"FIXTURE_PACKAGE_VERSION": "1.1-security-update"}
                new = self.package("--print-key", **env)
                self.assertEqual(new.returncode, 0, new.stderr)
                self.assertNotEqual(old.stdout, new.stdout)

    def test_missing_allowlisted_plugin_is_an_error(self) -> None:
        (self.root / "system/usr/lib/fixture/gstreamer-1.0/libgstplayback.so").unlink()
        result = self.package("--print-key")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing runtime input", result.stderr)
        self.assertIn("libgstplayback.so", result.stderr)

    def test_stale_wpe_prefix_is_an_error(self) -> None:
        (self.root / "wpe/.release-wpe-digest").write_text("sha256:" + "00" * 32 + "\n")
        result = self.package("--print-key")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("release WPE prefix is stale", result.stderr)

    def test_an_unstamped_wpe_prefix_is_an_error(self) -> None:
        (self.root / "wpe/.release-wpe-digest").unlink()
        result = self.package("--print-key")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("release WPE prefix is stale", result.stderr)

    def test_a_wpe_prefix_without_a_version_is_an_error(self) -> None:
        (self.root / "wpe/lib/pkgconfig/wpe-webkit-2.0.pc").write_text("Name: wpe-webkit-2.0\n")
        result = self.package(self.root / "dist")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no Version", result.stderr)

    def test_shared_host_check_rejects_an_undeclared_dependency(self) -> None:
        tree = self.root / "bundle"
        tree.mkdir()
        plugin = self.root / "system/usr/lib/fixture/gstreamer-1.0/libgstplayback.so"
        shutil.copy2(plugin, tree / "plugin.so")
        allowed = self.root / "packaging/linux/runtime-libraries.txt"
        # A bare host-path RUNPATH is overwritten, so this library becomes missing.
        result = run(sys.executable, self.root / "scripts/web-engine-closure.py", "check", tree, tree / "lib", allowed)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unresolved dependencies", result.stderr)
        # A host path supplied through the loader cache (modeled by ldd output) is resolved but still fails the
        # declared-host-library check.
        ldd = self.root / "tools/ldd"
        ldd.write_text(f"#!/bin/sh\nprintf '\\tlibengine-dependency.so.1 => {plugin.parent.parent}/libengine-dependency.so.1 (0x123)\\n'\n")
        ldd.chmod(0o755)
        env = dict(os.environ, PATH=f"{ldd.parent}:{os.environ['PATH']}")
        result = run(sys.executable, self.root / "scripts/web-engine-closure.py", "check", tree, tree / "lib", allowed, env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("undeclared host library: libengine-dependency.so.1", result.stderr)

    def test_allowlist_contains_required_media_plugins(self) -> None:
        plugins = {line for line in (REPO / "packaging/linux/gstreamer-plugins.txt").read_text().splitlines()
                   if line and not line.startswith("#")}
        self.assertEqual(len(plugins), 71)
        self.assertLessEqual({"opengl", "encoding", "gio", "alsa", "libav", "va", "nvcodec", "v4l2codecs"}, plugins)
        self.assertFalse({"webrtc", "vulkan", "v4l2", "hls", "dash", "adaptivedemux2"} & plugins)


if __name__ == "__main__":
    unittest.main()
