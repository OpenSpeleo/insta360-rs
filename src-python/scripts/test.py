"""Build both distributions, rebuild the sdist, and test isolated installations.

Run with a Python environment containing build and maturin. FFmpeg development
libraries must be available to Cargo; ffmpeg and ffprobe generate test media.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from email.parser import BytesParser
from pathlib import Path

PACKAGE = Path(__file__).resolve().parents[1]
MNN_NOTICES = ("MNN-LICENSE.txt", "MNN-THIRD-PARTY-NOTICES.txt")


def run(*command, cwd=None, env=None, timeout=None):
    print("+", *map(str, command), flush=True)
    subprocess.run(
        list(map(str, command)), cwd=cwd, env=env, timeout=timeout, check=True
    )


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def stage_source(workspace, destination):
    mnn_root = os.environ.get("MNN_ROOT")
    require(mnn_root, "MNN_ROOT must identify the pinned MNN CPU prefix")
    mnn_root = Path(mnn_root)
    for filename in MNN_NOTICES:
        require((mnn_root / filename).is_file(), f"MNN_ROOT lacks {filename}")
    development = shutil.ignore_patterns(
        ".git", "target", ".cache", "dist", ".venv", "__pycache__", ".DS_Store"
    )
    extensions = shutil.ignore_patterns("_native*.so", "_native*.pyd")

    def ignore(directory, names):
        ignored = development(directory, names)
        if Path(directory) == workspace / "src-python" / "python" / "insta360_rs":
            ignored.update(extensions(directory, names))
        return ignored

    shutil.copytree(
        workspace,
        destination,
        ignore=ignore,
    )
    package = destination / "src-python"
    notices = package / "python" / "insta360_rs" / "_licenses"
    notices.mkdir(parents=True, exist_ok=True)
    for filename in MNN_NOTICES:
        shutil.copy2(mnn_root / filename, notices / filename)
    # Use the same notice paths as release builders. Root Cargo and Python
    # LICENSE.md files otherwise collide in Maturin's workspace source archive.
    run(
        sys.executable,
        Path(__file__).with_name("stage-project-licenses.py"),
        package,
        "--runtime",
    )
    return package


def check_wheel(path):
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        for filename in ["__init__.py", "__init__.pyi", "py.typed"]:
            require(f"insta360_rs/{filename}" in names, f"Wheel lacks {filename}")
        require(
            any(
                name.startswith("insta360_rs/_native.")
                and name.endswith((".so", ".pyd"))
                for name in names
            ),
            "Wheel lacks a native extension",
        )
        for filename in ["LICENSE.md", "NOTICE.md"]:
            require(
                any(name.endswith("/" + filename) for name in names),
                f"Wheel lacks {filename}",
            )
        for filename in MNN_NOTICES:
            require(
                any(
                    ".dist-info/licenses/" in name and name.endswith("/" + filename)
                    for name in names
                ),
                f"Wheel lacks runtime license {filename}",
            )
        metadata_name = next(
            name for name in names if name.endswith(".dist-info/METADATA")
        )
        metadata = BytesParser().parsebytes(archive.read(metadata_name))
        require(metadata["Name"] == "insta360-rs", "Incorrect distribution name")
        require(metadata["Requires-Python"] == ">=3.10", "Incorrect Python floor")
        require(metadata["License-Expression"] == "Apache-2.0", "Incorrect license")
        wheel_name = next(name for name in names if name.endswith(".dist-info/WHEEL"))
        wheel = BytesParser().parsebytes(archive.read(wheel_name))
        require(wheel["Root-Is-Purelib"] == "false", "Native wheel marked pure Python")
        require(
            any(tag.startswith("cp310-abi3-") for tag in wheel.get_all("Tag", [])),
            "Wheel does not advertise the Python 3.10 stable ABI",
        )
        require(
            not any(
                "/.venv/" in name or "/tests/" in name or "/__pycache__/" in name
                for name in names
            ),
            "Wheel contains development artifacts",
        )


def check_sdist(path, destination):
    with tarfile.open(path) as archive:
        # Do not depend on the Python 3.12+ extraction-filter API: the package's
        # minimum supported interpreter is 3.10. Only regular files/directories
        # inside one archive root are accepted.
        names = archive.getnames()
        for member in archive.getmembers():
            target = (destination / member.name).resolve()
            require(
                target.is_relative_to(destination.resolve()), "Unsafe sdist member path"
            )
            require(
                member.isfile() or member.isdir(), "Unexpected sdist link/special file"
            )
        for filename in [
            "pyproject.toml",
            "python/insta360_rs/__init__.pyi",
            "python/insta360_rs/py.typed",
            "docs/README.md",
            "tests/test_public_api.py",
            "scripts/test.py",
            "scripts/run-tests.py",
            "scripts/stage-project-licenses.py",
            *(f"python/insta360_rs/_licenses/{name}" for name in MNN_NOTICES),
        ]:
            require(
                any(name.endswith("/" + filename) for name in names),
                f"Source distribution lacks {filename}",
            )
        require(
            not any(
                any(
                    part in name
                    for part in (
                        "/.venv/",
                        "/dist/",
                        "/__pycache__/",
                        "/.cache/",
                        "/target/",
                    )
                )
                for name in names
            ),
            "Source distribution contains build artifacts",
        )
        for member in archive.getmembers():
            target = destination / member.name
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.extractfile(member) as source, target.open("wb") as output:
                    shutil.copyfileobj(source, output)
    roots = list(destination.iterdir())
    require(len(roots) == 1, "Source distribution must have one root")
    root = roots[0]
    manifests = list(root.rglob("src-rust/assets/model-bundle.json"))
    require(len(manifests) == 1, "Source distribution must have one asset manifest")
    manifest = manifests[0]
    # Payloads live in five transitive data crates. Verify the complete bundle
    # without depending on Maturin's directory layout for path dependencies.
    data_manifests = [
        (path, json.loads(path.read_text()))
        for path in root.rglob("model-bundle.json")
        if path != manifest
    ]
    payload_roots = []
    for name in (
        "insta360-rs-data-core",
        "insta360-rs-data-enhancement",
        "insta360-rs-data-underwater-model-a",
        "insta360-rs-data-underwater-model-b",
        "insta360-rs-data-underwater-resources",
    ):
        matches = [
            path for path, bundle in data_manifests if bundle["bundle_id"] == name
        ]
        require(len(matches) == 1, f"Source distribution must contain {name}")
        dependency = matches[0].parent
        for filename in ("Cargo.toml", "src/lib.rs", "LICENSE.md", "NOTICE.md"):
            require(
                (dependency / filename).is_file(),
                f"Source distribution lacks {name}/{filename}",
            )
        payload_roots.append(dependency / "assets")
    original_parts = {}
    for descriptor in json.loads(manifest.read_text())["assets"]:
        payloads = [
            directory / descriptor["path"]
            for directory in payload_roots
            if (directory / descriptor["path"]).is_file()
        ]
        require(
            len(payloads) == 1 and payloads[0].is_file(),
            f"Source distribution must contain exactly one {descriptor['path']}",
        )
        payload = payloads[0].read_bytes()
        require(
            len(payload) == descriptor["byte_length"],
            f"Source distribution changed the size of {descriptor['path']}",
        )
        require(
            hashlib.sha256(payload).hexdigest() == descriptor["sha256"],
            f"Source distribution changed {descriptor['path']}",
        )
        provenance = descriptor["provenance"]
        normalization = provenance["normalization"]
        if normalization == "identity":
            require(
                descriptor["sha256"] == provenance["source_sha256"],
                "Original asset source hash changed",
            )
        else:
            match = re.fullmatch(
                r"byte-preserving slice \[(\d+), (\d+)\); concatenate part0 then part1 to reconstruct original",
                normalization,
            )
            require(match is not None, "Unrecognized asset normalization")
            start, end = map(int, match.groups())
            require(
                end - start == len(payload),
                "Asset chunk size disagrees with its declared range",
            )
            key = (provenance["source_path"], provenance["source_sha256"])
            original_parts.setdefault(key, []).append((start, end, payload))
    verify_original_parts(original_parts)
    # Also compare asset descriptors and implementation with this checkout.
    # Building the archive proves all transitive include_bytes! paths resolve.
    asset_root = manifest.parent
    original = PACKAGE.parent / "src-rust" / "assets"
    if original.exists():
        for source in original.rglob("*"):
            if source.is_file() and source.name not in {".DS_Store", ".gitattributes"}:
                packaged = asset_root / source.relative_to(original)
                require(packaged.is_file(), f"Source distribution lacks {source.name}")
                require(
                    hashlib.sha256(packaged.read_bytes()).digest()
                    == hashlib.sha256(source.read_bytes()).digest(),
                    f"Source distribution changed {source.name}",
                )
    return root


def verify_original_parts(groups):
    """Reconstruct original vendor identities from strictly contiguous byte slices."""
    for (_, expected_hash), parts in groups.items():
        digest = hashlib.sha256()
        end = 0
        for start, next_end, payload in sorted(parts):
            require(
                start == end and next_end - start == len(payload),
                "Asset chunks overlap, have a gap, or have an invalid length",
            )
            digest.update(payload)
            end = next_end
        require(
            digest.hexdigest() == expected_hash, "Reassembled vendor asset hash changed"
        )


def test_installation(wheel, directory, tests, interpreters):
    check_wheel(wheel)
    for index, interpreter in enumerate(interpreters):
        environment = directory / f"venv-{index}"
        run(interpreter, "-m", "venv", environment)
        python = environment / (
            "Scripts/python.exe" if os.name == "nt" else "bin/python"
        )
        run(
            python,
            "-m",
            "pip",
            "install",
            "--disable-pip-version-check",
            "--no-deps",
            wheel,
        )
        # -I disables cwd/PYTHONPATH and user-site imports. Verify the extension
        # actually comes from the new environment before exercising the API.
        run(
            python,
            "-I",
            "-c",
            "from pathlib import Path; import sys, insta360_rs; "
            "from insta360_rs import _native; "
            "assert Path(_native.__file__).is_relative_to(Path(sys.prefix)); "
            "assert {'libx265', 'libkvazaar'} & set(insta360_rs.capabilities().hevc_encoders), "
            "'Full validation requires a native software HEVC encoder'; "
            "print('Installed extension:', _native.__file__)",
            cwd=directory,
        )
        run(
            python,
            "-I",
            "-X",
            "faulthandler",
            tests.parent / "scripts" / "run-tests.py",
            cwd=directory,
            timeout=300,
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--python",
        action="append",
        dest="interpreters",
        help="Interpreter to test (repeat for an ABI matrix); defaults to this Python",
    )
    args = parser.parse_args()
    for command in ["cargo", "ffmpeg", "ffprobe"]:
        require(
            shutil.which(command),
            f"{command} is required for complete build/FFI validation",
        )
    # Keep compilation reusable without retaining source archives or environments.
    env = os.environ.copy()
    env.setdefault("CARGO_TARGET_DIR", str(PACKAGE.parent / "target" / "python-tests"))
    interpreters = [
        str(Path(p).resolve()) if Path(p).exists() else shutil.which(p)
        for p in args.interpreters or [sys.executable]
    ]
    require(all(interpreters), "A requested Python interpreter was not found")
    with tempfile.TemporaryDirectory(prefix="insta360-python-build-") as temporary:
        root = Path(temporary)
        artifacts = root / "artifacts"
        package = stage_source(PACKAGE.parent, root / "checkout")
        run(
            sys.executable,
            "-m",
            "build",
            "--sdist",
            "--wheel",
            "--outdir",
            artifacts,
            package,
            env=env,
        )
        wheel = next(artifacts.glob("*.whl"))
        source = check_sdist(next(artifacts.glob("*.tar.gz")), root / "source")
        test_installation(wheel, root, package / "tests", interpreters)
        rebuilt = root / "rebuilt"
        run(
            sys.executable,
            "-m",
            "build",
            "--wheel",
            "--outdir",
            rebuilt,
            source,
            env=env,
        )
        # Use the tests shipped in the sdist as well as its rebuilt native wheel.
        source_tests = next(source.rglob("test_public_api.py")).parent
        second = root / "source-installation"
        second.mkdir()
        test_installation(
            next(rebuilt.glob("*.whl")), second, source_tests, interpreters
        )
    print("Wheel, sdist, rebuilt wheel, and isolated native API suites passed.")


if __name__ == "__main__":
    main()
