"""Regression coverage for native setup in direct Git/prek Clippy runs."""

import importlib.util
import io
import os
import shutil
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "run_clippy", Path(__file__).with_name("run-clippy.py")
)
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class ClippyRunnerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="clippy hook test ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for context in (
            patch.object(runner, "ROOT", self.root),
            patch.dict(os.environ, {"PATH": "/caller/bin"}, clear=True),
            patch.object(runner, "prepare_ffmpeg"),
        ):
            context.start()
            self.addCleanup(context.stop)

    def test_macos_selects_sdk_before_cargo_and_runs_full_clippy_scope(self):
        with (
            patch.object(runner.sys, "platform", "darwin"),
            patch.object(
                runner.subprocess, "check_output",
                return_value="/selected Xcode/SDKs/MacOSX.sdk\n",
            ) as output,
            patch.object(
                runner.subprocess, "run",
                return_value=subprocess.CompletedProcess([], 101),
            ) as run,
        ):
            self.assertEqual(runner.main(), 101)
        self.assertEqual(output.call_args.args[0], ["xcrun", "--sdk", "macosx", "--show-sdk-path"])
        self.assertEqual(run.call_args.kwargs["env"]["SDKROOT"], "/selected Xcode/SDKs/MacOSX.sdk")
        self.assertEqual(run.call_args.kwargs["env"]["PATH"], "/caller/bin")
        self.assertEqual(run.call_args.kwargs["cwd"], self.root)
        self.assertFalse(run.call_args.kwargs["check"])
        self.assertEqual(run.call_args.args[0], [
            "cargo", "clippy", "--workspace", "--locked", "--all-targets",
            "--all-features", "--", "-D", "warnings",
        ])
        self.assertNotIn("SDKROOT", os.environ)

    def test_prepared_environment_is_preserved_on_every_platform(self):
        prepared = {
            "PATH": "/caller/bin",
            "SDKROOT": "/explicit SDK",
            "DEVELOPER_DIR": "/selected Xcode",
            "CC": "custom-clang",
            "CXX": "custom-clang++",
            "LIBCLANG_PATH": "/custom libclang",
            "PKG_CONFIG_PATH": "/ffmpeg/pkgconfig",
            "CARGO_TARGET_DIR": "/existing target",
            "RUSTFLAGS": "-C debuginfo=1",
            "CFLAGS": "-O2",
            "MACOSX_DEPLOYMENT_TARGET": "11.0",
        }
        for platform in ("darwin", "linux", "win32"):
            with (
                self.subTest(platform=platform),
                patch.dict(os.environ, prepared, clear=True),
                patch.object(runner.sys, "platform", platform),
                patch.object(runner.subprocess, "check_output") as output,
                patch.object(runner.subprocess, "run") as run,
            ):
                self.assertEqual(runner.prepare_environment(), prepared)
                output.assert_not_called()
                run.assert_not_called()

    def test_discovery_failure_prevents_cargo(self):
        with (
            patch.object(runner.sys, "platform", "darwin"),
            patch.object(
                runner.subprocess, "check_output",
                side_effect=subprocess.CalledProcessError(7, "native discovery"),
            ),
            patch.object(runner.subprocess, "run") as run,
            redirect_stderr(io.StringIO()),
        ):
            self.assertEqual(runner.main(), 7)
            run.assert_not_called()

    def test_ffmpeg_configuration_reaches_cargo(self):
        def configure(env):
            env.update(FFMPEG_DIR="/prepared SDK", MACOSX_DEPLOYMENT_TARGET="11.0")

        with (
            patch.object(runner.sys, "platform", "linux"),
            patch.object(runner, "prepare_ffmpeg", side_effect=configure),
        ):
            env = runner.prepare_environment()
        self.assertEqual(env["FFMPEG_DIR"], "/prepared SDK")
        self.assertEqual(env["MACOSX_DEPLOYMENT_TARGET"], "11.0")

    def test_missing_tool_or_empty_sdk_prevents_build_and_cargo(self):
        for response in (FileNotFoundError("xcrun"), "\n"):
            with (
                self.subTest(response=response),
                patch.object(runner.sys, "platform", "darwin"),
                patch.object(runner.subprocess, "check_output", side_effect=[response]),
                patch.object(runner.subprocess, "run") as run,
                redirect_stderr(io.StringIO()),
            ):
                self.assertEqual(runner.main(), 1)
                run.assert_not_called()

    def test_direct_hook_uses_the_native_runner(self):
        config = Path(__file__).resolve().parents[2] / ".pre-commit-config.yaml"
        hook = (
            config.read_text().split("- id: cargo-clippy\n", 1)[1].split("- id:", 1)[0]
        )
        entry = next(line.strip() for line in hook.splitlines() if "entry:" in line)
        self.assertEqual(entry, "entry: python3 scripts/ci/run-clippy.py")
        self.assertIn("pass_filenames: false", hook)
        self.assertIn("language: python", hook)
        self.assertIn("cmake==3.31.10; sys_platform != 'linux'", hook)


class FFmpegRunnerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="clippy ffmpeg test ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        scripts = self.root / "scripts/ci"
        scripts.mkdir(parents=True)
        shutil.copyfile(
            Path(__file__).with_name("use-ffmpeg.py"), scripts / "use-ffmpeg.py"
        )
        self.env = {
            "PATH": "/caller/bin",
            "SDKROOT": "/selected SDK",
            "LIBCLANG_PATH": "/selected clang",
            "RUSTFLAGS": "-C debuginfo=1",
            "CARGO_TARGET_DIR": "/caller target",
        }
        self.key = "a" * 64
        context = patch.object(runner, "ROOT", self.root)
        context.start()
        self.addCleanup(context.stop)

    def create_sdk(self, native, key=None):
        cache = self.root / ".cache" / ("wheel-native" if native else "wheel")
        relative = f"native/{key or self.key}/prefix"
        prefix = cache / relative
        for directory in ("share/insta360-rs", "include/libavcodec", "lib/pkgconfig"):
            (prefix / directory).mkdir(parents=True, exist_ok=True)
        (prefix / "share/insta360-rs/complete").touch()
        (prefix / "include/libavcodec/avcodec.h").touch()
        (prefix / "lib/pkgconfig/libavcodec.pc").write_text("prefix=/old checkout\n")
        (cache / "sdk-prefix.txt").write_text(relative + "\n")
        return prefix

    def test_cold_build_then_cache_reuse_on_supported_platforms(self):
        source = self.root / "scripts"
        for platform in ("darwin", "win32", "linux"):
            with (
                self.subTest(platform=platform),
                tempfile.TemporaryDirectory() as directory,
            ):
                self.root = Path(directory).resolve()
                shutil.copytree(source, self.root / "scripts")
                env = self.env.copy()
                builder = (
                    [
                        runner.sys.executable,
                        str(self.root / "scripts/ci/build-native-wheel.py"),
                    ]
                    if platform != "linux"
                    else ["bash", str(self.root / "scripts/ci/build-wheel.sh")]
                )
                builds = []

                def run(args, *, env, cwd, check):
                    self.assertEqual(cwd, self.root)
                    if args[0] == "pkg-config":
                        self.assertEqual(
                            args[1:],
                            [
                                "--exists",
                                "libavutil",
                                "libavcodec",
                                "libavformat",
                                "libswscale",
                            ],
                        )
                        return subprocess.CompletedProcess(args, 1)
                    self.assertEqual(args, [*builder, "--prewarm"])
                    self.assertTrue(check)
                    builds.append(self.create_sdk(platform != "linux"))
                    return subprocess.CompletedProcess(args, 0)

                with (
                    patch.object(runner, "ROOT", self.root),
                    patch.object(runner.sys, "platform", platform),
                    patch.object(runner.platform, "machine", return_value="x86_64"),
                    patch.object(
                        runner.subprocess, "check_output", return_value=self.key + "\n"
                    ) as output,
                    patch.object(runner.subprocess, "run", side_effect=run),
                ):
                    runner.prepare_ffmpeg(env)
                    reused = self.env.copy()
                    runner.prepare_ffmpeg(reused)
                self.assertEqual(env, reused)
                self.assertEqual(len(builds), 1)
                prefix = builds[0]
                self.assertEqual(env["PKG_CONFIG_PATH"], str(prefix / "lib/pkgconfig"))
                self.assertEqual(
                    (prefix / "lib/pkgconfig/libavcodec.pc").read_text(),
                    f"prefix={prefix}\n",
                )
                for name, value in self.env.items():
                    if platform != "win32" or name != "PATH":
                        self.assertEqual(env[name], value)
                if platform != "linux":
                    self.assertEqual(env["FFMPEG_DIR"], str(prefix))
                self.assertEqual(output.call_count, 2)
                self.assertEqual(output.call_args.args[0], [*builder, "--cache-key"])

    def test_explicit_discovery_is_never_replaced(self):
        for setting in (
            "FFMPEG_DIR",
            "PKG_CONFIG_PATH",
            "PKG_CONFIG_LIBDIR",
            "PKG_CONFIG_PATH_aarch64_apple_darwin",
            "HOST_PKG_CONFIG_PATH",
            "TARGET_PKG_CONFIG_LIBDIR",
            "PKG_CONFIG_aarch64-apple-darwin",
            "PKG_CONFIG_aarch64_apple_darwin",
            "VCPKG_ROOT",
            "VCPKGRS_TRIPLET",
        ):
            with self.subTest(setting=setting):
                env = {**self.env, setting: "/caller prefix"}
                original = env.copy()
                with (
                    patch.object(runner.subprocess, "run") as run,
                    patch.object(runner.subprocess, "check_output") as output,
                ):
                    runner.prepare_ffmpeg(env)
                self.assertEqual(env, original)
                run.assert_not_called()
                output.assert_not_called()

    def test_system_libraries_do_not_build_an_sdk(self):
        env = {**self.env, "PKG_CONFIG": "/custom pkg-config"}
        with (
            patch.object(
                runner.subprocess,
                "run",
                return_value=subprocess.CompletedProcess([], 0),
            ) as run,
            patch.object(runner.subprocess, "check_output") as output,
        ):
            runner.prepare_ffmpeg(env)
        self.assertEqual(run.call_args.args[0][0], "/custom pkg-config")
        output.assert_not_called()
        self.assertNotIn("FFMPEG_DIR", env)

    def test_stale_or_incomplete_sdk_reenters_authoritative_builder(self):
        for stale in (True, False):
            with self.subTest(stale=stale):
                prefix = self.create_sdk(True, "b" * 64 if stale else self.key)
                if not stale:
                    (prefix / "share/insta360-rs/complete").unlink()
                with (
                    patch.object(runner.sys, "platform", "darwin"),
                    patch.object(
                        runner.subprocess, "check_output", return_value=self.key
                    ),
                    patch.object(
                        runner.subprocess,
                        "run",
                        side_effect=[
                            subprocess.CompletedProcess([], 1),
                            subprocess.CalledProcessError(9, "FFmpeg setup"),
                        ],
                    ) as run,
                ):
                    with self.assertRaises(subprocess.CalledProcessError):
                        runner.prepare_ffmpeg(self.env.copy())
                self.assertEqual(run.call_args.args[0][-1], "--prewarm")

    def test_setup_failure_stops_before_cargo(self):
        with (
            patch.dict(os.environ, self.env, clear=True),
            patch.object(runner.sys, "platform", "darwin"),
            patch.object(runner.subprocess, "check_output", return_value=self.key),
            patch.object(
                runner.subprocess,
                "run",
                side_effect=[
                    FileNotFoundError("pkg-config"),
                    subprocess.CalledProcessError(9, "FFmpeg setup"),
                ],
            ) as run,
            redirect_stderr(io.StringIO()),
        ):
            self.assertEqual(runner.main(), 9)
        self.assertEqual(run.call_count, 2)
        self.assertEqual(
            run.call_args.args[0][1],
            str(self.root / "scripts/ci/build-native-wheel.py"),
        )

    def test_unsupported_host_requires_explicit_or_system_libraries(self):
        with (
            patch.object(runner.sys, "platform", "linux"),
            patch.object(runner.platform, "machine", return_value="aarch64"),
            patch.object(
                runner.subprocess,
                "run",
                return_value=subprocess.CompletedProcess([], 1),
            ),
            patch.object(runner.subprocess, "check_output") as output,
        ):
            with self.assertRaisesRegex(RuntimeError, "Install FFmpeg"):
                runner.prepare_ffmpeg(self.env)
        output.assert_not_called()

    def test_invalid_cache_identity_stops_before_build(self):
        with (
            patch.object(runner.sys, "platform", "darwin"),
            patch.object(
                runner.subprocess, "check_output", return_value="../../outside"
            ),
            patch.object(
                runner.subprocess,
                "run",
                return_value=subprocess.CompletedProcess([], 1),
            ) as run,
        ):
            with self.assertRaisesRegex(RuntimeError, "invalid cache key"):
                runner.prepare_ffmpeg(self.env)
        self.assertEqual(run.call_count, 1)


if __name__ == "__main__":
    unittest.main()
