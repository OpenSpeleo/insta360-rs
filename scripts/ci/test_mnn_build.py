"""Verify source identity, native cache integrity, and bounded MNN extraction."""

import hashlib
import importlib.util
import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "build_mnn", Path(__file__).with_name("build-mnn.py")
)
mnn = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mnn)


class MnnBuildTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="mnn builder test ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def archive(self, entries):
        path = self.root / "source.tar.gz"
        with tarfile.open(path, "w:gz") as archive:
            for name, content in entries:
                entry = tarfile.TarInfo(name)
                if isinstance(content, bytes):
                    entry.size = len(content)
                    archive.addfile(entry, io.BytesIO(content))
                else:
                    entry.type = content[0]
                    entry.linkname = "../outside"
                    archive.addfile(entry)
        return path

    def test_unsafe_archive_paths_and_special_files_fail_before_extraction(self):
        for name, kind in (
            ("../escape", b"bad"),
            ("/absolute", b"bad"),
            ("fifo", (tarfile.FIFOTYPE,)),
            ("hard", (tarfile.LNKTYPE,)),
        ):
            with self.subTest(name=name):
                archive = self.archive([("safe", b"good"), (name, kind)])
                destination = self.root / "unpacked"
                with self.assertRaises(ValueError):
                    mnn.extract_source(archive, destination)
                self.assertFalse(destination.exists())

    def test_unneeded_links_are_ignored_without_following_them(self):
        archive = self.archive(
            [
                ("examples/link", (tarfile.SYMTYPE,)),
                ("include/source.hpp", b"original header"),
            ]
        )
        destination = self.root / "source"
        mnn.extract_source(archive, destination)
        self.assertFalse((destination / "examples/link").exists())
        self.assertEqual(
            (destination / "include/source.hpp").read_bytes(), b"original header"
        )

    def test_verified_build_cache_rejects_changed_native_bytes_and_configuration(self):
        source = f"MNN-{mnn.COMMIT}"
        archive = self.archive(
            [
                (f"{source}/include/MNN/Interpreter.hpp", b"header"),
                (f"{source}/LICENSE.txt", b"original MNN notice"),
                (f"{source}/3rd_party/half/LICENSE.txt", b"original half notice"),
                (
                    f"{source}/3rd_party/lic/flatbuffer_license",
                    b"original flatbuffers notice",
                ),
            ]
        )
        body = archive.read_bytes()
        calls = []

        def compile_source(args, *, check):
            self.assertTrue(check)
            calls.append(args)
            if "--build" in args:
                build = Path(args[args.index("--build") + 1])
                build.mkdir()
                (build / "libMNN.a").write_bytes(b"static CPU library")

        prefix = self.root / "prefix"
        with (
            patch.object(mnn, "SHA256", hashlib.sha256(body).hexdigest()),
            patch.object(
                mnn.urllib.request,
                "urlopen",
                side_effect=lambda *_args, **_kwargs: io.BytesIO(body),
            ),
            patch.object(mnn.subprocess, "run", side_effect=compile_source),
        ):
            mnn.build(prefix, 2)
            config = mnn.build_configuration()
            self.assertTrue(mnn.verify_prefix(prefix, config))
            mnn.build(prefix, 2)
            self.assertEqual(len(calls), 2)
            for flag in (
                "-DMNN_BUILD_SHARED_LIBS=OFF",
                "-DMNN_KLEIDIAI=OFF",
                "-DCMAKE_POSITION_INDEPENDENT_CODE=ON",
            ):
                self.assertIn(flag, calls[0])
            self.assertEqual(
                (prefix / "MNN-LICENSE.txt").read_bytes(), b"original MNN notice"
            )
            self.assertIn(
                b"original half notice",
                (prefix / "MNN-THIRD-PARTY-NOTICES.txt").read_bytes(),
            )
            self.assertFalse(mnn.verify_prefix(prefix, {**config, "machine": "other"}))
            (prefix / "lib/libMNN.a").write_bytes(b"damaged")
            self.assertFalse(mnn.verify_prefix(prefix, config))
            with self.assertRaises(ValueError):
                mnn.build(prefix, 2)
            self.assertEqual(len(calls), 2)
            metadata = json.loads((prefix / "insta360-mnn-build.json").read_text())
            metadata["files"] = {"../escape": "invalid"}
            (prefix / "insta360-mnn-build.json").write_text(json.dumps(metadata))
            self.assertFalse(mnn.verify_prefix(prefix, config))

    def test_source_digest_failure_never_invokes_cmake_or_publishes_prefix(self):
        prefix = self.root / "prefix"
        with (
            patch.object(
                mnn.urllib.request, "urlopen", return_value=io.BytesIO(b"wrong source")
            ),
            patch.object(mnn.subprocess, "run") as run,
        ):
            with self.assertRaisesRegex(ValueError, "digest"):
                mnn.build(prefix, 2)
            run.assert_not_called()
        self.assertFalse(prefix.exists())


if __name__ == "__main__":
    unittest.main()
