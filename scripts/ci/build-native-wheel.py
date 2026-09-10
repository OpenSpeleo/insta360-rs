"""Build and repair a macOS or Windows wheel without running runtime tests."""

import argparse
import hashlib
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[2]


def run(*command, **kwargs):
    print("+", *map(str, command), flush=True)
    subprocess.run(list(map(str, command)), check=True, **kwargs)


def output(*command, env=None):
    return subprocess.check_output(list(map(str, command)), text=True, env=env).strip()


def verify_archive(wheel, target):
    if "-cp310-abi3-" not in wheel.name or target not in wheel.name:
        raise RuntimeError(f"Unexpected wheel platform or ABI: {wheel.name}")
    with zipfile.ZipFile(wheel) as archive:
        names = archive.namelist()
        suffix = ".dll" if sys.platform == "win32" else ".dylib"
        for library in ("avcodec", "avformat", "avutil", "swscale", "x265"):
            if not any(
                library in name.lower() and name.endswith(suffix) for name in names
            ):
                raise RuntimeError(f"Wheel is missing bundled {library}")
        if sys.platform == "win32":
            if not any(
                name.rsplit("/", 1)[-1].startswith("z-") and name.endswith(".dll")
                for name in names
            ):
                raise RuntimeError("Wheel is missing the bundled zlib DLL")
        for filename in (
            "FFMPEG-LICENSE.txt", "X265-LICENSE.txt", "SOURCES.txt",
            "MNN-LICENSE.txt", "MNN-THIRD-PARTY-NOTICES.txt",
        ):
            if not any(
                ".dist-info/licenses/" in name and name.endswith(filename)
                for name in names
            ):
                raise RuntimeError(f"Wheel is missing runtime license {filename}")
        for filename in ("ffmpeg-8.1.2.tar.xz", "x265_4.1.tar.gz"):
            if not any(
                "/_licenses/sources/" in name and name.endswith(filename)
                for name in names
            ):
                raise RuntimeError(f"Wheel is missing runtime source {filename}")
        if sys.platform == "win32":
            for filename in ("ZLIB-LICENSE.txt", "zlib-1.3.2.tar.gz"):
                if not any(name.endswith(filename) for name in names):
                    raise RuntimeError(f"Wheel is missing {filename}")
        for filename in ("py.typed", "__init__.pyi"):
            if f"insta360_rs/{filename}" not in names:
                raise RuntimeError(f"Wheel is missing {filename}")
    print(f"Verified repaired wheel contents: {wheel.name}")


def prepare_mnn(cache, env):
    """Build MNN with the same target and compiler environment as the wheel."""
    builder = ROOT / "scripts/ci/build-mnn.py"
    prefix = cache / "mnn" / hashlib.sha256(builder.read_bytes()).hexdigest()
    run(sys.executable, builder, "--output", prefix, env=env)
    env["MNN_ROOT"] = str(prefix)
    return prefix


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    if sys.platform not in ("darwin", "win32"):
        parser.error("Use build-wheel.sh for Linux wheels")
    machine = platform.machine().lower()
    if sys.platform == "win32" and machine not in ("amd64", "x86_64"):
        parser.error("The Windows builder supports x86_64")
    if sys.platform == "darwin" and machine not in ("arm64", "x86_64"):
        parser.error("The macOS builder supports arm64 and x86_64")

    env = os.environ.copy()
    with (ROOT / "rust-toolchain.toml").open("rb") as toolchain_file:
        env["RUSTUP_TOOLCHAIN"] = tomllib.load(toolchain_file)["toolchain"]["channel"]
    if sys.platform == "darwin":
        env.setdefault("MACOSX_DEPLOYMENT_TARGET", "11.0")
        env.setdefault("SDKROOT", output("xcrun", "--sdk", "macosx", "--show-sdk-path"))
        env.setdefault(
            "LIBCLANG_PATH",
            str(Path(output("xcrun", "--find", "clang")).parent.parent / "lib"),
        )
        env.setdefault("BINDGEN_EXTRA_CLANG_ARGS", f"--sysroot={env['SDKROOT']}")
    cache = ROOT / ".cache" / "wheel-native"
    cache.mkdir(parents=True, exist_ok=True)
    env["INSTA360_WHEEL_CACHE"] = str(cache)
    env.setdefault("INSTA360_WHEEL_JOBS", str(min(os.cpu_count() or 2, 4)))
    env.setdefault("CARGO_BUILD_JOBS", env["INSTA360_WHEEL_JOBS"])
    env["CARGO_TARGET_DIR"] = str(ROOT / "target" / "wheel-native")
    # CMake 4 removed OLD policies used by x265; install CMake 3 in setup.
    import cmake

    cmake_executable = Path(cmake.CMAKE_BIN_DIR) / (
        "cmake.exe" if sys.platform == "win32" else "cmake"
    )
    env["INSTA360_CMAKE"] = str(cmake_executable)
    native_script = ROOT / "scripts" / "ci" / "build-ffmpeg-native.sh"
    runtime_config = native_script.with_name("ffmpeg-runtime-config.sh")
    fingerprint = hashlib.sha256(native_script.read_bytes())
    fingerprint.update(runtime_config.read_bytes())
    fingerprint.update(platform.platform().encode())
    fingerprint.update(env.get("MACOSX_DEPLOYMENT_TARGET", "").encode())
    fingerprint.update(output(cmake_executable, "--version").encode())
    prefix = cache / "native" / fingerprint.hexdigest() / "prefix"
    env["INSTA360_FFMPEG_PREFIX"] = str(prefix)

    if sys.platform == "win32":
        msys = Path(env["INSTA360_MSYS_BIN"])
        # Keep MSVC link.exe ahead of MSYS link.exe, and use MSYS only for the
        # native C/C++ build. Cargo and maturin retain the original Windows PATH.
        entries = env["PATH"].split(os.pathsep)
        msvc = [entry for entry in entries if (Path(entry) / "cl.exe").is_file()]
        if not msvc:
            raise RuntimeError("Run setup-native-wheel to configure MSVC first")
        native_env = env.copy()
        native_env["PATH"] = os.pathsep.join(msvc + [str(msys)] + entries)
        script_path = output(msys / "cygpath.exe", "-u", native_script)
        run(msys / "bash.exe", script_path, env=native_env)
        env["PATH"] = str(prefix / "bin") + os.pathsep + env["PATH"]
        target = "win_amd64"
    else:
        run("bash", native_script, env=env)
        env["DYLD_LIBRARY_PATH"] = str(prefix / "lib")
        target = f"macosx_{env['MACOSX_DEPLOYMENT_TARGET'].replace('.', '_')}_{machine}"
    env["FFMPEG_DIR"] = str(prefix)
    env["PKG_CONFIG_PATH"] = str(prefix / "lib" / "pkgconfig")

    destination = args.out.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    mnn_prefix = prepare_mnn(cache, env)

    with tempfile.TemporaryDirectory(prefix="insta360-wheel-") as directory:
        temporary = Path(directory)
        source = temporary / "source"
        shutil.copytree(
            ROOT,
            source,
            ignore=shutil.ignore_patterns(
                ".git", "target", ".cache", "dist", ".venv", "__pycache__", ".DS_Store"
            ),
        )
        package = source / "src-python"
        notices = package / "python" / "insta360_rs" / "_licenses"
        sources = notices / "sources"
        notices.mkdir(parents=True, exist_ok=True)
        for notice in ("MNN-LICENSE.txt", "MNN-THIRD-PARTY-NOTICES.txt"):
            shutil.copy2(mnn_prefix / notice, notices)
        sources.mkdir(parents=True)
        for path in (prefix / "share" / "insta360-rs").glob("*.txt"):
            shutil.copy2(path, notices)
        for path in (
            native_script,
            runtime_config,
            ROOT / "scripts" / "ci" / "requirements-wheel.txt",
            Path(__file__),
            cache / "downloads" / "ffmpeg-8.1.2.tar.xz",
            cache / "downloads" / "x265_4.1.tar.gz",
        ):
            shutil.copy2(path, sources)
        if sys.platform == "win32":
            shutil.copy2(cache / "downloads" / "zlib-1.3.2.tar.gz", sources)
        (notices / "BUILD-ENVIRONMENT.txt").write_text(
            "\n".join(
                [
                    platform.platform(),
                    output("rustc", "--version", env=env),
                    output("cargo", "--version", env=env),
                    output(sys.executable, "-m", "maturin", "--version"),
                    output(cmake_executable, "--version"),
                ]
            )
            + "\n"
        )
        run(
            sys.executable,
            package / "scripts" / "stage-project-licenses.py",
            package,
            "--runtime",
        )
        unrepaired = temporary / "unrepaired"
        # Maturin 1.15 builds wheels from the generated sdist with this flag.
        run(
            sys.executable,
            "-m",
            "maturin",
            "build",
            "--release",
            "--locked",
            "--sdist",
            "--strip",
            "--interpreter",
            sys.executable,
            "--out",
            unrepaired,
            cwd=package,
            env=env,
        )
        wheels = list(unrepaired.glob("*.whl"))
        if len(wheels) != 1:
            raise RuntimeError(f"Expected one native wheel, found {wheels}")
        if sys.platform == "win32":
            run(
                sys.executable,
                "-m",
                "delvewheel",
                "repair",
                "--add-path",
                prefix / "bin",
                "--wheel-dir",
                destination,
                wheels[0],
                env=env,
            )
        else:
            run(
                "delocate-wheel",
                "--require-archs",
                machine,
                "--require-target-macos-version",
                env["MACOSX_DEPLOYMENT_TARGET"],
                "--wheel-dir",
                destination,
                wheels[0],
                env=env,
            )
        repaired = list(destination.glob(f"*-{target}.whl"))
        if len(repaired) != 1:
            raise RuntimeError(
                f"Expected one repaired wheel for {target}, found {repaired}"
            )
        verify_archive(repaired[0], target)
        # Linux provides the canonical source distribution; platform jobs only
        # contribute their repaired wheel to the release artifact set.


if __name__ == "__main__":
    main()
