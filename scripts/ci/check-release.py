"""Validate release versions, matching tags, and the originating successful CI run."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib

DATA_CRATES = {
    "insta360-rs-data-core": "data/core",
    "insta360-rs-data-enhancement": "data/enhancement",
    "insta360-rs-data-underwater-model-a": "data/underwater-model-a",
    "insta360-rs-data-underwater-model-b": "data/underwater-model-b",
    "insta360-rs-data-underwater-resources": "data/underwater-resources",
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
        *(f"{path}/Cargo.toml" for path in DATA_CRATES.values()),
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


def git_head(root: Path) -> str:
    return subprocess.run(
        ["git", "rev-parse", "--verify", "HEAD^{commit}"],
        cwd=root, check=True, capture_output=True, text=True, timeout=10,
    ).stdout.strip()


def matching_tag(root: Path) -> str | None:
    """Select only the workspace version's tag when it names the checked-out commit."""
    with (root / "Cargo.toml").open("rb") as source:
        version = tomllib.load(source)["workspace"]["package"]["version"]
    tag = f"v{version}"
    validate(tag, root)
    head = git_head(root)
    result = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}^{{commit}}"],
        cwd=root, capture_output=True, text=True, timeout=10,
    )
    if result.returncode == 1:
        return None
    result.check_returncode()
    return tag if result.stdout.strip() == head else None


def positive_decimal(value: str) -> int:
    if not re.fullmatch(r"[1-9][0-9]*", value):
        raise argparse.ArgumentTypeError("must be a positive decimal integer")
    return int(value)


def ci_run_succeeded(
    run: dict, repository: str, run_id: int, run_attempt: int, head: str,
) -> bool:
    """Fail closed on mismatched identity, a newer attempt, or terminal failure."""
    if not isinstance(run, dict):
        raise ValueError("GitHub returned an invalid CI run")
    if type(run.get("id")) is not int or run["id"] != run_id:
        raise ValueError("GitHub returned a different CI run ID")
    if type(run.get("run_attempt")) is not int or run["run_attempt"] != run_attempt:
        raise ValueError("The latest CI run attempt does not match the dispatched attempt")
    for field in ("repository", "head_repository"):
        origin = run.get(field)
        if not isinstance(origin, dict) or origin.get("full_name") != repository:
            raise ValueError(f"CI {field} must match {repository}")
    if run.get("path") != ".github/workflows/ci.yml":
        raise ValueError("The source run must use .github/workflows/ci.yml")
    event = run.get("event")
    if event not in {"push", "workflow_dispatch"}:
        raise ValueError("The source CI run must be a push or workflow_dispatch")
    if event == "push" and run.get("head_branch") != "master":
        raise ValueError("Push-triggered source CI must run on master")
    if run.get("head_sha") != head:
        raise ValueError("The release checkout does not match the source CI commit")
    status = run.get("status")
    conclusion = run.get("conclusion")
    if status == "completed":
        if conclusion != "success":
            raise ValueError(f"The source CI run did not succeed: {conclusion}")
        return True
    if status not in {"queued", "in_progress", "waiting", "pending", "requested"}:
        raise ValueError(f"GitHub returned an unexpected CI status: {status}")
    if conclusion is not None:
        raise ValueError(f"The source CI run has an unexpected conclusion: {conclusion}")
    return False


def read_ci_run(repository: str, run_id: int, timeout: float) -> dict:
    result = subprocess.run(
        ["gh", "api", "--method", "GET", f"repos/{repository}/actions/runs/{run_id}"],
        check=True, capture_output=True, text=True, timeout=timeout,
    )
    return json.loads(result.stdout)


def wait_for_ci(
    repository: str, run_id: int, run_attempt: int, head: str,
    *, timeout: float = 300, interval: float = 5,
) -> None:
    """Wait for the dispatching CI job to finish without accepting a rerun."""
    deadline = time.monotonic() + timeout
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise ValueError("Timed out waiting for the source CI run to succeed")
        run = read_ci_run(repository, run_id, min(30, remaining))
        if ci_run_succeeded(run, repository, run_id, run_attempt, head):
            return
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise ValueError("Timed out waiting for the source CI run to succeed")
        print(
            f"Waiting for CI run {run_id}, attempt {run_attempt}, to finish...",
            file=sys.stderr, flush=True,
        )
        time.sleep(min(interval, remaining))


def verify_ci(root: Path, run_id: int, run_attempt: int) -> str:
    if os.environ.get("GITHUB_REF_TYPE") != "tag":
        raise ValueError("Release dispatch must use a tag ref")
    version = validate(os.environ.get("GITHUB_REF_NAME", ""), root)
    repository = os.environ.get("GITHUB_REPOSITORY", "")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("GITHUB_REPOSITORY must identify the release repository")
    wait_for_ci(repository, run_id, run_attempt, git_head(root))
    return version


def main(argv: list[str] | None = None, root: Path | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag", nargs="?", help="defaults to GITHUB_REF_NAME")
    parser.add_argument("--matching-tag", action="store_true")
    parser.add_argument("--ci-run-id", type=positive_decimal)
    parser.add_argument("--ci-run-attempt", type=positive_decimal)
    args = parser.parse_args(argv)
    root = root or Path(__file__).resolve().parents[2]
    ci_requested = args.ci_run_id is not None or args.ci_run_attempt is not None
    if ci_requested and (args.ci_run_id is None or args.ci_run_attempt is None):
        parser.error("--ci-run-id and --ci-run-attempt must be supplied together")
    if args.matching_tag and (args.tag is not None or ci_requested):
        parser.error("--matching-tag cannot be combined with a tag or CI run options")
    if ci_requested and args.tag is not None:
        parser.error("CI run verification takes the release tag from GITHUB_REF_NAME")
    if args.matching_tag:
        tag = matching_tag(root)
        if tag is not None:
            print(tag)
    elif ci_requested:
        version = verify_ci(root, args.ci_run_id, args.ci_run_attempt)
        print(f"Validated release {version} against CI run {args.ci_run_id}, attempt {args.ci_run_attempt}")
    else:
        tag = args.tag if args.tag is not None else os.environ.get("GITHUB_REF_NAME", "")
        version = validate(tag, root)
        print(f"Validated release {version} for crates.io and PyPI")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
