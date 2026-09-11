"""Keep the generated catalog comparable after Windows-style Git checkouts."""

from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
CATALOG = Path("docs/housing-catalog.md")


class CatalogCheckoutTests(unittest.TestCase):
    def test_autocrlf_checkout_preserves_generated_catalog(self):
        with tempfile.TemporaryDirectory(prefix="insta360-catalog-checkout-") as temporary:
            checkout = Path(temporary)

            def git(*arguments):
                subprocess.run(
                    ["git", *arguments], cwd=checkout, check=True, capture_output=True
                )

            git("init", "--quiet")
            for relative in (Path(".gitattributes"), Path("docs/.gitattributes")):
                source = ROOT / relative
                if source.exists():
                    target = checkout / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(source, target)

            # The generator emits LF, even when this test runs on Windows.
            expected = (ROOT / CATALOG).read_bytes().replace(b"\r\n", b"\n")
            target = checkout / CATALOG
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(expected)
            git("-c", "core.autocrlf=false", "add", "--all")
            target.unlink()
            git("-c", "core.autocrlf=true", "checkout-index", "--all", "--force")

            self.assertEqual(target.read_bytes(), expected)


if __name__ == "__main__":
    unittest.main()
