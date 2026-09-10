"""Relocate project notices in a staging tree to avoid workspace sdist collisions."""

import argparse
import json
import re
import shutil
from pathlib import Path


def stage_project_licenses(package, *, runtime=False):
    metadata_path = package / "pyproject.toml"
    text = metadata_path.read_text()
    project = re.search(r"(?ms)^\[project\][ \t]*\n(.*?)(?=^\[|\Z)", text)
    if project is None:
        raise RuntimeError("Expected a project metadata table")
    licenses = ["python/insta360_rs/_licenses/project/*.md"]
    if runtime:
        licenses.append("python/insta360_rs/_licenses/*.txt")
    replacement = "license-files = " + json.dumps(licenses)
    body, count = re.subn(
        r"(?m)^license-files\s*=\s*\[[^\]]*\]",
        replacement,
        project.group(1),
        count=1,
    )
    if count == 0:
        body, count = re.subn(
            r"(?m)^license\s*=.*$",
            lambda match: match.group(0) + "\n" + replacement,
            body,
            count=1,
        )
    if count != 1:
        raise RuntimeError("Expected a project license declaration")
    notices = package / "python" / "insta360_rs" / "_licenses" / "project"
    notices.mkdir(parents=True, exist_ok=True)
    for name in ("LICENSE.md", "NOTICE.md"):
        shutil.copy2(package / name, notices / name)
    metadata_path.write_text(text[: project.start(1)] + body + text[project.end(1) :])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("--runtime", action="store_true")
    args = parser.parse_args()
    stage_project_licenses(args.package, runtime=args.runtime)


if __name__ == "__main__":
    main()
