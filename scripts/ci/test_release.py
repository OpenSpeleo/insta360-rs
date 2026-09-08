"""Tests for the version gate that precedes registry publication."""

import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "check_release", Path(__file__).with_name("check-release.py")
)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseVersionTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "src-python").mkdir()
        (self.root / "data/core").mkdir(parents=True)
        (self.root / "data/enhancement").mkdir()
        self.manifests = [
            "Cargo.toml", "data/core/Cargo.toml", "data/enhancement/Cargo.toml",
            "src-python/Cargo.toml",
        ]
        for relative in self.manifests:
            (self.root / relative).write_text('[package]\nversion.workspace = true\n')
        with (self.root / "Cargo.toml").open("a") as manifest:
            manifest.write('[workspace.package]\nversion = "1.2.3"\n[workspace.dependencies]\n')
            for name, path in {"insta360-rs": ".", **release.DATA_CRATES}.items():
                manifest.write(f'{name} = {{ version = "=1.2.3", path = "{path}" }}\n')
            manifest.write('[dependencies]\n')
            for name in release.DATA_CRATES:
                manifest.write(f'{name}.workspace = true\n')
        with (self.root / "src-python/Cargo.toml").open("a") as manifest:
            manifest.write('publish = false\n[dependencies]\ninsta360-rs.workspace = true\n')
        (self.root / "src-python/pyproject.toml").write_text(
            '[project]\ndynamic = ["version", "authors"]\n'
        )

    def test_matching_versions_are_accepted(self):
        self.assertEqual(release.validate("v1.2.3", self.root), "1.2.3")

    def test_workspace_version_must_match_tag(self):
        manifest = self.root / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace('version = "1.2.3"', 'version = "1.2.4"'))
        with self.assertRaisesRegex(ValueError, "found 1.2.4"):
            release.validate("v1.2.3", self.root)

    def test_each_manifest_must_inherit_version(self):
        for relative in self.manifests:
            with self.subTest(manifest=relative):
                manifest = self.root / relative
                original = manifest.read_text()
                manifest.write_text(original.replace('version.workspace = true', 'version = "1.2.3"'))
                with self.assertRaisesRegex(ValueError, "must inherit workspace.package.version"):
                    release.validate("v1.2.3", self.root)
                manifest.write_text(original)

    def test_python_version_must_be_dynamic(self):
        manifest = self.root / "src-python/pyproject.toml"
        for content in ['version = "1.2.3"', 'dynamic = ["authors"]', 'version = "1.2.3"\ndynamic = ["version"]']:
            with self.subTest(content=content):
                manifest.write_text('[project]\n' + content + '\n')
                with self.assertRaisesRegex(ValueError, "derive its version dynamically"):
                    release.validate("v1.2.3", self.root)

    def test_python_crate_cannot_be_published_to_crates_io(self):
        manifest = self.root / "src-python/Cargo.toml"
        manifest.write_text(manifest.read_text().replace('publish = false', 'publish = true'))
        with self.assertRaisesRegex(ValueError, "publish = false"):
            release.validate("v1.2.3", self.root)

    def test_each_data_dependency_must_be_exactly_pinned(self):
        manifest = self.root / "Cargo.toml"
        original = manifest.read_text()
        for name in ["insta360-rs", *release.DATA_CRATES]:
            for version in ["1.2.3", "^1.2.3", "=1.2.4"]:
                with self.subTest(dependency=name, version=version):
                    manifest.write_text(original.replace(
                        f'{name} = {{ version = "=1.2.3"',
                        f'{name} = {{ version = "{version}"',
                    ))
                    with self.assertRaisesRegex(ValueError, "must pin"):
                        release.validate("v1.2.3", self.root)
        manifest.write_text(original)

    def test_each_data_dependency_must_use_the_expected_local_path(self):
        manifest = self.root / "Cargo.toml"
        original = manifest.read_text()
        for name, path in {"insta360-rs": ".", **release.DATA_CRATES}.items():
            with self.subTest(dependency=name):
                manifest.write_text(original.replace(f'path = "{path}"', 'path = "elsewhere"'))
                with self.assertRaisesRegex(ValueError, "must resolve"):
                    release.validate("v1.2.3", self.root)
        manifest.write_text(original)

    def test_dependencies_cannot_override_workspace_pins(self):
        for relative, names in [("Cargo.toml", release.DATA_CRATES), ("src-python/Cargo.toml", ["insta360-rs"])]:
            manifest = self.root / relative
            original = manifest.read_text()
            for name in names:
                with self.subTest(manifest=relative, dependency=name):
                    manifest.write_text(original.replace(
                        f'{name}.workspace = true', f'{name} = {{ version = "1.2.3" }}'
                    ))
                    with self.assertRaisesRegex(ValueError, "must inherit workspace dependency"):
                        release.validate("v1.2.3", self.root)
            manifest.write_text(original)

    def test_invalid_tags_never_reach_publication(self):
        for tag in ["", "1.2.3", "v1.2", "v01.2.3", "v1.2.3rc1", "v1.2.3-rc.1", "v1.2.3+build"]:
            with self.subTest(tag=tag):
                with self.assertRaisesRegex(ValueError, "vMAJOR.MINOR.PATCH"):
                    release.validate(tag, self.root)


if __name__ == "__main__":
    unittest.main()
