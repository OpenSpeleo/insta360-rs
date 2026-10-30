#!/usr/bin/env python3
"""Prepare FFmpeg discovery and run the complete workspace Clippy check."""

import os
import platform
import re
import runpy
import subprocess  # nosec B404
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def prepare_ffmpeg(env: dict[str, str]) -> None:
    """Reuse caller discovery or prepare the SDK with the existing wheel recipe."""
    # Explicit discovery settings belong to the caller, including cross builds.
    # Invalid selections must fail rather than silently selecting another SDK.
    if env.get("FFMPEG_DIR") or any(
        value
        and (
            key.startswith(
                (
                    "PKG_CONFIG_PATH",
                    "PKG_CONFIG_LIBDIR",
                    "PKG_CONFIG_SYSROOT_DIR",
                    "HOST_PKG_CONFIG",
                    "TARGET_PKG_CONFIG",
                    "VCPKG_ROOT",
                    "VCPKGRS_TRIPLET",
                )
            )
            or re.fullmatch(r"PKG_CONFIG_[a-z0-9_-]+", key)
        )
        for key, value in env.items()
    ):
        return
    try:
        available = (
            subprocess.run(
                [
                    env.get("PKG_CONFIG", "pkg-config"),
                    "--exists",
                    "libavutil",
                    "libavcodec",
                    "libavformat",
                    "libswscale",
                ],
                cwd=ROOT,
                env=env,
                check=False,
            ).returncode
            == 0
        )  # nosec B603 B607
    except FileNotFoundError:
        available = False
    if available:
        return

    if sys.platform in ("darwin", "win32"):
        builder = [sys.executable, str(ROOT / "scripts/ci/build-native-wheel.py")]
        cache = ROOT / ".cache/wheel-native"
    elif sys.platform == "linux" and platform.machine().lower() in ("x86_64", "amd64"):
        builder = ["bash", str(ROOT / "scripts/ci/build-wheel.sh")]
        cache = ROOT / ".cache/wheel"
    else:
        raise RuntimeError(
            "Install FFmpeg development libraries or set FFMPEG_DIR on this platform"
        )

    # Recheck the authoritative recipe/host identity; a marker alone can be stale.
    key = subprocess.check_output(
        [*builder, "--cache-key"],
        cwd=ROOT,
        env=env,
        text=True,
    ).strip()  # nosec B603
    if len(key) != 64 or any(character not in "0123456789abcdef" for character in key):
        raise RuntimeError("FFmpeg builder returned an invalid cache key")
    relative = f"native/{key}/prefix"
    marker = cache / "sdk-prefix.txt"
    if (
        not marker.is_file()
        or marker.read_text(encoding="utf-8").strip() != relative
        or not (cache / relative / "share/insta360-rs/complete").is_file()
    ):
        subprocess.run(
            [*builder, "--prewarm"],
            cwd=ROOT,
            env=env,
            check=True,
        )  # nosec B603
    configure = runpy.run_path(str(ROOT / "scripts/ci/use-ffmpeg.py"))["configure"]
    env.update(configure(ROOT, env))


def prepare_environment() -> dict[str, str]:
    """Preserve caller settings and prepare FFmpeg for the Cargo-owned build."""
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

    prepare_ffmpeg(env)
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
