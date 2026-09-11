"""Verify that Windows-style Git checkouts preserve licensed payload bytes."""

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class AssetCheckoutTests(unittest.TestCase):
    def test_autocrlf_checkout_preserves_every_manifest_payload(self):
        with tempfile.TemporaryDirectory(prefix="insta360-asset-checkout-") as temporary:
            checkout = Path(temporary)

            def git(*arguments):
                subprocess.run(
                    ["git", *arguments], cwd=checkout, check=True, capture_output=True
                )

            git("init", "--quiet")
            attributes = list((ROOT / "data").rglob(".gitattributes"))
            if (ROOT / ".gitattributes").exists():
                attributes.append(ROOT / ".gitattributes")
            for source in attributes:
                target = checkout / source.relative_to(ROOT)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)

            payloads = []
            for manifest_path in sorted((ROOT / "data").glob("*/model-bundle.json")):
                manifest = json.loads(manifest_path.read_text())
                for asset in manifest["assets"]:
                    source = manifest_path.parent / "assets" / asset["path"]
                    target = checkout / source.relative_to(ROOT)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(source, target)
                    payloads.append((target, asset))
            self.assertTrue(payloads)

            # Store the source bytes, then apply the same conversion used by
            # Windows checkouts; ordinary Linux/macOS integrity tests miss it.
            git("-c", "core.autocrlf=false", "add", "--all")
            for target, _ in payloads:
                target.unlink()
            git("-c", "core.autocrlf=true", "checkout-index", "--all", "--force")

            for target, asset in payloads:
                with self.subTest(asset=asset["id"]):
                    content = target.read_bytes()
                    self.assertEqual(len(content), asset["byte_length"])
                    self.assertEqual(hashlib.sha256(content).hexdigest(), asset["sha256"])


if __name__ == "__main__":
    unittest.main()
