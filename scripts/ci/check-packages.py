"""Verify all published archives, including before their first release.

The data archives are packaged normally. Local crates.io patches allow Cargo to
resolve the main archive's unpublished data dependencies while packaging. Its
extracted sources are then built and tested with patches pointing only to the
*extracted data archives*. This checks the actual shipped sources without access
to source-tree payloads. Release publication uses Cargo's native multi-package
verification in the repository workspace, finishing every build before any upload.
"""

import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib


MAX_CRATE_BYTES = 10_000_000
PACKAGE_PATHS = ("data/core", "data/enhancement", "data/underwater-model-a", "data/underwater-model-b", "data/underwater-resources", ".")


def check_size(archive: Path) -> int:
    size = archive.stat().st_size
    if size >= MAX_CRATE_BYTES:
        raise ValueError(
            f"{archive.name}: {size:,} bytes must be below {MAX_CRATE_BYTES:,} bytes"
        )
    print(f"{archive.name}: {size:,} / {MAX_CRATE_BYTES:,} bytes", flush=True)
    return size


def extract_package(archive: Path, destination: Path, name: str, version: str) -> Path:
    prefix = f"{name}-{version}"
    with tarfile.open(archive, "r:gz") as package:
        members = package.getmembers()
        if not members or any(
            member.name.split("/", 1)[0] != prefix or not member.isfile()
            for member in members
        ):
            raise ValueError(f"{archive.name}: unexpected archive layout")
        package.extractall(destination, filter="data")
    root = destination / prefix
    if not (root / "Cargo.toml").is_file():
        raise ValueError(f"{archive.name}: missing packaged Cargo.toml")
    return root


def patch_arguments(packages: dict[str, Path]) -> list[str]:
    arguments = []
    for name, path in packages.items():
        arguments.extend([
            "--config",
            f"patch.crates-io.{name}.path={json.dumps(str(path))}",
        ])
    return arguments


def run(command: list[str], cwd: Path, environment: dict[str, str]) -> None:
    print(f"+ {shlex.join(command)}", flush=True)
    subprocess.run(command, cwd=cwd, env=environment, check=True)


def verify(root: Path, output: Path, allow_dirty: bool, all_features: bool) -> None:
    root = root.resolve()
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    with (root / "rust-toolchain.toml").open("rb") as source:
        environment.setdefault("RUSTUP_TOOLCHAIN", tomllib.load(source)["toolchain"]["channel"])
    target = Path(environment.get("CARGO_TARGET_DIR", root / "target"))
    if not target.is_absolute():
        target = root / target
    target = target.resolve()
    # An absolute shared target keeps extracted sources isolated from the
    # checkout while reusing the job's compiled dependencies.
    environment["CARGO_TARGET_DIR"] = str(target)
    with (root / "Cargo.toml").open("rb") as source:
        version = tomllib.load(source)["workspace"]["package"]["version"]
    run([sys.executable, str(root / "scripts/ci/check-release.py"), f"v{version}"], root, environment)

    with tempfile.TemporaryDirectory(prefix="insta360-packaged-") as temporary:
        extracted = Path(temporary)
        data_packages: dict[str, Path] = {}
        source_packages: dict[str, Path] = {}
        for relative in PACKAGE_PATHS:
            manifest = root / relative / "Cargo.toml"
            with manifest.open("rb") as source:
                package = tomllib.load(source)["package"]
            name = package["name"]
            is_main = relative == "."
            patches = patch_arguments(data_packages) if is_main else []
            command = ["cargo", "package", "--locked", "--manifest-path", str(manifest)]
            if allow_dirty:
                command.append("--allow-dirty")
            if is_main:
                # Explicit locked tests/builds below verify the extracted main
                # archive and all asset bytes against the staged dependencies.
                command.append("--no-verify")
            # Packaging starts from the checkout's path dependency graph. Use
            # those same paths for resolution to avoid adding unused patches
            # to its lockfile. Compilation below uses extracted archives only.
            packaging_patches = patch_arguments(source_packages) if is_main else []
            run(command + packaging_patches, root, environment)
            archive = target / "package" / f"{name}-{version}.crate"
            check_size(archive)
            shutil.copy2(archive, output / archive.name)
            packaged = extract_package(archive, extracted, name, version)
            features = ["--all-features"] if is_main and all_features else []
            common = ["--locked", "--manifest-path", str(packaged / "Cargo.toml")]
            for operation in [
                ["test", "--all-targets"],
                ["test", "--doc"],
                ["build", "--release", "--lib", "--bins", "--examples"],
            ]:
                run(["cargo"] + operation + common + features + patches, packaged, environment)
            if not is_main:
                data_packages[name] = packaged
                source_packages[name] = manifest.parent
    print(f"All archives passed size, extracted-source, and locked-build checks: {output}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--allow-dirty", action="store_true")
    parser.add_argument("--all-features", action="store_true")
    parser.add_argument("--check-size-only", nargs="+", type=Path, metavar="ARCHIVE")
    arguments = parser.parse_args()
    if arguments.check_size_only:
        for archive in arguments.check_size_only:
            check_size(archive)
        return
    verify(
        arguments.root,
        arguments.output_dir or arguments.root / "dist/crates",
        arguments.allow_dirty,
        arguments.all_features,
    )


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
