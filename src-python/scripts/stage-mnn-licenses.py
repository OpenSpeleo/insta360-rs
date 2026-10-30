"""Stage pristine MNN notices from Cargo's resolved source package for wheels."""

import argparse
import json
import lzma
import subprocess
import tarfile
from pathlib import Path, PurePosixPath


def source_archive(manifest):
    metadata = json.loads(
        subprocess.check_output(
            [
                "cargo",
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--manifest-path",
                str(manifest),
                "--no-default-features",
                "--features",
                "underwater-ai,ai-stitching",
            ],
            text=True,
        )
    )
    packages = [
        package for package in metadata["packages"] if package["name"] == "mnn-src"
    ]
    if len(packages) != 1:
        raise RuntimeError("Expected one Cargo-resolved mnn-src package")
    return Path(packages[0]["manifest_path"]).parent / "mnn-sources.tar.xz"


def stage_notices(archive, destination):
    notices = {}
    with lzma.open(archive, "rb") as compressed:
        with tarfile.open(fileobj=compressed, mode="r|") as source:
            for member in source:
                path = PurePosixPath(member.name)
                name = path.name.lower()
                if not member.isfile() or not (
                    "license" in name or "notice" in name or "lic" in path.parts
                ):
                    continue
                if path.is_absolute() or ".." in path.parts:
                    raise ValueError("Unsafe native notice path")
                with source.extractfile(member) as notice:
                    notices[path] = notice.read()
        # tar EOF can precede the XZ footer: validate the complete stream first.
        while compressed.read(1024 * 1024):
            pass
    if PurePosixPath("mnn/LICENSE.txt") not in notices:
        raise RuntimeError("MNN source package lacks its original license")
    notices[PurePosixPath("MNN-LICENSE.txt")] = notices[
        PurePosixPath("mnn/LICENSE.txt")
    ]
    for relative, data in notices.items():
        target = destination.joinpath(*relative.parts)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--manifest-path", type=Path, required=True)
    args = parser.parse_args()
    stage_notices(source_archive(args.manifest_path), args.destination)


if __name__ == "__main__":
    main()
