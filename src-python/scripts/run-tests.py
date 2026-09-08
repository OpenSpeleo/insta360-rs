"""Run the native Python suite with mandatory media-test prerequisites.

Install the extension into the active interpreter first. CI and manual test
runs use this entry point so missing codecs/tools fail visibly.
"""

import importlib.machinery
import shutil
import sys
import unittest
from pathlib import Path


def main():
    missing = [tool for tool in ("ffmpeg", "ffprobe") if not shutil.which(tool)]
    if missing:
        raise RuntimeError(
            "Python media tests require PATH tools: " + ", ".join(missing)
        )

    try:
        import insta360_rs
        from insta360_rs import _native
    except ImportError as error:
        raise RuntimeError(
            "Install or rebuild insta360-rs in this Python environment before testing"
        ) from error
    if not any(
        str(_native.__file__).endswith(suffix)
        for suffix in importlib.machinery.EXTENSION_SUFFIXES
    ):
        raise RuntimeError("Python tests require the compiled native extension")
    if not {"libx265", "libkvazaar"} & set(insta360_rs.capabilities().hevc_encoders):
        raise RuntimeError(
            "Python video tests require software HEVC in the linked FFmpeg libraries "
            "(libx265 or libkvazaar)"
        )
    print(f"Testing installed extension: {_native.__file__}", flush=True)

    tests = Path(__file__).resolve().parents[1] / "tests"
    suite = unittest.defaultTestLoader.discover(str(tests))
    if suite.countTestCases() == 0:
        raise RuntimeError(f"No Python tests discovered in {tests}")
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
