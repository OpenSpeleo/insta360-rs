"""Check the SDK cache identity independently of wheel and Rust dependencies."""

import importlib.util
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "use_ffmpeg", Path(__file__).with_name("use-ffmpeg.py")
)
ffmpeg = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ffmpeg)


class PreparedFFmpegTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="prepared ffmpeg ")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name).resolve()

    def sdk(self, native):
        cache = self.root / ".cache" / ("wheel-native" if native else "wheel")
        prefix = cache / "native/test/prefix"
        (prefix / "include/libavcodec").mkdir(parents=True)
        (prefix / "include/libavcodec/avcodec.h").write_text("original header\n")
        (prefix / "lib/pkgconfig").mkdir(parents=True)
        (prefix / "lib/pkgconfig/libavcodec.pc").write_text(
            "prefix=/previous/checkout\nlibdir=/previous/checkout/lib\n"
        )
        (cache / "sdk-prefix.txt").write_text("native/test/prefix\n")
        return prefix

    def test_linux_sdk_preserves_existing_runtime_paths(self):
        prefix = self.sdk(native=False)
        with (
            patch.object(ffmpeg.sys, "platform", "linux"),
            patch.dict(os.environ, {"LD_LIBRARY_PATH": "/existing/lib"}, clear=True),
        ):
            env = ffmpeg.configure(self.root)
        self.assertEqual(env, {
            "PKG_CONFIG_PATH": str(prefix / "lib/pkgconfig"),
            "LD_LIBRARY_PATH": str(prefix / "lib") + os.pathsep + "/existing/lib",
        })
        self.assertEqual(
            (prefix / "lib/pkgconfig/libavcodec.pc").read_text(),
            f"prefix={prefix}\nlibdir={prefix}/lib\n",
        )

    def test_windows_sdk_exports_compiler_and_dll_search_paths(self):
        prefix = self.sdk(native=True)
        with (
            patch.object(ffmpeg.sys, "platform", "win32"),
            patch.dict(os.environ, {"PATH": "compiler-tools"}, clear=True),
            patch.object(ffmpeg.subprocess, "check_output") as output,
        ):
            env = ffmpeg.configure(self.root)
        output.assert_not_called()
        self.assertEqual(env["FFMPEG_DIR"], str(prefix))
        self.assertEqual(env["PATH"], str(prefix / "bin") + os.pathsep + "compiler-tools")
        self.assertNotIn("LD_LIBRARY_PATH", env)

    def test_macos_uses_fallback_loading_and_selected_sdk(self):
        prefix = self.sdk(native=True)
        inherited = {
            "DYLD_FALLBACK_LIBRARY_PATH": "/other/lib",
            "SDKROOT": "/selected/sdk",
            "LIBCLANG_PATH": "/selected/clang/lib",
            "MACOSX_DEPLOYMENT_TARGET": "12.0",
            "BINDGEN_EXTRA_CLANG_ARGS": "--sysroot=/selected/sdk -DSELECTED=1",
        }
        with (
            patch.object(ffmpeg.sys, "platform", "darwin"),
            patch.dict(os.environ, inherited, clear=True),
            patch.object(ffmpeg.subprocess, "check_output") as output,
        ):
            env = ffmpeg.configure(self.root)
        output.assert_not_called()
        self.assertEqual(env["FFMPEG_DIR"], str(prefix))
        self.assertNotIn("DYLD_LIBRARY_PATH", env)
        self.assertEqual(
            env["DYLD_FALLBACK_LIBRARY_PATH"], str(prefix / "lib") + os.pathsep + "/other/lib",
        )
        for key in inherited.keys() - {"DYLD_FALLBACK_LIBRARY_PATH"}:
            self.assertEqual(env[key], inherited[key])

    def test_macos_discovers_missing_sdk_configuration(self):
        self.sdk(native=True)
        with (
            patch.object(ffmpeg.sys, "platform", "darwin"),
            patch.dict(os.environ, {}, clear=True),
            patch.object(ffmpeg.subprocess, "check_output", side_effect=[
                "/selected/sdk\n", "/selected/toolchain/bin/clang\n",
            ]),
        ):
            env = ffmpeg.configure(self.root)
        self.assertEqual(env["SDKROOT"], "/selected/sdk")
        self.assertEqual(env["LIBCLANG_PATH"], str(Path("/selected/toolchain/lib")))
        self.assertEqual(env["BINDGEN_EXTRA_CLANG_ARGS"], "--sysroot=/selected/sdk")
        self.assertEqual(env["MACOSX_DEPLOYMENT_TARGET"], "11.0")

    def test_prefix_cannot_escape_the_extracted_cache(self):
        prefix = self.sdk(native=True)
        cache = prefix.parents[2]
        (cache / "sdk-prefix.txt").write_text("../../../outside\n")
        with patch.object(ffmpeg.sys, "platform", "win32"):
            with self.assertRaisesRegex(RuntimeError, "invalid prefix"):
                ffmpeg.configure(self.root)


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "requires POSIX bash")
class FFmpegCacheTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="ffmpeg cache ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.scripts = self.root / "scripts/ci"
        self.scripts.mkdir(parents=True)
        for name in ("build-wheel.sh", "build-ffmpeg.sh", "ffmpeg-runtime-config.sh"):
            shutil.copyfile(Path(__file__).with_name(name), self.scripts / name)

    def run_script(self, argument, root=None):
        return subprocess.run(
            ["bash", str((root or self.root) / "scripts/ci/build-wheel.sh"), argument],
            text=True, capture_output=True, timeout=10,
        )

    def key(self, root=None):
        result = self.run_script("--cache-key", root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stdout, r"^[a-f0-9]{64}\n$")
        return result.stdout.strip()

    def test_metadata_does_not_start_a_build(self):
        self.key()
        image = self.run_script("--image")
        self.assertEqual(image.returncode, 0, image.stderr)
        self.assertRegex(image.stdout, r"^quay.io/pypa/manylinux_2_28_x86_64@sha256:[a-f0-9]{64}\n$")
        for name in ("dist", ".cache", "target"):
            self.assertFalse((self.root / name).exists())

    def test_native_inputs_invalidate_the_sdk(self):
        original_key = self.key()
        for name in ("build-ffmpeg.sh", "ffmpeg-runtime-config.sh"):
            with self.subTest(name=name):
                path = self.scripts / name
                original = path.read_text()
                path.write_text(original + "\n# changed native recipe\n")
                self.assertNotEqual(self.key(), original_key)
                path.write_text(original)
        wrapper = self.scripts / "build-wheel.sh"
        wrapper.write_text(re.sub(r"@sha256:[a-f0-9]{64}", "@sha256:" + "0" * 64, wrapper.read_text()))
        self.assertNotEqual(self.key(), original_key)

    def test_wheel_and_rust_changes_reuse_the_sdk(self):
        original_key = self.key()
        for relative in ("Cargo.lock", "rust-toolchain.toml", "scripts/ci/requirements-wheel.txt"):
            (self.root / relative).write_text("changed dependency input\n")
        wrapper = self.scripts / "build-wheel.sh"
        wrapper.write_text(wrapper.read_text() + "\n# changed wheel packaging\n")
        self.assertEqual(self.key(), original_key)

    def test_checkout_path_does_not_change_the_key(self):
        relocated = self.root / "another checkout"
        shutil.copytree(self.scripts, relocated / "scripts/ci")
        self.assertEqual(self.key(relocated), self.key())

    def test_missing_native_input_fails(self):
        (self.scripts / "ffmpeg-runtime-config.sh").unlink()
        self.assertNotEqual(self.run_script("--cache-key").returncode, 0)


if __name__ == "__main__":
    unittest.main()
