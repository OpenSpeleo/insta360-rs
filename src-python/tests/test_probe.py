"""Native parser, Python-owned result values and recording identity contracts."""

import gc
import struct
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch, sentinel

import insta360_rs
from export_fixtures import calibrated_metadata
from media_fixtures import assert_readonly, synthetic_recording, varint


class ProbeTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.path = self.root / "recording.insv"
        self.path.write_bytes(synthetic_recording())

    def test_wrapper_normalizes_paths_and_explicit_pairs(self):
        for inputs, expected in [
            ("single.insv", ["single.insv"]),
            (Path("single.insv"), ["single.insv"]),
            ((Path("a.insv"), "b.insv"), ["a.insv", "b.insv"]),
        ]:
            with self.subTest(inputs=inputs):
                with patch.object(
                    insta360_rs, "_probe", return_value=sentinel.info
                ) as native:
                    self.assertIs(insta360_rs.probe(inputs), sentinel.info)
                native.assert_called_once_with(expected)

    def test_all_media_track_and_trailer_properties(self):
        info = insta360_rs.probe(self.path)
        self.assertIsInstance(info, insta360_rs.MediaInfo)
        self.assertEqual(info.inputs, [self.path])
        self.assertTrue(all(isinstance(path, Path) for path in info.inputs))
        self.assertEqual(info.camera, "X5")
        self.assertEqual(info.camera_name, "Insta360 X5")
        self.assertEqual(info.serial, "TEST-X5-PYTHON")
        self.assertEqual(info.firmware, "v1.2.3_test")
        self.assertAlmostEqual(info.duration_seconds, 10.01)
        self.assertAlmostEqual(info.fps, 30000 / 1001)
        self.assertEqual(info.offset_versions, [1, 2, 3, 6])
        self.assertEqual(info.optical_profiles, ["InvisibleDiveWater", "bare"])
        self.assertEqual(info.gyro_sample_count, 2)
        self.assertEqual(info.exposure_sample_count, 2)
        self.assertEqual(len(info.video_tracks), 2)
        for index, track in enumerate(info.video_tracks):
            self.assertIsInstance(track, insta360_rs.VideoTrackInfo)
            self.assertEqual(
                (track.index, track.width, track.height, track.codec),
                (index, 2880, 2880, "hvc1"),
            )
        trailer = info.trailer
        self.assertIsInstance(trailer, insta360_rs.TrailerInfo)
        self.assertEqual(trailer.version, 3)
        self.assertEqual(trailer.record_count, 3)
        self.assertGreater(trailer.offset, 0)
        self.assertEqual(trailer.offset + trailer.size, self.path.stat().st_size)
        self.assertEqual(
            self.path.read_bytes()[trailer.offset + 4 : trailer.offset + 8], b"inst"
        )

    def test_native_values_are_readonly_and_collections_are_owned(self):
        info = insta360_rs.probe(self.path)
        assert_readonly(
            self,
            info,
            [
                "inputs",
                "camera",
                "camera_name",
                "serial",
                "firmware",
                "duration_seconds",
                "fps",
                "video_tracks",
                "offset_versions",
                "optical_profiles",
                "optics",
                "gyro_sample_count",
                "exposure_sample_count",
                "trailer",
            ],
        )
        track, trailer = info.video_tracks[0], info.trailer
        assert_readonly(self, track, ["index", "width", "height", "codec"])
        assert_readonly(self, trailer, ["offset", "size", "version", "record_count"])
        for attribute in [
            "inputs",
            "video_tracks",
            "offset_versions",
            "optical_profiles",
        ]:
            values = getattr(info, attribute)
            length = len(values)
            values.clear()
            self.assertEqual(len(getattr(info, attribute)), length)
        self.path.unlink()
        del info
        gc.collect()
        self.assertEqual(track.width, 2880)
        self.assertEqual(trailer.record_count, 3)

    def test_known_camera_names_and_aliases_have_stable_identifiers(self):
        for name, expected in [
            ("Insta360 ONE", "ONE"),
            ("Insta360 ONE R", "ONE R"),
            ("Insta360 ONE RS", "ONE RS"),
            ("Insta360 X4 Air", "X4 Air"),
            ("Insta360 ONE X", "X1"),
            ("One2", "X1"),
            ("Insta360 ONE X2", "X2"),
            ("OneXS", "X2"),
            ("Insta360 X3", "X3"),
            ("Insta360 X4", "X4"),
            ("Insta360 A3", "X5"),
            ("Insta360 C9", "X6"),
            ("insta360 x5", "X5"),
            ("Unknown Future Camera", "Unknown Future Camera"),
        ]:
            with self.subTest(camera=name):
                self.path.write_bytes(synthetic_recording(camera=name))
                info = insta360_rs.probe(self.path)
                self.assertEqual(info.camera, expected)
                self.assertEqual(info.camera_name, name)

    def test_absent_optional_metadata_uses_none_and_empty_collections(self):
        self.path.write_bytes(synthetic_recording(camera=None, populated=False))
        info = insta360_rs.probe(self.path)
        self.assertEqual(info.camera, "unspecified")
        self.assertIsNone(info.camera_name)
        self.assertIsNone(info.serial)
        self.assertIsNone(info.firmware)
        self.assertEqual(info.offset_versions, [])
        self.assertEqual(info.optical_profiles, [])
        self.assertEqual(info.gyro_sample_count, 0)
        self.assertEqual(info.exposure_sample_count, 0)
        self.assertEqual(info.trailer.record_count, 1)

    def test_optical_inspection_is_bounded_typed_and_read_only(self):
        for state, housing, environment in [
            (None, insta360_rs.Housing.NONE, insta360_rs.Environment.AIR),
            (10, insta360_rs.Housing.DIVE_CASE_PRO, insta360_rs.Environment.UNDERWATER),
            (11, insta360_rs.Housing.DIVE_CASE_PRO, insta360_rs.Environment.AIR),
        ]:
            with self.subTest(state=state):
                metadata = calibrated_metadata()
                if state is not None:
                    metadata += varint(68 << 3) + varint(state)
                self.path.write_bytes(synthetic_recording(metadata=metadata))
                report = insta360_rs.probe(self.path).optics
                self.assertIsInstance(report, insta360_rs.OpticalInspection)
                self.assertEqual(report.encoded_lens_id, 113)
                self.assertEqual(
                    report.evidence,
                    "encoded_lens" if state is None else "recorded_state",
                )
                self.assertIsNone(report.ambiguity)
                selection = report.detected
                self.assertIsInstance(selection, insta360_rs.OpticalSelection)
                self.assertEqual(selection.housing, housing)
                self.assertEqual(selection.environment, environment)
                self.assertEqual(
                    selection.lens_accessory, insta360_rs.LensAccessory.NONE
                )
                self.assertEqual(
                    selection.mounting_accessory, insta360_rs.MountingAccessory.NONE
                )
                assert_readonly(
                    self,
                    report,
                    ["detected", "evidence", "encoded_lens_id", "ambiguity"],
                )
                assert_readonly(
                    self,
                    selection,
                    ["housing", "environment", "lens_accessory", "mounting_accessory"],
                )
        self.path.write_bytes(synthetic_recording(camera=None, populated=False))
        report = insta360_rs.probe(self.path).optics
        self.assertIsNone(report.detected)
        self.assertIsNone(report.encoded_lens_id)
        self.assertIsInstance(report.ambiguity, str)

    def test_reads_legacy_record_footers_without_an_index(self):
        self.path.write_bytes(synthetic_recording(indexed=False))
        info = insta360_rs.probe(self.path)
        self.assertEqual(info.camera, "X5")
        self.assertEqual(info.trailer.record_count, 3)
        self.assertEqual(info.gyro_sample_count, 2)

    def test_paths_with_unicode_spaces_and_uppercase_extension(self):
        path = self.root / "記録 café.INsV"
        self.path.rename(path)
        self.assertEqual(insta360_rs.probe(path).inputs, [path])

    def test_primary_alone_is_valid_and_secondary_requires_its_primary(self):
        primary = self.root / "VID_20260101_120000_00_001.insv"
        secondary = self.root / "VID_20260101_120000_10_001.insv"
        self.path.rename(primary)
        self.assertEqual(insta360_rs.probe(primary).inputs, [primary])
        primary.rename(secondary)
        for inputs in [secondary, [secondary]]:
            with (
                self.subTest(inputs=inputs),
                self.assertRaises(insta360_rs.InvalidMediaError),
            ):
                insta360_rs.probe(inputs)

    def test_pair_discovery_order_and_flattened_track_indices(self):
        primary = self.root / "VID_20260101_120000_00_001.insv"
        secondary = self.root / "VID_20260101_120000_10_001.insv"
        primary.write_bytes(self.path.read_bytes())
        secondary.write_bytes(self.path.read_bytes())
        for inputs in [primary, secondary, [primary], [secondary, primary]]:
            with self.subTest(inputs=inputs):
                info = insta360_rs.probe(inputs)
                self.assertEqual(info.inputs, [primary, secondary])
                self.assertEqual(
                    [track.index for track in info.video_tracks], list(range(4))
                )
                self.assertEqual(
                    info.gyro_sample_count, 2
                )  # Primary metadata, not a sum.

    def test_rejects_pairs_with_different_camera_or_duration(self):
        primary = self.root / "VID_20260101_120000_00_001.insv"
        secondary = self.root / "VID_20260101_120000_10_001.insv"
        primary.write_bytes(self.path.read_bytes())
        for kwargs in [{"camera": "Insta360 X4"}, {"sample_count": 600}]:
            with self.subTest(kwargs=kwargs):
                secondary.write_bytes(synthetic_recording(**kwargs))
                with self.assertRaises(insta360_rs.InvalidMediaError):
                    insta360_rs.probe([secondary, primary])

    def test_invalid_input_count_and_pair_names_are_typed_errors(self):
        other = self.root / "another.insv"
        other.write_bytes(self.path.read_bytes())
        for inputs in [[], [self.path] * 3, [self.path, self.path], [self.path, other]]:
            with (
                self.subTest(inputs=inputs),
                self.assertRaises(insta360_rs.InvalidMediaError),
            ):
                insta360_rs.probe(inputs)

    def test_missing_paths_and_directories_are_io_errors(self):
        for path in [self.root / "missing.insv", self.root]:
            with (
                self.subTest(path=path),
                self.assertRaises(insta360_rs.Insta360IOError),
            ):
                insta360_rs.probe(path)

    def test_wrong_extension_is_rejected_before_parsing(self):
        path = self.root / "recording.mp4"
        self.path.rename(path)
        with self.assertRaises(insta360_rs.InvalidMediaError):
            insta360_rs.probe(path)

    def test_rejects_missing_magic_truncated_or_invalid_index_without_panicking(self):
        original = self.path.read_bytes()
        invalid_size = bytearray(original)
        invalid_size[-40:-36] = struct.pack("<I", 0xFFFFFFFF)
        invalid_index = bytearray(original)
        invalid_index[-76:-72] = struct.pack("<I", 11)
        for contents in [
            b"",
            b"not an INSV file",
            original[:-1],
            original[:-32] + bytes(32),
            invalid_size,
            invalid_index,
        ]:
            with self.subTest(size=len(contents)):
                self.path.write_bytes(contents)
                with self.assertRaises(insta360_rs.InvalidMediaError):
                    insta360_rs.probe(self.path)

    def test_rejects_a_valid_container_without_video_tracks(self):
        self.path.write_bytes(synthetic_recording(tracks=0))
        with self.assertRaises(insta360_rs.InvalidMediaError):
            insta360_rs.probe(self.path)


if __name__ == "__main__":
    unittest.main()
