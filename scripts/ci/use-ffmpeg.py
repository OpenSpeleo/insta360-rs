"""Relocate a prepared FFmpeg SDK and export its compiler/runtime paths."""

import os
from pathlib import Path
import subprocess
import sys


def prepend_path(directory: Path, variable: str) -> str:
    inherited = os.environ.get(variable)
    return str(directory) + (os.pathsep + inherited if inherited else "")


def configure(root: Path) -> dict[str, str]:
    native = sys.platform in ("darwin", "win32")
    cache = (root / ".cache" / ("wheel-native" if native else "wheel")).resolve()
    relative = Path((cache / "sdk-prefix.txt").read_text().strip())
    prefix = (cache / relative).resolve()
    if not prefix.is_relative_to(cache) or not (prefix / "include/libavcodec/avcodec.h").is_file():
        raise RuntimeError("Prepared FFmpeg installation is missing or has an invalid prefix")
    pkg_config = prefix / "lib/pkgconfig"
    for path in pkg_config.glob("*.pc"):
        text = path.read_text()
        lines = text.splitlines()
        previous = next(line.removeprefix("prefix=") for line in lines if line.startswith("prefix="))
        path.write_text(text.replace(previous, str(prefix)))
    environment = {"PKG_CONFIG_PATH": str(pkg_config)}
    if not native:
        environment["LD_LIBRARY_PATH"] = prepend_path(prefix / "lib", "LD_LIBRARY_PATH")
        return environment
    environment["FFMPEG_DIR"] = str(prefix)
    if sys.platform == "win32":
        environment["PATH"] = prepend_path(prefix / "bin", "PATH")
    else:
        # Resolve the SDK's relocatable @rpath names without replacing the
        # absolute libraries used by Homebrew's separate fixture-generator CLI.
        environment["DYLD_FALLBACK_LIBRARY_PATH"] = prepend_path(
            prefix / "lib", "DYLD_FALLBACK_LIBRARY_PATH"
        )
        sdk = os.environ.get("SDKROOT") or subprocess.check_output(
            ["xcrun", "--sdk", "macosx", "--show-sdk-path"], text=True
        ).strip()
        clang = os.environ.get("LIBCLANG_PATH")
        if not clang:
            executable = subprocess.check_output(
                ["xcrun", "--find", "clang"], text=True
            ).strip()
            clang = str(Path(executable).parent.parent / "lib")
        environment.update(
            MACOSX_DEPLOYMENT_TARGET=os.environ.get("MACOSX_DEPLOYMENT_TARGET", "11.0"),
            SDKROOT=sdk,
            LIBCLANG_PATH=clang,
            BINDGEN_EXTRA_CLANG_ARGS=os.environ.get(
                "BINDGEN_EXTRA_CLANG_ARGS", f"--sysroot={sdk}"
            ),
        )
    return environment


if __name__ == "__main__":
    environment = configure(Path(__file__).resolve().parents[2])
    with open(os.environ["GITHUB_ENV"], "a") as destination:
        for key, value in environment.items():
            if key != "PATH":
                print(f"{key}={value}", file=destination)
    if "PATH" in environment:
        with open(os.environ["GITHUB_PATH"], "a") as destination:
            print(Path(environment["FFMPEG_DIR"]) / "bin", file=destination)
    print("Prepared FFmpeg libraries configured for this checkout")
