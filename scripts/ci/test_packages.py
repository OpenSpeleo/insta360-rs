"""Tests for the strict archive size and extracted-source package checks."""

import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import tomllib
import unittest
from unittest import mock

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

    def test_every_publishable_workspace_member_has_extracted_archive_verification(self):
        root = Path(__file__).resolve().parents[2]
        manifest = tomllib.loads((root / "Cargo.toml").read_text())
        published = set()
        for relative in manifest["workspace"]["members"]:
            package = tomllib.loads((root / relative / "Cargo.toml").read_text())["package"]
            if package.get("publish") is not False:
                published.add(relative)
        self.assertEqual(set(packages.PACKAGE_PATHS), published)
        self.assertEqual(len(packages.PACKAGE_PATHS), len(published))
        self.assertEqual(packages.PACKAGE_PATHS[-1], ".", "data must be verified before the library")

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

    def test_all_feature_archives_run_isolated_references_with_extracted_dependencies(self):
        (self.root / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "0.1.0"\n[package]\nname = "example"\n'
        )
        (self.root / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "stable"\n')
        (self.root / "data").mkdir()
        (self.root / "data/Cargo.toml").write_text('[package]\nname = "example-data"\n')
        target = self.root / "target"
        (target / "package").mkdir(parents=True)
        for name in ("example-data", "example"):
            archive = target / "package" / f"{name}-0.1.0.crate"
            with tarfile.open(archive, "w:gz") as package:
                content = f'[package]\nname = "{name}"\n'.encode()
                member = tarfile.TarInfo(f"{name}-0.1.0/Cargo.toml")
                member.size = len(content)
                package.addfile(member, io.BytesIO(content))

        for all_features in (False, True):
            with (
                self.subTest(all_features=all_features),
                mock.patch.object(packages, "PACKAGE_PATHS", ("data", ".")),
                mock.patch.object(packages, "run") as run,
                mock.patch.dict(packages.os.environ, {"CARGO_TARGET_DIR": str(target)}),
            ):
                packages.verify(self.root, self.root / "dist", False, all_features)
                references = [
                    call.args for call in run.call_args_list if "--ignored" in call.args[0]
                ]
                self.assertEqual(len(references), 2 if all_features else 0)
                for (command, cwd, environment), reference in zip(
                    references, packages.MODEL_REFERENCE_TESTS
                ):
                    self.assertEqual(command[:4], ["cargo", "test", "--lib", reference])
                    self.assertEqual(
                        command[-5:],
                        ["--", "--ignored", "--exact", "--nocapture", "--test-threads=1"],
                    )
                    self.assertIn("--locked", command)
                    self.assertIn("--all-features", command)
                    self.assertEqual(cwd.name, "example-0.1.0")
                    self.assertNotEqual(cwd.parent, self.root)
                    self.assertEqual(
                        command[command.index("--manifest-path") + 1], str(cwd / "Cargo.toml")
                    )
                    patch = command[command.index("--config") + 1]
                    self.assertIn(str(cwd.parent / "example-data-0.1.0"), patch)
                    self.assertNotIn(str(self.root.resolve() / "data"), patch)
                    self.assertEqual(environment["CARGO_TARGET_DIR"], str(target.resolve()))


if __name__ == "__main__":
    unittest.main()
