"""Verify source identity, native cache integrity, and bounded MNN extraction."""

import hashlib
import importlib.util
import io
import json
import subprocess
import tarfile
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "build_mnn", Path(__file__).with_name("build-mnn.py")
)
mnn = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mnn)


class MnnBuildTests(unittest.TestCase):
    def test_cli_configuration_is_read_only(self):
        output = io.StringIO()
        with (
            patch("sys.argv", ["build-mnn.py", "--configuration"]),
            patch.object(mnn, "build") as build,
            redirect_stdout(output),
        ):
            mnn.main()
        self.assertEqual(json.loads(output.getvalue()), mnn.build_configuration())
        build.assert_not_called()

    def test_cli_verification_never_builds_or_downloads(self):
        for valid in (True, False):
            with (
                self.subTest(valid=valid),
                patch(
                    "sys.argv", ["build-mnn.py", "--output", "prefix", "--verify-only"]
                ),
                patch.object(mnn, "verify_prefix", return_value=valid) as verify,
                patch.object(mnn, "build") as build,
                redirect_stdout(io.StringIO()),
                redirect_stderr(io.StringIO()),
            ):
                if valid:
                    mnn.main()
                else:
                    with self.assertRaises(SystemExit) as failure:
                        mnn.main()
                    self.assertEqual(failure.exception.code, 2)
                verify.assert_called_once_with(
                    Path("prefix"), mnn.build_configuration()
                )
                build.assert_not_called()

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

    def source_archive(self):
        source = f"MNN-{mnn.COMMIT}"
        return self.archive(
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

    def test_verified_build_cache_rejects_changed_native_bytes_and_configuration(self):
        body = self.source_archive().read_bytes()
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

    def test_deep_cache_build_uses_short_work_paths_and_atomic_publication(self):
        body = self.source_archive().read_bytes()
        prefix = (
            self.root
            / "src-tauri/.mnn-cache"
            / f"mnn-windows-x86_64-{'a' * 64}"
            / "install"
        ).resolve()
        work_paths = []
        rename = Path.rename

        def compile_source(args, *, check):
            self.assertTrue(check)
            if "-S" in args:
                source = Path(args[args.index("-S") + 1])
                build = Path(args[args.index("-B") + 1])
                self.assertTrue((source / "include/MNN/Interpreter.hpp").is_file())
                self.assertFalse(source.is_relative_to(prefix.parent))
                self.assertFalse(build.is_relative_to(prefix.parent))
                self.assertEqual(build.parent.parent, Path(tempfile.gettempdir()))
                work_paths.append(build.parent)
                self.assertEqual(args[5:], list(mnn.CMAKE_FLAGS))
            else:
                build = Path(args[args.index("--build") + 1])
                self.assertEqual(
                    args,
                    [
                        "cmake",
                        "--build",
                        str(build),
                        "--config",
                        "Release",
                        "--target",
                        "MNN",
                        "--parallel",
                        "2",
                    ],
                )
                (build / "Release").mkdir(parents=True)
                (build / "Release/MNN.lib").write_bytes(b"static CPU library")

        def publish(source, target):
            self.assertEqual(target, prefix.resolve())
            self.assertEqual(source.parent.parent, target.parent)
            self.assertFalse(target.exists())
            self.assertTrue(mnn.verify_prefix(source, mnn.build_configuration()))
            return rename(source, target)

        with (
            patch.object(mnn, "SHA256", hashlib.sha256(body).hexdigest()),
            patch.object(mnn.urllib.request, "urlopen", return_value=io.BytesIO(body)),
            patch.object(mnn.subprocess, "run", side_effect=compile_source),
            patch.object(Path, "rename", autospec=True, side_effect=publish) as move,
            redirect_stdout(io.StringIO()),
        ):
            mnn.build(prefix, 2)
            move.assert_called_once()
            self.assertTrue(mnn.verify_prefix(prefix, mnn.build_configuration()))
        self.assertEqual(len(work_paths), 1)
        self.assertFalse(work_paths[0].exists())
        self.assertEqual(list(prefix.parent.iterdir()), [prefix])
        self.assertEqual((prefix / "lib/MNN.lib").read_bytes(), b"static CPU library")

    def test_cmake_failures_remove_work_and_staging_without_publishing(self):
        body = self.source_archive().read_bytes()
        for failed_step in ("-S", "--build"):
            with self.subTest(failed_step=failed_step):
                prefix = self.root / failed_step / "install"
                work_paths = []

                def compile_source(args, *, check):
                    self.assertTrue(check)
                    if "-S" in args:
                        build = Path(args[args.index("-B") + 1])
                        build.mkdir()
                        work_paths.append(build.parent)
                    if failed_step in args:
                        raise subprocess.CalledProcessError(1, args)

                with (
                    patch.object(mnn, "SHA256", hashlib.sha256(body).hexdigest()),
                    patch.object(
                        mnn.urllib.request, "urlopen", return_value=io.BytesIO(body)
                    ),
                    patch.object(mnn.subprocess, "run", side_effect=compile_source),
                ):
                    with self.assertRaises(subprocess.CalledProcessError):
                        mnn.build(prefix, 2)
                self.assertEqual(len(work_paths), 1)
                self.assertFalse(work_paths[0].exists())
                self.assertEqual(list(prefix.parent.iterdir()), [])

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
