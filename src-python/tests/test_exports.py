"""End-to-end image/video exports and native argument-validation contracts."""

import json
import subprocess
import tempfile
import unittest
from fractions import Fraction
from pathlib import Path

import insta360_rs as api
from export_fixtures import (
    MEDIA_TOOLS_AVAILABLE,
    ExportFixtureMixin,
    calibrated_metadata,
    cpu_config,
    make_recording,
    media_description,
    rgb_pixels,
    trailer,
)
from media_fixtures import assert_readonly, ffprobe, run_tool


class ExportValidationTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.source = self.root / "missing.insv"

    def check_frames(self, error, **kwargs):
        for export in (api.export_frames, api.start_export_frames):
            with self.subTest(api=export.__name__, options=kwargs):
                with self.assertRaises(error):
                    export(self.source, self.root / "frames", **kwargs)
        self.assertEqual(list(self.root.iterdir()), [])

    def check_video(self, error, **kwargs):
        for export in (api.export_video, api.start_export_video):
            with self.subTest(api=export.__name__, options=kwargs):
                with self.assertRaises(error):
                    export(self.source, self.root / "video.mp4", **kwargs)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_requires_exactly_one_nonempty_selection_mode(self):
        for options in (
            {},
            {"indices": []},
            {"timestamps": []},
            {"indices": [0], "timestamps": [0]},
            {"indices": [0], "start": 0},
            {"timestamps": [0], "fps": 1},
            {"indices": [], "timestamps": [0]},
        ):
            self.check_frames(api.InvalidMediaError, **options)

    def test_sample_range_requires_all_three_values(self):
        for options in (
            {"start": 0},
            {"end": 1},
            {"fps": 1},
            {"start": 0, "end": 1},
            {"start": 0, "fps": 1},
            {"end": 1, "fps": 1},
        ):
            self.check_frames(api.InvalidMediaError, **options)

    def test_selection_timestamps_reject_nonfinite_negative_and_overflow(self):
        for value in (-1, float("nan"), float("inf"), float("-inf"), 1e30):
            self.check_frames(api.InvalidMediaError, timestamps=[value])
            self.check_frames(api.InvalidMediaError, start=value, end=1, fps=1)
            self.check_frames(api.InvalidMediaError, start=0, end=value, fps=1)

    def test_sample_range_validates_order_and_supported_rate(self):
        for start, end in ((1, 1), (2, 1)):
            self.check_frames(api.InvalidMediaError, start=start, end=end, fps=1)
        for fps in (0, -1, float("nan"), float("inf"), 0.0001, 1e20):
            self.check_frames(api.InvalidMediaError, start=0, end=1, fps=fps)

    def test_indices_enforce_native_integer_bounds(self):
        for index, error in (
            (-1, OverflowError),
            (2**64, OverflowError),
            (0.5, TypeError),
            ("0", TypeError),
        ):
            self.check_frames(error, indices=[index])

    def test_quality_rejects_out_of_range_values_before_io(self):
        for quality in (0, 101, 255):
            self.check_frames(api.InvalidMediaError, indices=[0], quality=quality)
            self.check_video(api.InvalidMediaError, quality=quality)
        for quality, error in (
            (-1, OverflowError),
            (256, OverflowError),
            (90.5, TypeError),
            ("90", TypeError),
        ):
            self.check_frames(error, indices=[0], quality=quality)
            self.check_video(error, quality=quality)

    def test_scale_width_validates_type_and_nonzero(self):
        for width, error in (
            (0, api.InvalidMediaError),
            (-1, OverflowError),
            (2**32, OverflowError),
            (2.5, TypeError),
        ):
            self.check_frames(error, indices=[0], scale_width=width)

    def test_video_interval_validates_finite_nonnegative_values(self):
        for value in (-1, float("nan"), float("inf"), float("-inf"), 1e30):
            self.check_video(api.InvalidMediaError, start=value)
            self.check_video(api.InvalidMediaError, duration=value)
        self.check_video(api.InvalidMediaError, duration=0)

    def test_wrong_configuration_and_option_types_raise_type_error(self):
        self.check_frames(TypeError, indices=[0], config={})
        self.check_frames(TypeError, indices=[0], format="PNG")
        self.check_video(TypeError, config={})
        self.check_video(TypeError, audio="DROP")
        self.check_video(TypeError, acceleration="SOFTWARE")

    def test_valid_requests_for_missing_input_raise_io_error(self):
        self.check_frames(api.Insta360IOError, indices=[0])
        self.check_video(api.Insta360IOError, start=0, duration=1)

    def test_empty_input_raises_invalid_media_error(self):
        for export in (api.export_frames, api.start_export_frames):
            with self.assertRaises(api.InvalidMediaError):
                export([], self.root / "frames", indices=[0])
        for export in (api.export_video, api.start_export_video):
            with self.assertRaises(api.InvalidMediaError):
                export([], self.root / "video.mp4")


@unittest.skipUnless(
    MEDIA_TOOLS_AVAILABLE, "ffmpeg and ffprobe fixture tools unavailable"
)
class FrameExportIntegrationTests(ExportFixtureMixin, unittest.TestCase):
    def test_indices_are_sorted_deduplicated_and_written_as_valid_png(self):
        result = api.export_frames(
            self.source, self.root / "frames", indices=(7, 0, 7, 2), config=cpu_config()
        )
        self.assert_cpu_result(result, 3)
        self.assertEqual(
            [path.name for path in result.outputs],
            [f"frame_{index:08}.png" for index in (0, 2, 7)],
        )
        pixels = []
        for path in result.outputs:
            self.assertTrue(path.read_bytes().startswith(b"\x89PNG\r\n\x1a\n"))
            descriptor = media_description(path)["streams"][0]
            self.assertEqual((descriptor["width"], descriptor["height"]), (128, 64))
            decoded = rgb_pixels(path)
            self.assertEqual(len(decoded), 128 * 64 * 3)
            self.assertGreater(len(set(decoded)), 20)
            pixels.append(decoded)
        self.assertNotEqual(
            pixels[0], pixels[-1], "selection must advance decoded input"
        )
        with self.assertRaises(AttributeError):
            result.frames_written = 0
        with self.assertRaises(AttributeError):
            result.backend.selected = api.EffectiveBackend.GPU
        result.outputs.clear()
        self.assertEqual(len(result.outputs), 3, "output getters must return snapshots")

    def test_resolved_optics_preserve_requested_detected_and_effective_settings(self):
        result = api.export_frames(
            self.source, self.root / "optics", indices=[0], config=cpu_config()
        )
        report = result.optics
        self.assertIsInstance(report, api.OpticalResolution)
        self.assertEqual(report.requested.housing, api.Housing.AUTO)
        self.assertEqual(report.detected.housing, api.Housing.NONE)
        self.assertEqual(report.effective.environment, api.Environment.AIR)
        self.assertEqual(report.source_lens_id, 113)
        self.assertEqual(report.target_lens_id, 113)
        self.assertEqual(report.evidence, "encoded_lens")
        self.assertFalse(report.sensor_crop_applied)
        assert_readonly(self, result, ["optics"])
        assert_readonly(
            self,
            report,
            [
                "requested",
                "detected",
                "effective",
                "evidence",
                "source_lens_id",
                "target_lens_id",
                "sensor_crop_applied",
            ],
        )

    def test_underwater_modes_execute_and_independent_images_reset_temporal_state(self):
        modes = [(api.UnderwaterColorMode.LEGACY, None)]
        if api.capabilities().underwater_ai_compiled:
            modes += [(api.UnderwaterColorMode.AI, style) for style in range(4)]
        off = api.export_frames(
            self.source, self.root / "off", indices=[0, 3], config=cpu_config()
        )
        for index, (mode, style) in enumerate(modes):
            with self.subTest(mode=mode, style=style):
                options = api.UnderwaterColorOptions(mode=mode, style=style)
                config = cpu_config(underwater_color=options)
                result = api.export_frames(
                    self.source,
                    self.root / f"restored-{index}",
                    indices=[0, 3],
                    config=config,
                )
                self.assertEqual(result.frames_written, 2)
                self.assertNotEqual(
                    result.outputs[0].read_bytes(), off.outputs[0].read_bytes()
                )
                single = api.export_frames(
                    self.source,
                    self.root / f"single-{index}",
                    indices=[3],
                    config=config,
                )
                self.assertEqual(
                    result.outputs[1].read_bytes(), single.outputs[0].read_bytes()
                )
                config.underwater_color = api.UnderwaterColorOptions(
                    mode=mode, strength=0, style=style
                )
                zero = api.export_frames(
                    self.source, self.root / f"zero-{index}", indices=[0], config=config
                )
                self.assertEqual(
                    zero.outputs[0].read_bytes(), off.outputs[0].read_bytes()
                )

    def test_export_snapshots_underwater_options_before_configuration_replacement(self):
        config = cpu_config(
            underwater_color=api.UnderwaterColorOptions(
                mode=api.UnderwaterColorMode.LEGACY
            )
        )
        expected = api.export_frames(
            self.source, self.root / "expected", indices=[0], config=config
        )
        job = api.start_export_frames(
            self.source, self.root / "snapshot", indices=[0], config=config
        )
        config.underwater_color = api.UnderwaterColorOptions()
        actual = job.wait()
        off = api.export_frames(
            self.source, self.root / "off", indices=[0], config=config
        )
        self.assertEqual(
            actual.outputs[0].read_bytes(), expected.outputs[0].read_bytes()
        )
        self.assertNotEqual(actual.outputs[0].read_bytes(), off.outputs[0].read_bytes())

    def test_timestamps_choose_first_frame_at_or_after_request(self):
        indexed = api.export_frames(
            self.source, self.root / "indexed", indices=[0, 3, 5], config=cpu_config()
        )
        timed = api.export_frames(
            self.source,
            self.root / "timed",
            timestamps=(0.45, 0.0, 0.21, 0.45),
            config=cpu_config(),
        )
        self.assert_cpu_result(timed, 3)
        self.assertEqual(
            [path.name for path in timed.outputs],
            [f"frame_{timestamp:016}.png" for timestamp in (0, 210000, 450000)],
        )
        self.assertEqual(
            [path.read_bytes() for path in timed.outputs],
            [path.read_bytes() for path in indexed.outputs],
        )

    def test_several_timestamps_can_select_the_same_decoded_frame(self):
        result = api.export_frames(
            self.source,
            self.root / "frames",
            timestamps=[0.21, 0.22, 0.3],
            config=cpu_config(),
        )
        self.assert_cpu_result(result, 3)
        self.assertEqual(len({path.read_bytes() for path in result.outputs}), 1)

    def test_seek_to_exact_frame_timestamp_does_not_skip_that_frame(self):
        indexed = api.export_frames(
            self.source, self.root / "indexed", indices=[7], config=cpu_config()
        )
        timed = api.export_frames(
            self.source, self.root / "timed", timestamps=[0.7], config=cpu_config()
        )
        self.assertEqual(indexed.outputs[0].read_bytes(), timed.outputs[0].read_bytes())

    def test_timestamp_seek_discards_unequal_long_gop_preroll_before_synchronizing(
        self,
    ):
        source = make_recording(self.root, duration=2, gop=12, b_frames=3)
        indexed = api.export_frames(
            source, self.root / "indexed", indices=[15, 18], config=cpu_config()
        )
        timed = api.export_frames(
            source, self.root / "timed", timestamps=[1.5, 1.8], config=cpu_config()
        )
        self.assert_cpu_result(timed, 2)
        self.assertEqual(
            [path.read_bytes() for path in timed.outputs],
            [path.read_bytes() for path in indexed.outputs],
        )

    def test_sampled_range_includes_end_when_it_lands_on_the_grid(self):
        result = api.export_frames(
            self.source,
            self.root / "frames",
            start=0.2,
            end=0.6,
            fps=5,
            config=cpu_config(),
        )
        self.assert_cpu_result(result, 3)
        self.assertEqual(
            [path.name for path in result.outputs],
            [f"frame_{timestamp:016}.png" for timestamp in (200000, 400000, 600000)],
        )
        expected = api.export_frames(
            self.source, self.root / "indexed", indices=[2, 4, 6], config=cpu_config()
        )
        self.assertEqual(
            [path.read_bytes() for path in result.outputs],
            [path.read_bytes() for path in expected.outputs],
        )

    def test_sampled_range_rounds_fps_to_thousandths(self):
        result = api.export_frames(
            self.source,
            self.root / "frames",
            start=0,
            end=0.5,
            fps=4.0001,
            config=cpu_config(),
        )
        self.assert_cpu_result(result, 3)
        self.assertEqual(
            [path.name for path in result.outputs],
            [f"frame_{timestamp:016}.png" for timestamp in (0, 250000, 500000)],
        )

    def test_sampled_range_rounds_each_timestamp_without_drift(self):
        result = api.export_frames(
            self.source,
            self.root / "frames",
            start=0,
            end=0.1,
            fps=30,
            config=cpu_config(),
        )
        self.assert_cpu_result(result, 4)
        self.assertEqual(
            [path.name for path in result.outputs],
            [f"frame_{timestamp:016}.png" for timestamp in (0, 33333, 66667, 100000)],
        )
        indexed = api.export_frames(
            self.source, self.root / "indexed", indices=[0, 1], config=cpu_config()
        )
        self.assertEqual(
            result.outputs[0].read_bytes(), indexed.outputs[0].read_bytes()
        )
        self.assertTrue(
            all(
                path.read_bytes() == indexed.outputs[1].read_bytes()
                for path in result.outputs[1:]
            )
        )

    def test_jpeg_quality_boundaries_and_scale_are_applied(self):
        sizes = []
        for quality in (1, 100):
            result = api.export_frames(
                self.source,
                self.root / str(quality),
                indices=[0],
                format=api.ImageFormat.JPEG,
                quality=quality,
                scale_width=64,
                config=cpu_config(),
            )
            self.assert_cpu_result(result, 1)
            path = result.outputs[0]
            self.assertEqual(path.suffix, ".jpg")
            self.assertTrue(path.read_bytes().startswith(b"\xff\xd8"))
            stream = media_description(path)["streams"][0]
            self.assertEqual((stream["width"], stream["height"]), (64, 32))
            sizes.append(path.stat().st_size)
        self.assertGreater(sizes[1], sizes[0])

    def test_default_projection_and_explicit_scale(self):
        config = api.StitchConfig(
            backend=api.ProcessingBackend.CPU, stabilization=api.Stabilization.OFF
        )
        result = api.export_frames(
            self.source,
            self.root / "frames",
            indices=[0],
            config=config,
            scale_width=80,
        )
        stream = media_description(result.outputs[0])["streams"][0]
        self.assertEqual((stream["width"], stream["height"]), (80, 40))

    def test_default_configuration_exports_a_calibrated_recording(self):
        result = api.export_frames(self.source, self.root / "frames", indices=[0])
        self.assertEqual(result.frames_written, 1)
        self.assertEqual(result.backend.requested, api.ProcessingBackend.AUTO)
        stream = media_description(result.outputs[0])["streams"][0]
        self.assertEqual(stream["width"], 2 * stream["height"])
        self.assertGreater(stream["height"], 0)

    def test_stabilization_modes_use_embedded_motion(self):
        outputs = []
        for mode in (
            api.Stabilization.OFF,
            api.Stabilization.FLOW_STATE,
            api.Stabilization.DIRECTION_LOCK,
        ):
            result = api.export_frames(
                self.source,
                self.root / str(mode),
                indices=[2],
                config=cpu_config(stabilization=mode),
            )
            self.assert_cpu_result(result, 1)
            outputs.append(result.outputs[0].read_bytes())
        self.assertEqual(
            len(set(outputs)),
            1,
            "stationary upright camera leaves orientation unchanged",
        )

    def test_color_conversion_changes_pixels_and_preserve_matches_auto_without_log(
        self,
    ):
        outputs = []
        for conversion in (
            api.ColorConversion.AUTO,
            api.ColorConversion.PRESERVE,
            api.ColorConversion.I_LOG_TO_REC709,
        ):
            result = api.export_frames(
                self.source,
                self.root / str(conversion),
                indices=[0],
                config=cpu_config(color_conversion=conversion),
            )
            outputs.append(rgb_pixels(result.outputs[0]))
        self.assertEqual(outputs[0], outputs[1])
        self.assertNotEqual(outputs[1], outputs[2])

    def test_underwater_preset_runs_with_calibrated_underwater_recording(self):
        source = self.root / "underwater.insv"
        source.write_bytes(
            (self.fixture_root / "dual-lens.mp4").read_bytes()
            + trailer(calibrated_metadata(underwater=True))
        )
        config = api.StitchConfig.underwater_photogrammetry(
            backend=api.ProcessingBackend.CPU, width=128, height=64
        )
        result = api.export_frames(
            source, self.root / "frames", indices=[0], config=config
        )
        self.assert_cpu_result(result, 1)

    def test_unfulfillable_selection_rolls_back_images_already_written(self):
        for options in (
            {"indices": [0, 100]},
            {"timestamps": [0, 2]},
            {"start": 0, "end": 2, "fps": 1},
        ):
            output = self.root / "frames"
            with self.subTest(options=options):
                with self.assertRaises(api.InvalidMediaError):
                    api.export_frames(
                        self.source, output, config=cpu_config(), **options
                    )
                self.assertEqual(list(output.iterdir()), [])

    def test_existing_image_is_preserved_and_partial_export_is_removed(self):
        output = self.root / "frames"
        output.mkdir()
        existing = output / "frame_00000002.png"
        existing.write_bytes(b"existing bytes")
        with self.assertRaises(api.InvalidMediaError):
            api.export_frames(self.source, output, indices=[0, 2], config=cpu_config())
        self.assertEqual(existing.read_bytes(), b"existing bytes")
        self.assertEqual(list(output.iterdir()), [existing])

    def test_output_directory_that_is_a_file_returns_io_error(self):
        output = self.root / "frames"
        output.write_bytes(b"preserved")
        with self.assertRaises(api.Insta360IOError):
            api.export_frames(self.source, output, indices=[0], config=cpu_config())
        self.assertEqual(output.read_bytes(), b"preserved")

    def test_paired_files_with_two_video_tracks_each_reject_ambiguous_layout(self):
        primary = self.root / "VID_20260101_120000_00_001.insv"
        secondary = self.root / "VID_20260101_120000_10_001.insv"
        primary.write_bytes(self.source.read_bytes())
        secondary.write_bytes(self.source.read_bytes())
        for inputs in (primary, [secondary, primary]):
            with self.subTest(inputs=inputs):
                with self.assertRaises(api.MissingCapabilityError):
                    api.export_frames(
                        inputs, self.root / "frames", indices=[0], config=cpu_config()
                    )
                with self.assertRaises(api.MissingCapabilityError):
                    api.export_video(
                        inputs,
                        self.root / "video.mp4",
                        config=cpu_config(),
                        audio=api.AudioPolicy.DROP,
                    )
        self.assertFalse((self.root / "frames").exists())
        self.assertFalse((self.root / "video.mp4").exists())

    def test_odd_scale_and_mutated_invalid_projection_are_rejected(self):
        with self.assertRaises(api.InvalidMediaError):
            api.export_frames(
                self.source,
                self.root / "odd",
                indices=[0],
                scale_width=63,
                config=cpu_config(),
            )
        for change in ({"width": 0}, {"height": None}, {"height": 63}):
            config = cpu_config()
            for field, value in change.items():
                setattr(config, field, value)
            with self.assertRaises(api.InvalidMediaError):
                api.export_frames(
                    self.source, self.root / "invalid", indices=[0], config=config
                )
        self.assertFalse((self.root / "odd").exists())
        self.assertFalse((self.root / "invalid").exists())

    def test_native_source_errors_preserve_typed_exceptions(self):
        for name, metadata, gyro, config, error in (
            (
                "unsupported",
                calibrated_metadata(camera="Unknown Future Camera"),
                True,
                cpu_config(),
                api.UnsupportedCameraError,
            ),
            (
                "uncalibrated",
                calibrated_metadata(calibration=False),
                True,
                cpu_config(),
                api.MissingCalibrationError,
            ),
            (
                "no-gyro",
                calibrated_metadata(),
                False,
                cpu_config(stabilization=api.Stabilization.DIRECTION_LOCK),
                api.MissingCapabilityError,
            ),
        ):
            with self.subTest(source=name):
                source = self.root / f"{name}.insv"
                source.write_bytes(
                    (self.fixture_root / "dual-lens.mp4").read_bytes()
                    + trailer(metadata, gyro=gyro)
                )
                with self.assertRaises(error):
                    api.export_frames(
                        source, self.root / name, indices=[0], config=config
                    )
                self.assertFalse((self.root / name).exists())

    def test_export_does_not_modify_source_recording(self):
        before = self.source.read_bytes()
        api.export_frames(
            self.source, self.root / "frames", indices=[0], config=cpu_config()
        )
        self.assertEqual(self.source.read_bytes(), before)


@unittest.skipUnless(
    MEDIA_TOOLS_AVAILABLE, "ffmpeg and ffprobe fixture tools unavailable"
)
class VideoExportIntegrationTests(ExportFixtureMixin, unittest.TestCase):
    def export_video(self, name="video.mp4", **options):
        if not ({"libx265", "libkvazaar"} & set(api.capabilities().hevc_encoders)):
            self.skipTest("native FFmpeg lacks a software HEVC encoder")
        values = dict(
            config=cpu_config(),
            audio=api.AudioPolicy.DROP,
            acceleration=api.MediaAcceleration.SOFTWARE,
        )
        values.update(options)
        return api.export_video(self.source, self.root / name, **values)

    def test_software_hevc_export_contains_all_frames_and_drops_audio(self):
        result = self.export_video()
        self.assert_cpu_result(result, 10)
        self.assertEqual(result.outputs, [self.root / "video.mp4"])
        streams = media_description(result.outputs[0])["streams"]
        self.assertEqual(len(streams), 1)
        stream = streams[0]
        self.assertEqual(stream["codec_name"], "hevc")
        self.assertEqual((stream["width"], stream["height"]), (128, 64))
        self.assertEqual(stream["r_frame_rate"], "10/1")
        self.assertEqual(int(stream["nb_read_frames"]), 10)
        self.assertAlmostEqual(float(stream["duration"]), 1)
        self.assertGreater(len(set(rgb_pixels(result.outputs[0]))), 20)

    def test_interval_is_start_inclusive_end_exclusive_and_rebased_to_zero(self):
        result = self.export_video(start=0.2, duration=0.4)
        self.assert_cpu_result(result, 4)
        stream = media_description(result.outputs[0])["streams"][0]
        self.assertEqual(int(stream["nb_read_frames"]), 4)
        self.assertEqual(float(stream["start_time"]), 0)
        self.assertAlmostEqual(float(stream["duration"]), 0.4)

    def test_start_only_duration_only_and_interval_clipping(self):
        for index, options, count in (
            (0, {"start": 0.7}, 3),
            (1, {"duration": 0.3}, 3),
            (2, {"start": 0.8, "duration": 100}, 2),
        ):
            with self.subTest(options=options):
                result = self.export_video(f"{index}.mp4", **options)
                self.assert_cpu_result(result, count)

    def test_quality_boundaries_are_valid_hevc(self):
        for quality in (1, 100):
            with self.subTest(quality=quality):
                result = self.export_video(
                    f"{quality}.mp4", quality=quality, duration=0.1
                )
                self.assert_cpu_result(result, 1)
                self.assertEqual(
                    media_description(result.outputs[0])["streams"][0]["codec_name"],
                    "hevc",
                )

    def test_repeated_one_and_two_frame_exports_have_valid_packet_timestamps(self):
        # x265 can leave its B-frame delay uninitialized when flushed before
        # three frames. Repeat both short lengths to expose allocator-dependent
        # garbage DTS, including files that mux successfully with wrong timing.
        for repetition in range(3):
            for count in (1, 2):
                with self.subTest(repetition=repetition, frames=count):
                    duration = count / 10
                    result = self.export_video(
                        f"short-{repetition}-{count}.mp4", start=0.2, duration=duration
                    )
                    self.assert_cpu_result(result, count)
                    description = json.loads(
                        subprocess.run(
                            [
                                "ffprobe",
                                "-v",
                                "error",
                                "-select_streams",
                                "v:0",
                                "-show_streams",
                                "-show_format",
                                "-show_packets",
                                "-count_frames",
                                "-of",
                                "json",
                                str(result.outputs[0]),
                            ],
                            check=True,
                            capture_output=True,
                            timeout=30,
                        ).stdout
                    )
                    stream = description["streams"][0]
                    self.assertEqual(int(stream["nb_read_frames"]), count)
                    self.assertEqual(int(stream["nb_frames"]), count)
                    self.assertEqual(float(stream["start_time"]), 0)
                    self.assertAlmostEqual(float(stream["duration"]), duration)
                    self.assertEqual(float(description["format"]["start_time"]), 0)
                    self.assertAlmostEqual(
                        float(description["format"]["duration"]), duration
                    )
                    packets = description["packets"]
                    self.assertEqual(len(packets), count)
                    previous_dts = None
                    for index, packet in enumerate(packets):
                        self.assertAlmostEqual(float(packet["pts_time"]), index / 10)
                        # Kvazaar can retain a valid negative decode delay even
                        # for two-frame clips. x265's historical uninitialized
                        # DTS is far outside this bounded, monotonic interval.
                        dts = float(packet["dts_time"])
                        self.assertGreaterEqual(dts, -0.2)
                        self.assertLessEqual(dts, float(packet["pts_time"]))
                        if previous_dts is not None:
                            self.assertAlmostEqual(dts - previous_dts, 0.1)
                        previous_dts = dts
                        self.assertAlmostEqual(float(packet["duration_time"]), 0.1)

    def audio_source(self, *, codec="aac", offset="0", silent=False):
        source = self.root / "audio-source.insv"
        arguments = ["-v", "error", "-nostdin", "-i", self.source, "-map", "0:v"]
        if not silent:
            arguments += [
                "-map",
                "0:a",
                "-c:a",
                codec,
                "-af",
                f"asetpts=PTS+{offset}/TB",
            ]
        run_tool(
            "ffmpeg",
            *arguments,
            "-c:v",
            "copy",
            "-movie_timescale",
            "1000000",
            "-f",
            "mp4",
            source,
        )
        with source.open("ab") as output:
            output.write(trailer(calibrated_metadata()))
        return source

    def assert_copied_audio(
        self, source, output, *, start=Fraction(0), end=Fraction(1)
    ):
        original = ffprobe(source)
        copied = ffprobe(output)
        source_audio = next(
            s for s in original["streams"] if s["codec_type"] == "audio"
        )
        output_audio = next(s for s in copied["streams"] if s["codec_type"] == "audio")
        self.assertEqual(
            [s["codec_type"] for s in copied["streams"]], ["video", "audio"]
        )
        for field in ("codec_name", "sample_rate", "channels", "extradata_hash"):
            self.assertEqual(output_audio[field], source_audio[field], field)
        source_base = Fraction(source_audio["time_base"])
        output_base = Fraction(output_audio["time_base"])
        video = next(s for s in original["streams"] if s["codec_type"] == "video")
        video_origin = int(video["start_pts"]) * Fraction(video["time_base"])
        expected = [
            packet
            for packet in original["packets"]
            if packet["stream_index"] == source_audio["index"]
            and int(packet["pts"]) * source_base >= video_origin + start
            and (int(packet["pts"]) + int(packet["duration"])) * source_base
            <= video_origin + end
        ]
        actual = [
            p for p in copied["packets"] if p["stream_index"] == output_audio["index"]
        ]
        self.assertGreater(len(expected), 0)
        self.assertEqual(len(actual), len(expected))
        for before, after in zip(expected, actual, strict=True):
            self.assertEqual(after["data_hash"], before["data_hash"])
            for field in ("pts", "dts"):
                self.assertLessEqual(
                    abs(
                        int(after[field]) * output_base
                        - (int(before[field]) * source_base - video_origin - start)
                    ),
                    output_base,
                    field,
                )
            self.assertEqual(
                int(after["duration"]) * output_base,
                int(before["duration"]) * source_base,
            )
        return actual, output_base

    def test_default_and_explicit_audio_copy_preserve_packets_through_both_apis(self):
        for export in (api.export_video, api.start_export_video):
            for explicit in (False, True):
                with self.subTest(api=export.__name__, explicit=explicit):
                    output = self.root / f"{export.__name__}-{explicit}.mp4"
                    result = export(
                        self.source,
                        output,
                        config=cpu_config(),
                        acceleration=api.MediaAcceleration.SOFTWARE,
                        **({"audio": api.AudioPolicy.COPY} if explicit else {}),
                    )
                    if isinstance(result, api.ExportJob):
                        result = result.wait()
                    self.assert_cpu_result(result, 10)
                    self.assert_copied_audio(self.source, output)

    def test_repeated_audio_cuts_preserve_submillisecond_offsets_and_packet_boundaries(
        self,
    ):
        source = self.audio_source(offset="0.05")
        for repetition in range(3):
            for start, end in (
                (Fraction(0), Fraction(1)),
                (Fraction(1, 5), Fraction(3, 5)),
            ):
                with self.subTest(repetition=repetition, start=start):
                    output = self.root / f"offset-{repetition}-{float(start)}.mp4"
                    result = api.export_video(
                        source,
                        output,
                        config=cpu_config(),
                        acceleration=api.MediaAcceleration.SOFTWARE,
                        start=float(start),
                        duration=float(end - start),
                    )
                    self.assert_cpu_result(result, int((end - start) * 10))
                    packets, base = self.assert_copied_audio(
                        source, output, start=start, end=end
                    )
                    first = int(packets[0]["pts"]) * base
                    self.assertGreater(first, 0)
                    self.assertNotEqual((first * 1000).denominator, 1)

    def test_alac_audio_copy_preserves_lossless_packets(self):
        source = self.audio_source(codec="alac")
        output = self.root / "alac.mp4"
        result = api.export_video(
            source,
            output,
            config=cpu_config(),
            acceleration=api.MediaAcceleration.SOFTWARE,
        )
        self.assert_cpu_result(result, 10)
        self.assert_copied_audio(source, output)

    def test_unsupported_audio_fails_without_output_and_drop_succeeds(self):
        source = self.audio_source(codec="ac3")
        output = self.root / "unsupported.mp4"
        for export in (api.export_video, api.start_export_video):
            with self.subTest(api=export.__name__):
                with self.assertRaisesRegex(api.MissingCapabilityError, "ac3 audio"):
                    result = export(source, output, config=cpu_config())
                    if isinstance(result, api.ExportJob):
                        result.wait()
                self.assertFalse(output.exists())
                self.assertEqual(list(self.root.iterdir()), [source])
        result = api.export_video(
            source,
            output,
            config=cpu_config(),
            audio=api.AudioPolicy.DROP,
            acceleration=api.MediaAcceleration.SOFTWARE,
        )
        self.assert_cpu_result(result, 10)
        self.assertEqual(
            [s["codec_type"] for s in ffprobe(output)["streams"]], ["video"]
        )

    def test_silent_audio_copy_warns_and_exports_video(self):
        source = self.audio_source(silent=True)
        output = self.root / "silent.mp4"
        job = api.start_export_video(
            source,
            output,
            config=cpu_config(),
            acceleration=api.MediaAcceleration.SOFTWARE,
        )
        self.assert_cpu_result(job.wait(), 10)
        self.assertTrue(any("no audio" in warning for warning in job.take_warnings()))
        self.assertEqual(
            [s["codec_type"] for s in ffprobe(output)["streams"]], ["video"]
        )

    def test_existing_video_is_never_overwritten(self):
        output = self.root / "video.mp4"
        output.write_bytes(b"keep")
        with self.assertRaises(api.InvalidMediaError):
            self.export_video()
        self.assertEqual(output.read_bytes(), b"keep")
        self.assertEqual(list(self.root.iterdir()), [output])

    def test_out_of_bounds_or_empty_intervals_leave_no_output(self):
        for index, options in enumerate(
            ({"start": 1}, {"start": 2}, {"start": 0.91, "duration": 0.01})
        ):
            with self.subTest(options=options):
                with self.assertRaises(api.InvalidMediaError):
                    self.export_video(f"{index}.mp4", **options)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_odd_height_is_rejected_for_yuv420_video(self):
        with self.assertRaises(api.InvalidMediaError):
            self.export_video(config=cpu_config(width=126, height=63))
        self.assertEqual(list(self.root.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
