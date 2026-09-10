"""Regression coverage for isolated source snapshots and project notice staging."""

import importlib.util
from pathlib import Path
import tempfile
import tomllib
import unittest


SCRIPTS = Path(__file__).resolve().parents[2] / "src-python" / "scripts"


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


licenses = load_script("stage-project-licenses")
runner = load_script("test")


class PythonPackagingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def package(self, name, declaration=True):
        package = self.root / name / "src-python"
        package.mkdir(parents=True)
        text = '[project]\nname = "example"\nlicense = "Apache-2.0"\n'
        if declaration:
            text += 'license-files = [\n  "LICENSE.md",\n  "NOTICE.md",\n]\n'
        text += '\n[tool.other]\nlicense-files = ["unrelated.txt"]\n'
        (package / "pyproject.toml").write_text(text)
        for name in ("LICENSE.md", "NOTICE.md"):
            (package / name).write_bytes(b"original project bytes\r\n" + name.encode())
            (package.parent / name).write_bytes(b"different workspace notice\n")
        return package

    def test_notice_bytes_and_unrelated_metadata_survive_all_staging_modes(self):
        for declaration in (False, True):
            for runtime in (False, True):
                with self.subTest(declaration=declaration, runtime=runtime):
                    package = self.package(f"{declaration}-{runtime}", declaration)
                    licenses.stage_project_licenses(package, runtime=runtime)
                    metadata = tomllib.loads((package / "pyproject.toml").read_text())
                    patterns = ["python/insta360_rs/_licenses/project/*.md"]
                    if runtime:
                        patterns.append("python/insta360_rs/_licenses/*.txt")
                    self.assertEqual(metadata["project"]["license-files"], patterns)
                    self.assertEqual(metadata["project"]["license"], "Apache-2.0")
                    self.assertEqual(
                        metadata["tool"]["other"]["license-files"], ["unrelated.txt"]
                    )
                    for name in ("LICENSE.md", "NOTICE.md"):
                        copies = list(package.glob(patterns[0].replace("*.md", name)))
                        self.assertEqual(len(copies), 1)
                        self.assertEqual(copies[0].read_bytes(), (package / name).read_bytes())
                        self.assertEqual(
                            (package.parent / name).read_bytes(),
                            b"different workspace notice\n",
                        )

    def test_snapshot_preserves_checkout_and_omits_development_artifacts(self):
        package = self.package("checkout")
        workspace = package.parent
        original = (package / "pyproject.toml").read_bytes()
        for relative in (
            "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "data/core/assets/model.bin",
            "data/core/assets/_native.so",
        ):
            path = workspace / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"original workspace input")
        for relative in (
            ".git/config", "target/compiled", ".cache/download", "dist/wheel",
            "src-python/.venv/python", "src-python/tests/__pycache__/test.pyc",
            "src-python/.DS_Store",
            "src-python/python/insta360_rs/_native.abi3.so",
            "src-python/python/insta360_rs/_native.pyd",
        ):
            path = workspace / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"development artifact")
        staged = runner.stage_source(workspace, self.root / "staged")
        self.assertEqual((package / "pyproject.toml").read_bytes(), original)
        self.assertFalse((package / "python" / "insta360_rs" / "_licenses").exists())
        self.assertEqual(
            (staged.parent / "data/core/assets/model.bin").read_bytes(),
            b"original workspace input",
        )
        self.assertEqual(
            (staged.parent / "data/core/assets/_native.so").read_bytes(),
            b"original workspace input",
        )
        self.assertFalse(any(
            path.read_bytes() == b"development artifact"
            for path in staged.parent.rglob("*") if path.is_file()
        ))
        self.assertTrue((staged / "python/insta360_rs/_licenses/project/LICENSE.md").is_file())

    def test_missing_project_license_fails_without_rewriting_metadata(self):
        package = self.package("invalid")
        original = '[project]\nname = "example"\n'
        (package / "pyproject.toml").write_text(original)
        with self.assertRaisesRegex(RuntimeError, "license declaration"):
            licenses.stage_project_licenses(package)
        self.assertEqual((package / "pyproject.toml").read_text(), original)


if __name__ == "__main__":
    unittest.main()
