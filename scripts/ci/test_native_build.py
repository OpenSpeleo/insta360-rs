"""Exercise native build orchestration with stand-ins for platform build tools."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


@unittest.skipUnless(os.name == "posix" and shutil.which("bash"), "requires POSIX bash")
class NativeBuildTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="native build ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.prefix = self.root / "prefix"
        self.build = self.root / "build"
        self.ffmpeg = self.build / "ffmpeg-test"
        self.ffmpeg.mkdir(parents=True)
        self.tools = self.root / "tools"
        self.tools.mkdir()
        for name in ("include", "lib", "bin"):
            (self.prefix / name).mkdir(parents=True)
        # Match the import library names installed by upstream CMake in CI.
        (self.prefix / "lib/z.lib").write_bytes(b"zlib import library")
        (self.prefix / "lib/libx265.lib").write_bytes(b"x265 import library")
        zlib = self.build / "zlib-1.3.2"
        zlib.mkdir()
        (zlib / "LICENSE").write_text("zlib license\n")
        self.script = self.root / "build-ffmpeg-native.sh"
        shutil.copyfile(Path(__file__).with_name(self.script.name), self.script)
        # Stub downloads and compilation; execute the real orchestration script.
        (self.root / "ffmpeg-runtime-config.sh").write_text("""
ffmpeg_version=test
ffmpeg_archive=ffmpeg-test.tar.xz
x265_version=test
x265_archive=x265-test.tar.gz
x265_options=()
ffmpeg_options=()
fetch_runtime_sources() { :; }
fetch_source() { :; }
write_runtime_notices() { mkdir -p "$prefix/share/insta360-rs"; }
""")
        self.executable(self.tools / "uname", 'printf "%s\\n" "$TEST_PLATFORM"\n')
        self.executable(self.tools / "tar", "exit 0\n")
        self.executable(self.tools / "cmake", "exit 0\n")
        self.executable(
            self.tools / "cygpath",
            "import sys\n"
            "path = sys.argv[2]\n"
            "print('C:' + path.replace('/', chr(92)) if sys.argv[1] == '-w' else path)\n",
            python=True,
        )
        self.executable(self.ffmpeg / "configure", """
import json
import os
from pathlib import Path
import sys
Path('configuration.json').write_text(json.dumps({
    'args': sys.argv[1:],
    'env': {key: os.environ.get(key) for key in ('INCLUDE', 'LIB', 'PATH')},
}))
if os.environ.get('TEST_CONFIGURE_STATUS'):
    Path('ffbuild').mkdir()
    Path('ffbuild/config.log').write_text('linker diagnostic from configure\\n')
    sys.exit(int(os.environ['TEST_CONFIGURE_STATUS']))
""", python=True)
        self.executable(self.tools / "make", 'touch make-called\n')
        self.env = os.environ.copy()
        for name in ("INCLUDE", "LIB", "BASH_ENV", "ENV"):
            self.env.pop(name, None)
        self.env.update(
            PATH=str(self.tools) + os.pathsep + os.environ["PATH"],
            TEST_PLATFORM="MSYS_NT-10.0-26100",
            INSTA360_FFMPEG_PREFIX=str(self.prefix),
            INSTA360_WHEEL_CACHE=str(self.root / "cache"),
            INSTA360_WHEEL_JOBS="2",
            INSTA360_CMAKE=str(self.tools / "cmake"),
            MACOSX_DEPLOYMENT_TARGET="11.0",
        )

    def executable(self, path, content, python=False):
        path.write_text(f"#!{sys.executable if python else '/bin/sh'}\n{content}")
        path.chmod(0o755)

    def run_build(self):
        return subprocess.run(
            ["bash", str(self.script)], env=self.env, text=True, capture_output=True,
            timeout=30,
        )

    def test_windows_import_libraries_and_search_paths(self):
        for inherited in (False, True):
            with self.subTest(inherited=inherited):
                for key in ("INCLUDE", "LIB"):
                    if inherited:
                        self.env[key] = rf"C:\Program Files\MSVC\{key};C:\Windows Kits\{key}"
                complete = self.prefix / "share/insta360-rs/complete"
                complete.unlink(missing_ok=True)
                result = self.run_build()
                self.assertEqual(result.returncode, 0, result.stderr)
                for source, alias in (("z.lib", "zlib.lib"), ("libx265.lib", "x265.lib")):
                    self.assertEqual(
                        (self.prefix / "lib" / alias).read_bytes(),
                        (self.prefix / "lib" / source).read_bytes(),
                    )
                config = json.loads((self.ffmpeg / "configuration.json").read_text())
                self.assertIn("--toolchain=msvc", config["args"])
                for key, directory in (("INCLUDE", "include"), ("LIB", "lib")):
                    expected = "C:" + str(self.prefix / directory).replace("/", "\\")
                    if inherited:
                        expected += ";" + self.env[key]
                    self.assertEqual(config["env"][key], expected)
                self.assertEqual(
                    config["env"]["PATH"], str(self.prefix / "bin") + ":" + self.env["PATH"]
                )
                self.assertTrue(complete.is_file())

    def test_configure_failure_prints_log_and_stops_build(self):
        self.env["TEST_CONFIGURE_STATUS"] = "23"
        result = self.run_build()
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertIn("linker diagnostic from configure", result.stderr)
        self.assertFalse((self.ffmpeg / "make-called").exists())
        self.assertFalse((self.prefix / "share/insta360-rs/complete").exists())

    def test_macos_does_not_apply_windows_setup(self):
        self.env["TEST_PLATFORM"] = "Darwin"
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads((self.ffmpeg / "configuration.json").read_text())
        self.assertNotIn("--toolchain=msvc", config["args"])
        self.assertIn("--install-name-dir=@rpath", config["args"])
        for key in ("INCLUDE", "LIB", "PATH"):
            self.assertEqual(config["env"][key], self.env.get(key))
        self.assertFalse((self.prefix / "lib/zlib.lib").exists())


if __name__ == "__main__":
    unittest.main()
