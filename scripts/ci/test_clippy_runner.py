"""Regression coverage for native setup in direct Git/prek Clippy runs."""

import importlib.util
import io
import os
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
        ):
            context.start()
            self.addCleanup(context.stop)

    def test_macos_selects_sdk_before_mnn_build_and_runs_full_clippy_scope(self):
        events = []

        def output(args, *, env, **kwargs):
            events.append(args)
            if args[0] == "xcrun":
                self.assertEqual(args, ["xcrun", "--sdk", "macosx", "--show-sdk-path"])
                return "/selected Xcode/SDKs/MacOSX.sdk\n"
            self.assertEqual(args[-1], "--configuration")
            self.assertEqual(env["SDKROOT"], "/selected Xcode/SDKs/MacOSX.sdk")
            self.assertNotIn("MNN_ROOT", env)
            self.assertEqual(kwargs["cwd"], self.root)
            return b'{"configuration": "from authoritative builder"}\n'

        def run(args, *, env, cwd, check):
            events.append(args)
            self.assertEqual(cwd, self.root)
            self.assertEqual(env["SDKROOT"], "/selected Xcode/SDKs/MacOSX.sdk")
            self.assertEqual(Path(env["MNN_ROOT"]).parent, self.root / ".cache/mnn")
            self.assertEqual(env["PATH"], "/caller/bin")
            if args[0] == "cargo":
                self.assertFalse(check)
                self.assertEqual(
                    args,
                    [
                        "cargo",
                        "clippy",
                        "--workspace",
                        "--locked",
                        "--all-targets",
                        "--all-features",
                        "--",
                        "-D",
                        "warnings",
                    ],
                )
                return subprocess.CompletedProcess(args, 101)
            self.assertTrue(check)
            self.assertEqual(args[-2:], ["--output", env["MNN_ROOT"]])
            return subprocess.CompletedProcess(args, 0)

        with (
            patch.object(runner.sys, "platform", "darwin"),
            patch.object(runner.subprocess, "check_output", side_effect=output),
            patch.object(runner.subprocess, "run", side_effect=run),
        ):
            self.assertEqual(runner.main(), 101)
        self.assertEqual(len(events), 4)
        self.assertEqual(events[2][1], str(self.root / "scripts/ci/build-mnn.py"))
        self.assertNotIn("SDKROOT", os.environ)
        self.assertNotIn("MNN_ROOT", os.environ)

    def test_prepared_environment_is_verified_and_preserved_on_every_platform(self):
        prepared = {
            "PATH": "/caller/bin",
            "MNN_ROOT": "/prepared MNN",
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
                run.assert_called_once_with(
                    [
                        runner.sys.executable,
                        str(self.root / "scripts/ci/build-mnn.py"),
                        "--verify-only",
                        "--output",
                        "/prepared MNN",
                    ],
                    cwd=self.root,
                    env=prepared,
                    check=True,
                )

    def test_cache_identity_is_stable_and_changes_with_builder_configuration(self):
        with (
            patch.object(runner.sys, "platform", "linux"),
            patch.object(
                runner.subprocess,
                "check_output",
                side_effect=[b"first", b"first", b"changed"],
            ),
            patch.object(runner.subprocess, "run"),
        ):
            first = runner.prepare_environment()
            self.assertEqual(runner.prepare_environment(), first)
            self.assertNotEqual(
                runner.prepare_environment()["MNN_ROOT"], first["MNN_ROOT"]
            )
        self.assertNotIn("MNN_ROOT", os.environ)

    def test_discovery_failure_prevents_build_and_cargo(self):
        for platform in ("darwin", "linux"):
            with (
                self.subTest(platform=platform),
                patch.object(runner.sys, "platform", platform),
                patch.object(
                    runner.subprocess,
                    "check_output",
                    side_effect=subprocess.CalledProcessError(7, "native discovery"),
                ),
                patch.object(runner.subprocess, "run") as run,
                redirect_stderr(io.StringIO()),
            ):
                self.assertEqual(runner.main(), 7)
                run.assert_not_called()

    def test_failed_build_or_explicit_prefix_verification_prevents_cargo(self):
        for prefix in ("", "/invalid prefix"):
            with (
                self.subTest(prefix=prefix),
                patch.dict(os.environ, {"MNN_ROOT": prefix}),
                patch.object(runner.sys, "platform", "linux"),
                patch.object(
                    runner.subprocess, "check_output", return_value=b"configuration"
                ),
                patch.object(
                    runner.subprocess,
                    "run",
                    side_effect=subprocess.CalledProcessError(9, "MNN setup"),
                ) as run,
                redirect_stderr(io.StringIO()),
            ):
                self.assertEqual(runner.main(), 9)
                self.assertEqual(run.call_count, 1)
                args = run.call_args.args[0]
                self.assertEqual(args[1], str(self.root / "scripts/ci/build-mnn.py"))
                self.assertEqual("--verify-only" in args, bool(prefix))

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


if __name__ == "__main__":
    unittest.main()
