"""Verify native dependency target propagation and release notice checks."""

import importlib.util
import io
import shutil
import tarfile
import tempfile
import types
import unittest
import zipfile
from contextlib import redirect_stdout
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

    def test_explicit_mnn_prefix_is_verified_and_reused(self):
        prefix = self.root / "prepared mnn"
        env = {"MNN_ROOT": str(prefix)}
        with patch.object(native, "run") as run:
            self.assertEqual(native.prepare_mnn(self.root, env), prefix.resolve())
        run.assert_called_once_with(
            native.sys.executable, native.ROOT / "scripts/ci/build-mnn.py",
            "--output", prefix.resolve(), "--verify-only", env=env,
        )

    def test_invalid_explicit_mnn_prefix_does_not_trigger_a_rebuild(self):
        with patch.object(native, "run", side_effect=RuntimeError("invalid SDK")) as run:
            with self.assertRaisesRegex(RuntimeError, "invalid SDK"):
                native.prepare_mnn(self.root, {"MNN_ROOT": str(self.root / "bad")})
        self.assertEqual(run.call_count, 1)

    def prepare_native_sources(self):
        scripts = self.root / "scripts/ci"
        scripts.mkdir(parents=True)
        for name in ("build-ffmpeg-native.sh", "ffmpeg-runtime-config.sh"):
            shutil.copyfile(native.ROOT / "scripts/ci" / name, scripts / name)
        shutil.copyfile(native.ROOT / "rust-toolchain.toml", self.root / "rust-toolchain.toml")

    def test_native_cache_key_tracks_native_inputs_and_target_only(self):
        self.prepare_native_sources()
        env = {"MACOSX_DEPLOYMENT_TARGET": "11.0"}
        with (
            patch.object(native, "ROOT", self.root),
            patch.object(native.platform, "platform", return_value="macOS-arm64") as platform,
            patch.object(native, "output", return_value="cmake 3.31") as output,
        ):
            original = native.native_cache_key(env, Path("cmake"))
            self.assertRegex(original, r"^[a-f0-9]{64}$")
            for name in ("build-ffmpeg-native.sh", "ffmpeg-runtime-config.sh"):
                with self.subTest(name=name):
                    path = self.root / "scripts/ci" / name
                    contents = path.read_bytes()
                    path.write_bytes(contents + b"\n# native change\n")
                    self.assertNotEqual(native.native_cache_key(env, Path("cmake")), original)
                    path.write_bytes(contents)
            for name in ("Cargo.lock", "rust-toolchain.toml", "scripts/ci/build-native-wheel.py"):
                (self.root / name).write_text("packaging change\n")
            self.assertEqual(native.native_cache_key(env, Path("cmake")), original)
            relocated = self.root / "another checkout"
            shutil.copytree(self.root / "scripts", relocated / "scripts")
            with patch.object(native, "ROOT", relocated):
                self.assertEqual(native.native_cache_key(env, Path("cmake")), original)
            self.assertNotEqual(
                native.native_cache_key({"MACOSX_DEPLOYMENT_TARGET": "12.0"}, Path("cmake")),
                original,
            )
            platform.return_value = "macOS-x86_64"
            self.assertNotEqual(native.native_cache_key(env, Path("cmake")), original)
            platform.return_value = "macOS-arm64"
            output.return_value = "cmake 3.32"
            self.assertNotEqual(native.native_cache_key(env, Path("cmake")), original)

    def test_cache_key_and_prewarm_never_build_mnn_or_wheels(self):
        self.prepare_native_sources()
        cache = self.root / ".cache/wheel-native"

        def build_sdk(*_command, env):
            prefix = Path(env["INSTA360_FFMPEG_PREFIX"])
            (prefix / "share/insta360-rs").mkdir(parents=True)
            (prefix / "share/insta360-rs/complete").touch()
            (prefix / "lib").mkdir()
            (prefix / "lib/libavcodec.dylib").write_bytes(b"compiled runtime")
            (prefix.parent / "build").mkdir()
            (prefix.parent / "build/private-object.o").write_bytes(b"not for consumers")
            (cache / "downloads").mkdir()
            (cache / "downloads/source.tar.gz").write_bytes(b"original source")

        for option in ("--cache-key", "--prewarm"):
            with (
                self.subTest(option=option),
                patch.object(native, "ROOT", self.root),
                patch.object(native.sys, "platform", "darwin"),
                patch.object(native.platform, "machine", return_value="arm64"),
                patch.object(native.platform, "platform", return_value="macOS-arm64"),
                patch.object(native, "output", return_value="/selected/tool"),
                patch.dict(native.sys.modules, {"cmake": types.SimpleNamespace(CMAKE_BIN_DIR="/cmake")}),
                patch.object(native.sys, "argv", ["build-native-wheel.py", option, "--out", str(self.root / "sdk")]),
                patch.object(native, "run", side_effect=build_sdk) as run,
                patch.object(native, "prepare_mnn") as mnn,
                redirect_stdout(io.StringIO()) as stdout,
            ):
                native.main()
                mnn.assert_not_called()
                if option == "--cache-key":
                    run.assert_not_called()
                    self.assertRegex(stdout.getvalue(), r"^[a-f0-9]{64}\n$")
                    self.assertFalse(cache.exists())
                else:
                    self.assertEqual(run.call_count, 1)
                    self.assertEqual(run.call_args.args[0], "bash")
        with tarfile.open(self.root / "sdk/ffmpeg-sdk.tar.gz") as archive:
            names = archive.getnames()
            self.assertIn(".cache/wheel-native/sdk-prefix.txt", names)
            self.assertIn(".cache/wheel-native/downloads/source.tar.gz", names)
            self.assertTrue(any(name.endswith("lib/libavcodec.dylib") for name in names))
            self.assertFalse(any("/build/" in name for name in names))
            self.assertEqual(
                archive.extractfile(".cache/wheel-native/downloads/source.tar.gz").read(),
                b"original source",
            )

    def test_incomplete_sdk_is_not_published(self):
        with self.assertRaisesRegex(RuntimeError, "incomplete"):
            native.archive_sdk(self.root, self.root / "prefix", self.root / "out")
        self.assertFalse((self.root / "out").exists())

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
