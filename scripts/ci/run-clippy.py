#!/usr/bin/env python3
"""Prepare native prerequisites and run the complete workspace Clippy check."""

import hashlib
import os
import subprocess  # nosec B404
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def prepare_environment() -> dict[str, str]:
    """Preserve caller settings and reuse the authoritative MNN builder."""
    env = os.environ.copy()
    if sys.platform == "darwin" and not env.get("SDKROOT"):
        # An implicit SDK can come from newer Command Line Tools while the
        # compiler/linker comes from the selected Xcode installation.
        env["SDKROOT"] = subprocess.check_output(
            ["xcrun", "--sdk", "macosx", "--show-sdk-path"],
            env=env,
            text=True,
        ).strip()  # nosec B603 B607
        if not env["SDKROOT"]:
            raise RuntimeError("xcrun did not return a macOS SDK path")

    builder = [sys.executable, str(ROOT / "scripts/ci/build-mnn.py")]
    if env.get("MNN_ROOT"):
        # Prepared CI/application prefixes must never be replaced or rebuilt.
        args = ["--verify-only", "--output", env["MNN_ROOT"]]
    else:
        configuration = subprocess.check_output(
            [*builder, "--configuration"], cwd=ROOT, env=env
        )  # nosec B603
        identity = hashlib.sha256(configuration).hexdigest()
        env["MNN_ROOT"] = str(ROOT / ".cache/mnn" / identity)
        args = ["--output", env["MNN_ROOT"]]
    subprocess.run([*builder, *args], cwd=ROOT, env=env, check=True)  # nosec B603
    return env


def main() -> int:
    try:
        env = prepare_environment()
        return subprocess.run(
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
            cwd=ROOT,
            env=env,
            check=False,
        ).returncode  # nosec B603 B607
    except subprocess.CalledProcessError as error:
        print(f"Clippy native setup failed: {error}", file=sys.stderr)
        return error.returncode
    except (OSError, RuntimeError) as error:
        print(f"Clippy native setup failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
