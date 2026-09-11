"""Missing fixture capabilities must fail before a suite can silently skip them."""

import importlib.util
from pathlib import Path
import shutil
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "check_test_tools", Path(__file__).with_name("check-test-tools.py")
)
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)


class TestToolTests(unittest.TestCase):
    def test_missing_executables_are_reported_together(self):
        with patch.object(checker.shutil, "which", return_value=None):
            with self.assertRaisesRegex(RuntimeError, "ffmpeg, ffprobe"):
                checker.verify()

    def test_unknown_encoder_is_rejected_even_when_ffmpeg_exits_successfully(self):
        with (
            patch.object(checker.shutil, "which", return_value="ffmpeg"),
            patch.object(checker, "run", return_value="Codec 'mpeg4' is not recognized"),
        ):
            with self.assertRaisesRegex(RuntimeError, "missing the mpeg4 encoder"):
                checker.verify()

    def test_eight_bit_only_hevc_does_not_qualify_ten_bit_fixtures(self):
        def describe(*command):
            if command[-1].startswith("encoder="):
                name = command[-1].removeprefix("encoder=")
                return f"Encoder {name} [fixture]:\nSupported pixel formats: yuv420p"
            return "fixture tool version"

        with (
            patch.object(checker.shutil, "which", return_value="ffmpeg"),
            patch.object(checker, "run", side_effect=describe),
        ):
            with self.assertRaisesRegex(RuntimeError, "10-bit YUV420"):
                checker.verify()

    @unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "requires media tools")
    def test_installed_tools_generate_and_probe_the_required_fixture(self):
        checker.verify()


if __name__ == "__main__":
    unittest.main()
