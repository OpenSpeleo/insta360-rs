"""Verify native dependency target propagation and release notice checks."""

import importlib.util
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "build_native_wheel", Path(__file__).with_name("build-native-wheel.py")
)
native = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(native)


class NativeWheelTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="native wheel test ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_mnn_inherits_prepared_wheel_target_and_compiler_environment(self):
        env = {
            "MACOSX_DEPLOYMENT_TARGET": "11.0",
            "SDKROOT": "/selected-sdk",
            "CC": "selected-clang",
            "CXXFLAGS": "-fvisibility=hidden",
            "PATH": "/selected-build-tools",
        }
        with patch.object(native, "run") as run:
            prefix = native.prepare_mnn(self.root, env)
        run.assert_called_once_with(
            native.sys.executable,
            native.ROOT / "scripts/ci/build-mnn.py",
            "--output",
            prefix,
            env=env,
        )
        self.assertEqual(env["MNN_ROOT"], str(prefix))
        self.assertEqual(env["MACOSX_DEPLOYMENT_TARGET"], "11.0")
        self.assertEqual(env["SDKROOT"], "/selected-sdk")

    def test_native_wheel_requires_both_independent_mnn_notices(self):
        target = "macosx_11_0_arm64"
        wheel = self.root / f"insta360_rs-0.1.0-cp310-abi3-{target}.whl"
        notices = (
            "FFMPEG-LICENSE.txt", "X265-LICENSE.txt", "SOURCES.txt",
            "MNN-LICENSE.txt", "MNN-THIRD-PARTY-NOTICES.txt",
        )
        for missing in (None, *notices[-2:]):
            with self.subTest(missing=missing):
                with zipfile.ZipFile(wheel, "w") as archive:
                    for library in ("avcodec", "avformat", "avutil", "swscale", "x265"):
                        archive.writestr(f"insta360_rs/.libs/lib{library}.dylib", b"library")
                    for notice in notices:
                        if notice != missing:
                            archive.writestr(f"insta360_rs.dist-info/licenses/{notice}", b"notice")
                    for source in ("ffmpeg-8.1.2.tar.xz", "x265_4.1.tar.gz"):
                        archive.writestr(f"insta360_rs/_licenses/sources/{source}", b"source")
                    for name in ("py.typed", "__init__.pyi"):
                        archive.writestr(f"insta360_rs/{name}", b"")
                with patch.object(native.sys, "platform", "darwin"):
                    if missing:
                        with self.assertRaisesRegex(RuntimeError, missing):
                            native.verify_archive(wheel, target)
                    else:
                        native.verify_archive(wheel, target)


if __name__ == "__main__":
    unittest.main()
