"""Check the installed distribution and the Python-to-native call contract."""

import ast
import importlib.machinery
import importlib.metadata
import inspect
import unittest
from pathlib import Path
from unittest.mock import patch, sentinel

import insta360_rs as sdk
from insta360_rs import _native


class CustomPath:
    def __init__(self, value):
        self.value = value

    def __fspath__(self):
        return self.value


class InstalledApiTests(unittest.TestCase):
    def test_linked_mnn_version_matches_runtime_capability(self):
        if sdk.capabilities().underwater_ai_compiled:
            self.assertEqual(sdk.mnn_runtime_version(), "3.6.1")
        else:
            with self.assertRaises(sdk.MissingCapabilityError):
                sdk.mnn_runtime_version()

    def test_real_extension_and_distribution_version(self):
        self.assertTrue(
            any(
                str(_native.__file__).endswith(suffix)
                for suffix in importlib.machinery.EXTENSION_SUFFIXES
            )
        )
        self.assertEqual(sdk.__version__, _native.__version__)
        self.assertEqual(sdk.__version__, importlib.metadata.version("insta360-rs"))
        self.assertEqual(
            importlib.metadata.metadata("insta360-rs")["Requires-Python"], ">=3.10"
        )

    def test_every_export_is_available_and_star_import_agrees(self):
        self.assertEqual(len(sdk.__all__), len(set(sdk.__all__)))
        namespace = {}
        exec("from insta360_rs import *", namespace)
        self.assertEqual(set(namespace) - {"__builtins__"}, set(sdk.__all__))
        for name in sdk.__all__:
            with self.subTest(name=name):
                self.assertIs(namespace[name], getattr(sdk, name))
                if hasattr(_native, name):
                    self.assertIs(getattr(sdk, name), getattr(_native, name))

    def test_installed_typing_contract_covers_exports_members_and_signatures(self):
        package = Path(sdk.__file__).parent
        self.assertTrue((package / "py.typed").is_file())
        tree = ast.parse((package / "__init__.pyi").read_text())
        names = set()
        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.ClassDef)):
                names.add(node.name)
            elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
                names.add(node.target.id)
            if isinstance(node, ast.ClassDef):
                runtime = getattr(sdk, node.name)
                for member in node.body:
                    name = (
                        member.name
                        if isinstance(member, ast.FunctionDef)
                        else member.target.id
                        if isinstance(member, ast.AnnAssign)
                        else None
                    )
                    if name:
                        with self.subTest(type=node.name, member=name):
                            self.assertTrue(hasattr(runtime, name))
            if isinstance(node, ast.FunctionDef) and node.name != "capabilities":
                runtime = inspect.signature(getattr(sdk, node.name))
                expected = [arg.arg for arg in node.args.args + node.args.kwonlyargs]
                self.assertEqual(list(runtime.parameters), expected)
                for arg in node.args.kwonlyargs:
                    self.assertEqual(
                        runtime.parameters[arg.arg].kind, inspect.Parameter.KEYWORD_ONLY
                    )
        self.assertEqual(names, set(sdk.__all__))

    def test_exception_hierarchy_and_messages(self):
        for name in [name for name in sdk.__all__ if name.endswith("Error")]:
            with self.subTest(name=name):
                error_type = getattr(sdk, name)
                self.assertTrue(issubclass(error_type, sdk.Insta360Error))
                self.assertEqual(str(error_type("native details")), "native details")
        self.assertTrue(issubclass(sdk.GpuUnavailableError, sdk.MissingCapabilityError))
        self.assertTrue(issubclass(sdk.GpuProcessingError, sdk.MediaProcessingError))
        self.assertTrue(issubclass(sdk.Insta360Error, Exception))
        self.assertFalse(issubclass(sdk.Insta360IOError, OSError))


class WrapperContractTests(unittest.TestCase):
    def calls(self, inputs):
        """Every path-taking public function, including both asynchronous forms."""
        output = CustomPath("output with spaces/é")
        return [
            ("_probe", lambda: sdk.probe(inputs), []),
            ("_open_media", lambda: sdk.open_media(inputs), []),
            ("_extract", lambda: sdk.extract(inputs, output), [output.value]),
            (
                "_export_video",
                lambda: sdk.export_video(inputs, output),
                [
                    output.value,
                    None,
                    90,
                    sdk.AudioPolicy.COPY,
                    None,
                    None,
                    sdk.MediaAcceleration.AUTO,
                ],
            ),
            (
                "_start_export_video",
                lambda: sdk.start_export_video(inputs, output),
                [
                    output.value,
                    None,
                    90,
                    sdk.AudioPolicy.COPY,
                    None,
                    None,
                    sdk.MediaAcceleration.AUTO,
                ],
            ),
            (
                "_export_frames",
                lambda: sdk.export_frames(inputs, output, indices=(0, 2)),
                [
                    output.value,
                    None,
                    [0, 2],
                    None,
                    None,
                    None,
                    None,
                    sdk.ImageFormat.PNG,
                    95,
                    None,
                ],
            ),
            (
                "_start_export_frames",
                lambda: sdk.start_export_frames(inputs, output, indices=(0, 2)),
                [
                    output.value,
                    None,
                    [0, 2],
                    None,
                    None,
                    None,
                    None,
                    sdk.ImageFormat.PNG,
                    95,
                    None,
                ],
            ),
        ]

    def test_all_functions_normalize_strings_pathlikes_and_sequences(self):
        for inputs, expected in [
            ("media é.insv", ["media é.insv"]),
            (Path("recording.insv"), ["recording.insv"]),
            (CustomPath("custom.insv"), ["custom.insv"]),
            ([Path("one.insv"), "two.insv"], ["one.insv", "two.insv"]),
            ((CustomPath("one.insv"), Path("two.insv")), ["one.insv", "two.insv"]),
        ]:
            for native_name, call, arguments in self.calls(inputs):
                with self.subTest(native=native_name, inputs=inputs):
                    with patch.object(
                        sdk, native_name, return_value=sentinel.result
                    ) as native:
                        self.assertIs(call(), sentinel.result)
                    native.assert_called_once_with(expected, *arguments)

    def test_invalid_paths_do_not_reach_native(self):
        for inputs in [None, 42, b"bytes.insv", [object()], CustomPath(42)]:
            for native_name, call, _ in self.calls(inputs):
                with self.subTest(native=native_name, inputs=inputs):
                    with patch.object(sdk, native_name) as native:
                        with self.assertRaises(TypeError):
                            call()
                    native.assert_not_called()

    def test_video_options_are_forwarded_in_native_order(self):
        config = sdk.StitchConfig(width=64, height=32)
        for name in ["export_video", "start_export_video"]:
            with self.subTest(function=name):
                with patch.object(
                    sdk, "_" + name, return_value=sentinel.result
                ) as native:
                    result = getattr(sdk, name)(
                        "recording.insv",
                        Path("out.mp4"),
                        config=config,
                        quality=31,
                        audio=sdk.AudioPolicy.DROP,
                        start=1.25,
                        duration=2.5,
                        acceleration=sdk.MediaAcceleration.SOFTWARE,
                    )
                self.assertIs(result, sentinel.result)
                native.assert_called_once_with(
                    ["recording.insv"],
                    "out.mp4",
                    config,
                    31,
                    sdk.AudioPolicy.DROP,
                    1.25,
                    2.5,
                    sdk.MediaAcceleration.SOFTWARE,
                )

    def test_frame_options_and_all_selection_modes_reach_native(self):
        config = sdk.StitchConfig(width=64, height=32)
        selections = [
            ({"indices": (4, 2, 4)}, [[4, 2, 4], None, None, None, None]),
            ({"timestamps": (0.25, 1.5)}, [None, [0.25, 1.5], None, None, None]),
            ({"start": 0.0, "end": 2.0, "fps": 2.5}, [None, None, 0.0, 2.0, 2.5]),
            ({"indices": []}, [[], None, None, None, None]),
        ]
        for name in ["export_frames", "start_export_frames"]:
            for selection, arguments in selections:
                with self.subTest(function=name, selection=selection):
                    with patch.object(
                        sdk, "_" + name, return_value=sentinel.result
                    ) as native:
                        result = getattr(sdk, name)(
                            "recording.insv",
                            Path("frames"),
                            config=config,
                            format=sdk.ImageFormat.JPEG,
                            quality=37,
                            scale_width=32,
                            **selection,
                        )
                    self.assertIs(result, sentinel.result)
                    native.assert_called_once_with(
                        ["recording.insv"],
                        "frames",
                        config,
                        *arguments,
                        sdk.ImageFormat.JPEG,
                        37,
                        32,
                    )

    def test_native_errors_propagate_without_replacement(self):
        for native_name, call, _ in self.calls("recording.insv"):
            with self.subTest(native=native_name):
                error = sdk.InvalidMediaError("original diagnostic")
                with patch.object(sdk, native_name, side_effect=error):
                    with self.assertRaises(sdk.InvalidMediaError) as raised:
                        call()
                self.assertIs(raised.exception, error)


if __name__ == "__main__":
    unittest.main()
