"""Tests for the strict archive size and extracted-source package checks."""

import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "check_packages", Path(__file__).with_name("check-packages.py")
)
packages = importlib.util.module_from_spec(spec)
spec.loader.exec_module(packages)


class PackageChecksTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def test_archive_must_be_strictly_below_ten_million_bytes(self):
        archive = self.root / "example.crate"
        for size in [9_999_999, 10_000_000, 10_000_001]:
            with self.subTest(size=size):
                with archive.open("wb") as target:
                    target.truncate(size)
                if size < 10_000_000:
                    self.assertEqual(packages.check_size(archive), size)
                else:
                    with self.assertRaisesRegex(ValueError, "must be below"):
                        packages.check_size(archive)

    def archive(self, members):
        path = self.root / "example-0.1.0.crate"
        with tarfile.open(path, "w:gz") as archive:
            for name, content in members:
                member = tarfile.TarInfo(name)
                member.size = len(content)
                archive.addfile(member, io.BytesIO(content))
        return path

    def test_extracts_packaged_manifest_and_payload(self):
        archive = self.archive([
            ("example-0.1.0/Cargo.toml", b'[package]\nname = "example"\n'),
            ("example-0.1.0/assets/model.ins", b"original payload"),
        ])
        extracted = packages.extract_package(archive, self.root / "extracted", "example", "0.1.0")
        self.assertEqual((extracted / "assets/model.ins").read_bytes(), b"original payload")

    def test_rejects_another_package_root(self):
        archive = self.archive([("other-0.1.0/Cargo.toml", b"manifest")])
        with self.assertRaisesRegex(ValueError, "unexpected archive layout"):
            packages.extract_package(archive, self.root / "extracted", "example", "0.1.0")

    def test_rejects_missing_manifest(self):
        archive = self.archive([("example-0.1.0/assets/model.ins", b"payload")])
        with self.assertRaisesRegex(ValueError, "missing packaged Cargo.toml"):
            packages.extract_package(archive, self.root / "extracted", "example", "0.1.0")


if __name__ == "__main__":
    unittest.main()
