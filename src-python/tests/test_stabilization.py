"""Installed-extension tests for exposure-mapped X5 exports and seeking."""

import struct
import unittest

import insta360_rs as api
from export_fixtures import (
    MEDIA_TOOLS_AVAILABLE,
    ExportFixtureMixin,
    calibrated_metadata,
    cpu_config,
    media_description,
    trailer,
    varint,
)


@unittest.skipUnless(
    MEDIA_TOOLS_AVAILABLE, "ffmpeg and ffprobe fixture tools unavailable"
)
class StabilizationExports(ExportFixtureMixin, unittest.TestCase):
    def moving_source(self, metadata=None):
        source = self.root / "moving.insv"
        source.write_bytes(
            (self.fixture_root / "dual-lens.mp4").read_bytes()
            + trailer(
                calibrated_metadata() if metadata is None else metadata,
                yaw_degrees_per_second=90,
                exposure_step_us=101_000,  # Deliberately differs from encoded 10 fps.
            )
        )
        return source

    def config(self, mode):
        return cpu_config(
            stabilization=mode,
            rolling_shutter=api.RollingShutterCorrection.REQUIRED,
        )

    def test_seek_and_full_decode_keep_identical_motion_anchor(self):
        source = self.moving_source()
        first_outputs = []
        for mode in (api.Stabilization.FLOW_STATE, api.Stabilization.DIRECTION_LOCK):
            with self.subTest(mode=mode):
                config = self.config(mode)
                whole = api.export_frames(
                    source,
                    self.root / f"all-{mode}",
                    indices=list(range(10)),
                    config=config,
                )
                selected = api.export_frames(
                    source, self.root / f"seek-{mode}", timestamps=[0.7], config=config
                )
                self.assertEqual(
                    whole.outputs[7].read_bytes(), selected.outputs[0].read_bytes()
                )
                first_outputs.append(whole.outputs[0].read_bytes())
        self.assertNotEqual(first_outputs[0], first_outputs[1])

    def test_both_modes_finalize_trimmed_video_and_report_motion(self):
        source = self.moving_source()
        for mode in (api.Stabilization.FLOW_STATE, api.Stabilization.DIRECTION_LOCK):
            with self.subTest(mode=mode):
                job = api.start_export_video(
                    source,
                    self.root / f"{mode}.mp4",
                    start=0.2,
                    duration=0.3,
                    audio=api.AudioPolicy.DROP,
                    acceleration=api.MediaAcceleration.SOFTWARE,
                    config=self.config(mode),
                )
                result = job.wait()
                self.assert_cpu_result(result, 3)
                stream = media_description(result.outputs[0])["streams"][0]
                self.assertEqual(stream["codec_name"], "hevc")
                self.assertEqual(int(stream["nb_read_frames"]), 3)
                self.assertIsInstance(job.stabilization(), str)
                self.assertEqual(job.take_warnings(), [])

    def test_missing_readout_is_reported_for_auto_and_rejected_when_required(self):
        metadata = calibrated_metadata().replace(
            varint(25 << 3 | 1) + struct.pack("<d", 10.0), b""
        )
        source = self.moving_source(metadata)
        job = api.start_export_frames(
            source,
            self.root / "auto",
            indices=[0],
            config=cpu_config(stabilization=api.Stabilization.DIRECTION_LOCK),
        )
        self.assert_cpu_result(job.wait(), 1)
        self.assertTrue(job.take_warnings())
        with self.assertRaises(api.MissingCalibrationError):
            api.export_frames(
                source,
                self.root / "required",
                indices=[0],
                config=self.config(api.Stabilization.DIRECTION_LOCK),
            )
        self.assertFalse((self.root / "required").exists())

    def test_off_is_a_bypass_and_rejects_required_readout(self):
        with self.assertRaises(api.InvalidMediaError):
            api.export_frames(
                self.source,
                self.root / "contradictory",
                indices=[0],
                config=self.config(api.Stabilization.OFF),
            )
        self.assertFalse((self.root / "contradictory").exists())


if __name__ == "__main__":
    unittest.main()
