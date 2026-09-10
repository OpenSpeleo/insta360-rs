#!/usr/bin/env python3
"""Build the exact independent MNN CPU prerequisite for underwater-ai."""

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess  # nosec B404
import tarfile
import tempfile
import urllib.request
from pathlib import Path

VERSION = "3.6.1"
COMMIT = "d407447ed56c4121a11ccbd266dc184ca1ead0c2"
SHA256 = "13dca9547df7dac40ab40c7318136406f4a33dfe99cd40bfaa4dbb2270cb8795"
URL = f"https://github.com/alibaba/MNN/archive/{COMMIT}.tar.gz"
MARKER = f"{VERSION} {COMMIT}\n"
CMAKE_FLAGS = (
    "-DMNN_BUILD_SHARED_LIBS=OFF",
    "-DMNN_SEP_BUILD=OFF",
    "-DMNN_BUILD_TOOLS=OFF",
    "-DMNN_BUILD_CONVERTER=OFF",
    "-DMNN_BUILD_DEMO=OFF",
    "-DMNN_OPENCL=OFF",
    "-DMNN_METAL=OFF",
    "-DMNN_VULKAN=OFF",
    "-DMNN_OPENMP=OFF",
    "-DMNN_BUILD_TEST=OFF",
    "-DMNN_BUILD_TRAIN=OFF",
    "-DMNN_BUILD_OPENCV=OFF",
    "-DMNN_CUDA=OFF",
    "-DMNN_KLEIDIAI=OFF",
    "-DMNN_SME2=OFF",
    "-DMNN_WIN_RUNTIME_MT=OFF",
    "-DCMAKE_BUILD_TYPE=Release",
    "-DCMAKE_POSITION_INDEPENDENT_CODE=ON",
)


def build_configuration() -> dict:
    """Bind cached native bytes to source, architecture, and compiler settings."""
    return {
        "version": VERSION,
        "commit": COMMIT,
        "source_sha256": SHA256,
        "system": platform.system(),
        "machine": platform.machine().lower(),
        "flags": list(CMAKE_FLAGS),
        "environment": {
            name: os.environ.get(name, "")
            for name in (
                "CC",
                "CXX",
                "CFLAGS",
                "CXXFLAGS",
                "LDFLAGS",
                "CMAKE_GENERATOR",
                "CMAKE_GENERATOR_PLATFORM",
                "MACOSX_DEPLOYMENT_TARGET",
                "SDKROOT",
            )
        },
        "builder_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    }


def verify_prefix(output: Path, configuration: dict) -> bool:
    """Reject stale, truncated, or altered cached libraries and headers."""
    try:
        metadata = json.loads((output / "insta360-mnn-build.json").read_text())
        if metadata["configuration"] != configuration:
            return False
        if (output / "insta360-mnn-version.txt").read_text() != MARKER:
            return False
        files = metadata["files"]
        if not isinstance(files, dict) or "include/MNN/Interpreter.hpp" not in files:
            return False
        if not any(name in files for name in ("lib/libMNN.a", "lib/MNN.lib")):
            return False
        for name, digest in files.items():
            relative = Path(name)
            if relative.is_absolute() or ".." in relative.parts:
                return False
            if hashlib.sha256((output / relative).read_bytes()).hexdigest() != digest:
                return False
        return True
    except (OSError, ValueError, KeyError, TypeError):
        return False


def extract_source(archive: Path, destination: Path) -> Path:
    """Validate archive paths and extract regular files without following links."""
    with tarfile.open(archive) as source:
        members = source.getmembers()
        for member in members:
            path = Path(member.name)
            if path.is_absolute() or ".." in path.parts:
                raise ValueError("MNN source archive contains an unsafe path")
            if not (member.isfile() or member.isdir() or member.issym()):
                raise ValueError("MNN source archive contains a special file")
        for member in members:
            target = destination / member.name
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                target.parent.mkdir(parents=True, exist_ok=True)
                with (
                    source.extractfile(member) as input_file,
                    target.open("wb") as output,
                ):
                    shutil.copyfileobj(input_file, output)
            # Symlinks belong to unused application examples; the CPU build
            # needs only regular source files and does not extract those links.
    return destination / f"MNN-{COMMIT}"


def build(output: Path, jobs: int) -> None:
    """Verify official source bytes, build static CPU MNN, and publish its prefix."""
    output = output.resolve()
    configuration = build_configuration()
    if output.exists():
        if verify_prefix(output, configuration):
            print(output)
            return
        raise ValueError(
            f"MNN output already exists but is incomplete or incompatible: {output}"
        )
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix="insta360-mnn-", dir=output.parent
    ) as directory:
        work = Path(directory)
        archive = work / "source.tar.gz"
        with (
            urllib.request.urlopen(URL, timeout=120) as response,  # nosec B310
            archive.open("wb") as target,
        ):
            shutil.copyfileobj(response, target)
        if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
            raise ValueError(
                "MNN source digest does not match the pinned commit archive"
            )
        source = extract_source(archive, work / "source")
        build_dir = work / "build"
        prefix = work / "prefix"
        subprocess.run(
            ["cmake", "-S", str(source), "-B", str(build_dir), *CMAKE_FLAGS], check=True
        )  # nosec B603 B607
        subprocess.run(
            [
                "cmake",
                "--build",
                str(build_dir),
                "--config",
                "Release",
                "--target",
                "MNN",
                "--parallel",
                str(jobs),
            ],
            check=True,
        )  # nosec B603 B607
        candidates = [
            build_dir / "libMNN.a",
            build_dir / "Release/MNN.lib",
            build_dir / "MNN.lib",
        ]
        library = next((path for path in candidates if path.is_file()), None)
        if library is None:
            raise ValueError("MNN build did not produce its static library")
        (prefix / "lib").mkdir(parents=True)
        shutil.copy2(library, prefix / "lib" / library.name)
        shutil.copytree(source / "include", prefix / "include")
        shutil.copy2(source / "LICENSE.txt", prefix / "MNN-LICENSE.txt")
        # These notices accompany code included by MNN's static CPU library.
        notices = [
            source / "3rd_party/half/LICENSE.txt",
            *sorted((source / "3rd_party/lic").iterdir()),
        ]
        with (prefix / "MNN-THIRD-PARTY-NOTICES.txt").open("wb") as target:
            for notice in notices:
                target.write(
                    f"\n--- {notice.relative_to(source).as_posix()} ---\n".encode()
                )
                target.write(notice.read_bytes())
        (prefix / "insta360-mnn-version.txt").write_text(MARKER)
        metadata = {
            "configuration": configuration,
            "files": {
                path.relative_to(prefix).as_posix(): hashlib.sha256(
                    path.read_bytes()
                ).hexdigest()
                for path in sorted(prefix.rglob("*"))
                if path.is_file()
            },
        }
        (prefix / "insta360-mnn-build.json").write_text(
            json.dumps(metadata, indent=2) + "\n"
        )
        prefix.rename(output)
    print(output)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", "--prefix", required=True, type=Path)
    parser.add_argument("--jobs", type=int, default=min(os.cpu_count() or 1, 8))
    args = parser.parse_args()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    build(args.output, args.jobs)


if __name__ == "__main__":
    main()
