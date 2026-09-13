"""The MSRV check must follow the manifest without altering native build inputs."""

import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


spec = importlib.util.spec_from_file_location("check_msrv", Path(__file__).with_name("check-msrv.py"))
msrv = importlib.util.module_from_spec(spec)
spec.loader.exec_module(msrv)


class MsrvCheckTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.manifest = self.root / "Cargo.toml"

    def version(self, value):
        self.manifest.write_text(f'[workspace.package]\nrust-version = "{value}"\n')

    def test_manifest_changes_select_the_compiler_and_preserve_build_inputs(self):
        environment = {
            "RUSTUP_TOOLCHAIN": "primary-compiler",
            "RUSTFLAGS": "caller rust flags",
            "CARGO_ENCODED_RUSTFLAGS": "caller encoded flags",
            "CARGO_TARGET_DIR": "caller-target",
            "CARGO_PROFILE_DEV_OPT_LEVEL": "caller-profile",
            "FFMPEG_DIR": "prepared-ffmpeg",
            "MNN_ROOT": "prepared-mnn",
            "PKG_CONFIG_PATH": "caller-pkgconfig",
            "MACOSX_DEPLOYMENT_TARGET": "caller-macos-target",
        }
        for declared, compiler in (("1.90", "1.90.0"), ("1.95.2", "1.95.2")):
            with self.subTest(declared=declared), mock.patch.dict(os.environ, environment):
                self.version(declared)
                original = self.manifest.read_bytes()
                inherited = []
                with mock.patch.object(msrv.subprocess, "run", side_effect=lambda *a, **k: inherited.append(dict(os.environ))) as run:
                    msrv.check(self.root)
                self.assertEqual(run.call_args_list, [
                    mock.call(
                        ["rustup", "toolchain", "install", compiler, "--profile", "minimal", "--no-self-update"],
                        cwd=self.root, check=True,
                    ),
                    mock.call(
                        ["rustup", "run", compiler, "cargo", "check", "--locked", "--all-targets", "--all-features"],
                        cwd=self.root, check=True,
                    ),
                ])
                for observed in inherited:
                    self.assertEqual({key: observed[key] for key in environment}, environment)
                self.assertEqual(self.manifest.read_bytes(), original)

    def test_invalid_minimum_fails_before_any_toolchain_command(self):
        for value in ("stable", "1", "1.90 --help", "1.90.0-nightly"):
            with self.subTest(value=value), mock.patch.object(msrv.subprocess, "run") as run:
                self.version(value)
                with self.assertRaisesRegex(ValueError, "numeric Rust release"):
                    msrv.check(self.root)
                run.assert_not_called()

    def test_failed_install_never_runs_the_primary_compiler_as_a_fallback(self):
        self.version("1.90")
        with mock.patch.object(msrv.subprocess, "run", side_effect=subprocess.CalledProcessError(1, "rustup")) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                msrv.check(self.root)
            self.assertEqual(run.call_count, 1)

    def test_ci_uses_the_manifest_driven_check(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/ci.yml").read_text()
        self.assertIn("run: python scripts/ci/check-msrv.py\n", workflow)


if __name__ == "__main__":
    unittest.main()
