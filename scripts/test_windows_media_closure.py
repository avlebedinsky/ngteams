"""Real x64 PE fixtures exercise normal/delay imports without a Windows VM or SDK."""

from __future__ import annotations

import importlib.util
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("media", REPO / "scripts/windows-media-closure.py")
media = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(media)


@unittest.skipUnless(shutil.which("clang") and shutil.which("rustc"), "PE fixtures need clang and Rust's LLD")
class WindowsMediaClosure(unittest.TestCase):
    def setUp(self):
        work = tempfile.TemporaryDirectory()
        self.addCleanup(work.cleanup)
        self.root = Path(work.name)
        sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True, cwd=REPO).strip())
        self.linker = next((sysroot / "lib/rustlib").glob("*/bin/rust-lld*"))
        self.reader = media.readobj_path()
        self.dll("Leaf", "__declspec(dllexport) int leaf(void) { return 7; }")
        self.dll("Middle", "__declspec(dllimport) int leaf(void); __declspec(dllexport) int middle(void) { return leaf(); }",
                 "Leaf.lib")
        self.dll("Top", "__declspec(dllimport) int middle(void); __declspec(dllexport) int top(void) { return middle(); }"
                 "void *__delayLoadHelper2(void *a, void *b) { return 0; }", "Middle.lib", "/delayload:Middle.dll")

    def dll(self, name, source, *flags):
        path = self.root / f"{name}.c"
        path.write_text(source)
        subprocess.run(["clang", "--target=x86_64-pc-windows-msvc", "-c", str(path), "-o", f"{name}.obj"],
                       cwd=self.root, check=True, capture_output=True)
        subprocess.run([str(self.linker), "-flavor", "link", "/dll", "/noentry", "/nodefaultlib",
                        f"/out:{name}.dll", f"{name}.obj", *flags], cwd=self.root, check=True, capture_output=True)

    def test_delay_and_transitive_imports_are_copied_case_insensitively(self):
        self.assertEqual(media.imports(self.root / "Top.dll", self.reader), ["middle.dll"])
        files, system = media.closure({self.root / "Top.dll": Path("lib/gstreamer-1.0/gsttop.dll")},
                                      media.index(self.root), self.reader)
        self.assertEqual({p.name for p in files}, {"Top.dll", "Middle.dll", "Leaf.dll"})
        self.assertEqual(files[self.root / "Leaf.dll"], Path("bin/Leaf.dll"))
        self.assertFalse(system)

    def test_missing_transitive_import_fails(self):
        (self.root / "Leaf.dll").unlink()
        with self.assertRaisesRegex(ValueError, "unresolved dependency leaf.dll"):
            media.closure({self.root / "Top.dll": Path("bin/Top.dll")}, media.index(self.root), self.reader)

    def test_duplicate_windows_dll_names_fail(self):
        if (self.root / "leaf.dll").exists():
            self.skipTest("the fixture filesystem is already case-insensitive")
        shutil.copy2(self.root / "Leaf.dll", self.root / "leaf.dll")
        with self.assertRaisesRegex(ValueError, "ambiguous DLL"):
            media.index(self.root)

    def test_only_declared_os_components_are_external(self):
        self.assertTrue(media.is_system("kernel32.dll"))
        self.assertTrue(media.is_system("api-ms-win-crt-runtime-l1-1-0.dll"))
        for name in ["vcruntime140.dll", "msvcp140.dll", "codec.dll", "api-made-up.dll"]:
            self.assertFalse(media.is_system(name))

    def test_linux_runtime_roots_have_explicit_windows_equivalents(self):
        linux = set(media.names(REPO / "packaging/linux/gstreamer-plugins.txt"))
        windows = set(media.plugin_names())
        equivalents = {
            "aom": {"dav1d", "svtav1"}, "de265": {"libav"}, "faad": {"libav"},
            "alsa": {"wasapi2"}, "pulseaudio": {"wasapi2"},
            # WPE-specific sinks/network input; the native client paints appsink frames in GPUI and feeds appsrc
            # from its engine. Hardware video decoding is unranked by the shared player on both platforms.
            "opengl": {"app"}, "gio": {"app"}, "nvcodec": set(), "v4l2codecs": set(), "va": set(),
            # WebKit's MIDI renderer: native inline cards accept FileKind::Video, not MIDI/music attachments.
            "wildmidi": set(),
        }
        self.assertLessEqual(linux - equivalents.keys(), windows)
        for plugin, replacements in equivalents.items():
            self.assertIn(plugin, linux)
            self.assertLessEqual(replacements, windows)

    def test_verify_detects_missing_corrupt_and_extra_runtime_files(self):
        root = self.root / "runtime"
        (root / "bin").mkdir(parents=True)
        shutil.copy2(self.root / "Leaf.dll", root / "bin/Leaf.dll")
        manifest = {"plugins": media.plugin_names(), "system_imports": [],
                    "files": {"bin/Leaf.dll": media.digest(root / "bin/Leaf.dll")}}
        (root / "MEDIA-MANIFEST.json").write_text(json.dumps(manifest))
        media.check(root, self.reader, [])
        (root / "surprise.dll").write_bytes(b"not in the runtime inventory")
        with self.assertRaisesRegex(ValueError, "inventory differs"):
            media.check(root, self.reader, [])
        (root / "surprise.dll").unlink()
        (root / "bin/Leaf.dll").write_bytes(b"corrupt")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            media.check(root, self.reader, [])
        (root / "bin/Leaf.dll").unlink()
        with self.assertRaisesRegex(ValueError, "inventory differs"):
            media.check(root, self.reader, [])

    def test_non_x64_input_fails(self):
        source = self.root / "Leaf.c"
        subprocess.run(["clang", "--target=i686-pc-windows-msvc", "-c", str(source), "-o", "x86.obj"],
                       cwd=self.root, check=True, capture_output=True)
        subprocess.run([str(self.linker), "-flavor", "link", "/dll", "/noentry", "/nodefaultlib",
                        "/out:x86.dll", "x86.obj"], cwd=self.root, check=True, capture_output=True)
        with self.assertRaisesRegex(ValueError, "not a Windows x64 PE"):
            media.imports(self.root / "x86.dll", self.reader)

    def sdk(self):
        sdk = self.root / "sdk"
        paths = [Path(f"lib/gstreamer-1.0/gst{name}.dll") for name in media.plugin_names()]
        paths += [Path(f"bin/{name}.dll") for name in
                  ("gstreamer-1.0-0", "gstapp-1.0-0", "gstaudio-1.0-0", "gstvideo-1.0-0")]
        paths += [Path("libexec/gstreamer-1.0/gst-plugin-scanner.exe"),
                  Path("bin/gst-inspect-1.0.exe"), Path("bin/gst-launch-1.0.exe")]
        for path in paths:
            (sdk / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(self.root / "Top.dll", sdk / path)
        for name in ("Leaf.dll", "Middle.dll"):
            shutil.copy2(self.root / name, sdk / "bin" / name)
        shutil.copy2(self.root / "Leaf.dll", sdk / "bin/unused.dll")
        notice = sdk / "share/licenses/gstreamer-1.0/LGPL-2.0-or-later.txt"
        notice.parent.mkdir(parents=True)
        notice.write_text("Synthetic SDK license text\n")
        (sdk / "share/versions.txt").write_text("cerbero 1.28.8\nfixture 1.0\n")
        license_file = self.root / "vc-license.txt"
        license_file.write_text("Synthetic runtime license\n")
        crt = self.root / "crt"
        crt.mkdir()
        return argparse.Namespace(sdk=sdk, crt=crt, crt_license=license_file,
                                  binary=[], output=self.root / "packaged", plan=False)

    def test_package_includes_all_roots_dependencies_licenses_and_versions(self):
        args = self.sdk()
        media.package(args, self.reader)
        root = args.output
        self.assertEqual((root / "share/licenses/microsoft-vc-runtime/vc-license.txt").read_text(),
                         "Synthetic runtime license\n")
        self.assertTrue((root / "share/licenses/gstreamer-1.0/LGPL-2.0-or-later.txt").is_file())
        self.assertTrue((root / "share/versions.txt").is_file())
        self.assertTrue((root / "bin/Leaf.dll").is_file())
        self.assertTrue((root / "bin/Middle.dll").is_file())
        self.assertFalse((root / "bin/unused.dll").exists())
        manifest = json.loads((root / "MEDIA-MANIFEST.json").read_text())
        self.assertEqual(manifest["plugins"], media.plugin_names())
        # A changed policy must make reuse fail instead of silently omitting the new plugin.
        manifest["plugins"].pop()
        (root / "MEDIA-MANIFEST.json").write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "plugin inventory changed"):
            media.check(root, self.reader, [])

    def test_missing_plugin_or_license_rejects_packaging_before_copy(self):
        args = self.sdk()
        license_file = args.sdk / "share/licenses/gstreamer-1.0/LGPL-2.0-or-later.txt"
        license_file.unlink()
        with self.assertRaisesRegex(ValueError, "missing SDK license"):
            media.package(args, self.reader)
        self.assertFalse(args.output.exists())
        next((args.sdk / "lib/gstreamer-1.0").glob("*.dll")).unlink()
        with self.assertRaisesRegex(ValueError, "missing runtime root"):
            media.package(args, self.reader)
        self.assertFalse(args.output.exists())


if __name__ == "__main__":
    unittest.main()
