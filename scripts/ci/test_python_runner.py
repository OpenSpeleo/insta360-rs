"""Shipping wheel capability checks must fail before missing features are skipped."""

import importlib.machinery
import importlib.util
from pathlib import Path
import sys
from types import ModuleType, SimpleNamespace
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]


def load_script(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


runner = load_script("python_suite_runner", ROOT / "src-python/scripts/run-tests.py")


class WheelCapabilityTests(unittest.TestCase):
    def setUp(self):
        self.api = ModuleType("insta360_rs")
        self.api._native = SimpleNamespace(
            __file__="_native" + importlib.machinery.EXTENSION_SUFFIXES[0]
        )
        self.capabilities = SimpleNamespace(
            gpu_available=True, underwater_ai_compiled=True, hevc_encoders=["libx265"]
        )
        self.api.capabilities = mock.Mock(return_value=self.capabilities)
        self.reason = mock.Mock(return_value=None)
        self.api.SeamMode = SimpleNamespace(AI=SimpleNamespace(unavailable_reason=self.reason))
        self.enterContext(mock.patch.dict(sys.modules, {"insta360_rs": self.api}))
        self.enterContext(mock.patch.dict(runner.os.environ, {}, clear=True))
        self.enterContext(mock.patch.object(runner.shutil, "which", return_value="fixture-tool"))
        self.enterContext(mock.patch("builtins.print"))
        self.loader = self.enterContext(mock.patch.object(runner.unittest, "defaultTestLoader"))
        self.loader.discover.return_value.countTestCases.return_value = 10
        self.test_runner = self.enterContext(mock.patch.object(runner.unittest, "TextTestRunner"))
        self.test_runner.return_value.run.return_value.wasSuccessful.return_value = True

    def test_required_ai_stitching_fails_before_discovery_with_specific_reason(self):
        self.reason.return_value = "rebuild with ai-stitching to enable model213"
        with mock.patch.dict(runner.os.environ, {"INSTA360_RS_REQUIRE_AI_STITCHING": "1"}):
            with self.assertRaisesRegex(RuntimeError, "requires AI stitching: rebuild with ai-stitching"):
                runner.main()
        self.loader.discover.assert_not_called()
        self.test_runner.assert_not_called()

    def test_required_ai_stitching_runs_suite_when_available(self):
        with mock.patch.dict(runner.os.environ, {"INSTA360_RS_REQUIRE_AI_STITCHING": "1"}):
            self.assertEqual(runner.main(), 0)
        self.reason.assert_called_once_with()
        self.loader.discover.assert_called_once()
        self.test_runner.return_value.run.assert_called_once_with(self.loader.discover.return_value)

    def test_development_builds_may_test_disabled_ai_behavior(self):
        self.reason.return_value = "AI stitching was compiled out"
        self.assertEqual(runner.main(), 0)
        self.reason.assert_not_called()
        self.loader.discover.assert_called_once()

    def test_clean_smoke_rejects_either_missing_shipped_ai_engine(self):
        smoke = load_script("wheel_smoke", ROOT / "scripts/ci/smoke-wheel.py")
        self.api.probe = mock.Mock()
        for underwater, seam_reason, expected in (
            (False, None, "wheel must include the underwater AI engine"),
            (True, "missing ai-stitching feature", "wheel must include AI stitching"),
        ):
            with self.subTest(underwater=underwater, seam_reason=seam_reason):
                self.capabilities.underwater_ai_compiled = underwater
                self.reason.return_value = seam_reason
                with self.assertRaisesRegex(AssertionError, expected):
                    smoke.smoke(Path("fixture.insv"))
                self.api.probe.assert_not_called()


if __name__ == "__main__":
    unittest.main()
