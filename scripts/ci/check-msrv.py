"""Check the declared Rust floor without changing the main build toolchain or environment."""

import argparse
from pathlib import Path
import re
import subprocess
import sys
import tomllib


def declared_toolchain(root: Path) -> str:
    with (root / "Cargo.toml").open("rb") as source:
        version = tomllib.load(source)["workspace"]["package"]["rust-version"]
    if not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+(?:\.[0-9]+)?", version):
        raise ValueError("workspace.package.rust-version must be a numeric Rust release")
    return version + ".0" if version.count(".") == 1 else version


def check(root: Path) -> None:
    root = root.resolve()
    toolchain = declared_toolchain(root)
    print(f"Checking declared Rust minimum {toolchain} with all library features", flush=True)
    subprocess.run(
        ["rustup", "toolchain", "install", toolchain, "--profile", "minimal", "--no-self-update"],
        cwd=root, check=True,
    )
    # Explicit rustup run leaves the pinned primary toolchain and all caller
    # native paths, flags, profiles, and target directories unchanged. Default
    # workspace members include every published crate, excluding the extension.
    subprocess.run(
        ["rustup", "run", toolchain, "cargo", "check", "--locked", "--all-targets", "--all-features"],
        cwd=root, check=True,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    check(parser.parse_args().root)


if __name__ == "__main__":
    try:
        main()
    except (KeyError, OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
