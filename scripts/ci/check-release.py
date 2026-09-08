"""Require matching release versions and exact bundled-data dependency pins."""

import os
from pathlib import Path
import re
import sys
import tomllib

DATA_CRATES = {
    "insta360-rs-data-core": "data/core",
    "insta360-rs-data-enhancement": "data/enhancement",
}


def validate(tag: str, root: Path) -> str:
    if not re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag):
        raise ValueError("Release tags must use vMAJOR.MINOR.PATCH (for example v0.1.0)")
    version = tag[1:]
    with (root / "Cargo.toml").open("rb") as source:
        manifest = tomllib.load(source)
    actual = manifest["workspace"]["package"]["version"]
    if actual != version:
        raise ValueError(f"{tag} requires workspace version {version}, found {actual}")
    for relative in [
        "Cargo.toml",
        "data/core/Cargo.toml",
        "data/enhancement/Cargo.toml",
        "src-python/Cargo.toml",
    ]:
        with (root / relative).open("rb") as source:
            package = tomllib.load(source)["package"]
        if package.get("version") != {"workspace": True}:
            raise ValueError(f"{relative} must inherit workspace.package.version")
        if relative == "src-python/Cargo.toml" and package.get("publish") is not False:
            raise ValueError("src-python/Cargo.toml must set publish = false")
    with (root / "src-python/pyproject.toml").open("rb") as source:
        project = tomllib.load(source)["project"]
    if "version" in project or "version" not in project.get("dynamic", []):
        raise ValueError("src-python/pyproject.toml must derive its version dynamically from Cargo")
    dependencies = manifest["workspace"]["dependencies"]
    for name, path in {"insta360-rs": ".", **DATA_CRATES}.items():
        dependency = dependencies[name]
        if not isinstance(dependency, dict) or dependency.get("version") != f"={version}":
            raise ValueError(f"Cargo.toml must pin {name} to exactly ={version}")
        if dependency.get("path") != path:
            raise ValueError(f"Cargo.toml must resolve {name} from {path}")
    for name in DATA_CRATES:
        if manifest["dependencies"].get(name) != {"workspace": True}:
            raise ValueError(f"Cargo.toml must inherit workspace dependency {name}")
    with (root / "src-python/Cargo.toml").open("rb") as source:
        dependency = tomllib.load(source)["dependencies"]["insta360-rs"]
    if dependency.get("workspace") is not True or {"version", "path"} & dependency.keys():
        raise ValueError("src-python/Cargo.toml must inherit workspace dependency insta360-rs")
    return version


if __name__ == "__main__":
    try:
        release = validate(
            sys.argv[1] if len(sys.argv) > 1 else os.environ.get("GITHUB_REF_NAME", ""),
            Path(__file__).resolve().parents[2],
        )
    except (ValueError, KeyError) as error:
        sys.exit(str(error))
    print(f"Validated release {release} for crates.io and PyPI")
