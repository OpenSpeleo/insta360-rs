"""Check the SDK cache identity independently of wheel and Rust dependencies."""

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


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
