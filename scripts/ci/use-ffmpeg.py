"""Relocate the prepared Linux FFmpeg libraries and export its compiler/runtime paths."""

import os
from pathlib import Path


def configure(root: Path) -> dict[str, str]:
    cache = (root / ".cache" / "wheel").resolve()
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
    return {
        "PKG_CONFIG_PATH": str(pkg_config),
        "LD_LIBRARY_PATH": str(prefix / "lib") + (
            ":" + os.environ["LD_LIBRARY_PATH"] if os.environ.get("LD_LIBRARY_PATH") else ""
        ),
    }


if __name__ == "__main__":
    environment = configure(Path(__file__).resolve().parents[2])
    with open(os.environ["GITHUB_ENV"], "a") as destination:
        for key, value in environment.items():
            print(f"{key}={value}", file=destination)
    print("Prepared FFmpeg libraries configured for this checkout")
