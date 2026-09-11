"""Public extraction API tests; run against an installed native extension."""

import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch, sentinel

import insta360_rs
from media_fixtures import (
    assert_readonly,
    ffprobe,
    generate_media,
    metadata_record,
    read_all,
    v2_tail,
    v3_tail,
)


class ExtractWrapperTests(unittest.TestCase):
    def test_normalizes_single_paths_and_explicit_pairs(self):
        for inputs, expected in [
            ("single.insv", ["single.insv"]),
            (Path("single.insv"), ["single.insv"]),
            ([Path("single.insv")], ["single.insv"]),
            (
                (Path("VID_00_001.insv"), "VID_10_001.insv"),
                ["VID_00_001.insv", "VID_10_001.insv"],
            ),
        ]:
            with self.subTest(inputs=inputs):
                with patch.object(
                    insta360_rs, "_extract", return_value=sentinel.report
                ) as native:
                    result = insta360_rs.extract(inputs, Path("extracted"))
                self.assertIs(result, sentinel.report)
                native.assert_called_once_with(expected, "extracted")

    def test_empty_input_raises_typed_error_without_creating_output(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "extracted"
            with self.assertRaises(insta360_rs.InvalidMediaError):
                insta360_rs.extract([], output)
            self.assertFalse(output.exists())

    def test_missing_input_raises_io_error_without_creating_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "extracted"
            with self.assertRaises(insta360_rs.Insta360IOError):
                insta360_rs.extract(root / "missing.insv", output)
            self.assertFalse(output.exists())


class ExtractIntegrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture_directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.fixture_directory.cleanup)
        media = Path(cls.fixture_directory.name) / "media.mp4"
        cls.media = generate_media(media, timecode=True)
        cls.metadata = b'{"camera":"Unknown Camera","custom":{"preserved":true}}'
        cls.recording = cls.media + v2_tail(cls.metadata)
        cls.reference = ffprobe(media)
        cls.stream_count = len(cls.reference["streams"])

    def test_extracts_v2_video_audio_and_metadata_without_calibration(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "recording.insv"
            source.write_bytes(self.recording)
            output = root / "extracted"

            report = insta360_rs.extract(source, output)

            self.assertIsInstance(report, insta360_rs.ExtractionReport)
            self.assertTrue(report.output_dir.is_absolute())
            self.assertTrue(report.output_dir.samefile(output))
            self.assertIsInstance(report.manifest_path, Path)
            self.assertEqual(report.input_count, 1)
            self.assertEqual(report.stream_count, self.stream_count)
            self.assertGreaterEqual(report.record_count, 1)
            self.assertTrue(report.files)
            self.assertTrue(all(isinstance(path, Path) for path in report.files))
            self.assertTrue(all(path.is_file() for path in report.files))
            self.assertIsInstance(json.loads(report.manifest_path.read_text()), dict)
            self.assertTrue(
                any(path.read_bytes() == self.metadata for path in report.files)
            )
            self.assertTrue(
                all(isinstance(warning, str) for warning in report.warnings)
            )
            with self.assertRaises(AttributeError):
                report.stream_count = 0

    def test_discovers_sibling_and_accepts_reversed_explicit_pair(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary = root / "VID_20260101_120000_00_001.insv"
            secondary = root / "VID_20260101_120000_10_001.insv"
            primary.write_bytes(self.recording)
            secondary.write_bytes(self.recording)
            for index, inputs in enumerate((secondary, [secondary, primary])):
                with self.subTest(inputs=inputs):
                    report = insta360_rs.extract(inputs, root / f"extracted-{index}")
                    self.assertEqual(report.input_count, 2)
                    self.assertEqual(report.stream_count, self.stream_count * 2)

    def test_rejects_nonempty_target_without_changing_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "recording.insv"
            source.write_bytes(self.recording)
            output = root / "extracted"
            output.mkdir()
            existing = output / "keep.txt"
            existing.write_text("keep this file")

            with self.assertRaises(insta360_rs.InvalidMediaError):
                insta360_rs.extract(source, output)

            self.assertEqual(existing.read_text(), "keep this file")
            self.assertEqual(list(output.iterdir()), [existing])

    def test_v2_manifest_preserves_json_raw_metadata_and_original_input(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "記録 with spaces.insv"
            source.write_bytes(self.recording)
            output = root / "extracted nested" / "components"
            report = insta360_rs.extract(source, output)
            manifest = json.loads(report.manifest_path.read_text())
            self.assertEqual(manifest["schema_version"], 1)
            self.assertEqual(manifest["warnings"], report.warnings)
            self.assertEqual(report.manifest_path, report.output_dir / "manifest.json")
            self.assertEqual(report.input_count, len(manifest["inputs"]))
            self.assertEqual(report.record_count, 1)
            entry = manifest["inputs"][0]
            source_path = Path(entry["source"])
            self.assertTrue(source_path.is_absolute())
            self.assertTrue(source_path.samefile(source))
            self.assertEqual(entry["size"], len(self.recording))
            base = report.output_dir / entry["directory"]
            trailer = entry["container"]["trailer"]
            self.assertEqual(trailer["version"], 2)
            self.assertTrue(trailer["valid"])
            self.assertEqual(
                (base / trailer["raw_path"]).read_bytes(), v2_tail(self.metadata)
            )
            record = entry["container"]["records"][0]
            self.assertEqual(record["region"], "metadata")
            self.assertEqual(record["encoding"], "json")
            self.assertEqual((base / record["raw_path"]).read_bytes(), self.metadata)
            self.assertEqual(
                json.loads((base / "metadata/v2.json").read_text()),
                json.loads(self.metadata),
            )
            self.assertEqual(source.read_bytes(), self.recording)
            self.assertEqual(
                set(report.files),
                {path for path in report.output_dir.rglob("*") if path.is_file()},
            )
            self.assertEqual(report.files, sorted(report.files))
            self.assertEqual(len(report.files), len(set(report.files)))
            assert_readonly(
                self,
                report,
                [
                    "output_dir",
                    "manifest_path",
                    "input_count",
                    "stream_count",
                    "record_count",
                    "files",
                    "warnings",
                ],
            )
            files, warnings = report.files, report.warnings
            files.clear()
            warnings.append("caller annotation")
            self.assertTrue(report.files)
            self.assertNotIn("caller annotation", report.warnings)

    def test_v3_preserves_duplicate_opaque_record_ids_and_exact_tail(self):
        records = [
            (1, 1, metadata_record("Unknown Camera", populated=False)),
            (238, 7, b"opaque first\x00\xff"),
            (238, 9, b"opaque second"),
        ]
        tail = v3_tail(records)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "recording.insv"
            source.write_bytes(self.media + tail)
            report = insta360_rs.extract(source, root / "extracted")
            manifest = json.loads(report.manifest_path.read_text())
            container = manifest["inputs"][0]["container"]
            self.assertEqual(report.record_count, 3)
            self.assertEqual(container["trailer"]["version"], 3)
            self.assertTrue(container["trailer"]["valid"])
            base = report.output_dir / "input-00"
            self.assertEqual((base / "extra-info/tail.bin").read_bytes(), tail)
            extracted = container["records"]
            self.assertEqual([record["id"] for record in extracted], [1, 238, 238])
            for record, (record_id, encoding, data) in zip(extracted, records):
                self.assertEqual(record["id"], record_id)
                self.assertEqual(record["format"], encoding)
                self.assertEqual(record["size"], len(data))
                self.assertEqual((base / record["raw_path"]).read_bytes(), data)

    def test_packet_manifest_timing_boundaries_side_data_and_remuxed_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "recording.insv"
            source.write_bytes(self.recording)
            original = insta360_rs.open_media(source)
            report = insta360_rs.extract(source, root / "extracted")
            entry = json.loads(report.manifest_path.read_text())["inputs"][0]
            streams = entry["media"]["streams"]
            base = report.output_dir / entry["directory"]
            self.assertEqual(len(streams), report.stream_count)
            self.assertIn("data", [stream["type"] for stream in streams])
            sequences = []
            side_data_count = 0
            for description, stream in zip(streams, original.streams):
                with self.subTest(stream=description["index"]):
                    self.assertEqual(description["index"], stream.info.stream_index)
                    self.assertEqual(description["type"], stream.info.kind)
                    self.assertEqual(description["codec"]["name"], stream.info.codec)
                    self.assertEqual(
                        (base / description["extradata"]).read_bytes(),
                        stream.info.codec_extradata,
                    )
                    payload = (base / description["packet_payload"]).read_bytes()
                    rows = [
                        json.loads(line)
                        for line in (base / description["packet_index"])
                        .read_text()
                        .splitlines()
                    ]
                    packets = read_all(stream.open_packets(), "read_packet")
                    expected = [
                        packet
                        for packet in self.reference["packets"]
                        if packet["stream_index"] == description["index"]
                    ]
                    self.assertEqual(len(rows), len(packets))
                    self.assertEqual(description["packet_count"], len(rows))
                    self.assertEqual(description["packet_bytes"], len(payload))
                    self.assertEqual(
                        payload, b"".join(packet.data for packet in packets)
                    )
                    offset = 0
                    for number, (row, packet, reference) in enumerate(
                        zip(rows, packets, expected)
                    ):
                        self.assertEqual(row["packet"], number)
                        self.assertEqual(row["offset"], offset)
                        self.assertEqual(row["size"], len(packet.data))
                        self.assertEqual(
                            "SHA256:"
                            + hashlib.sha256(
                                payload[offset : offset + row["size"]]
                            ).hexdigest(),
                            reference["data_hash"],
                        )
                        offset += row["size"]
                        sequences.append(row["sequence"])
                        for key in [
                            "pts",
                            "dts",
                            "duration",
                            "flags",
                            "key_frame",
                            "corrupt",
                            "source_position",
                        ]:
                            self.assertEqual(row[key], getattr(packet, key))
                        self.assertEqual(
                            row["time_base"],
                            {
                                "numerator": packet.time_base[0],
                                "denominator": packet.time_base[1],
                            },
                        )
                        self.assertEqual(len(row["side_data"]), len(packet.side_data))
                        for side, native_side in zip(
                            row["side_data"], packet.side_data
                        ):
                            side_payload = (base / side["file"]).read_bytes()
                            self.assertEqual(side["type"], native_side.kind)
                            self.assertEqual(
                                side_payload[
                                    side["offset"] : side["offset"] + side["size"]
                                ],
                                native_side.data,
                            )
                            side_data_count += 1
                    self.assertEqual(offset, len(payload))
                    if description["type"] in ["video", "audio"]:
                        remuxed = ffprobe(base / description["media_file"])
                        self.assertEqual(len(remuxed["streams"]), 1)
                        self.assertEqual(
                            [packet["data_hash"] for packet in remuxed["packets"]],
                            [packet["data_hash"] for packet in expected],
                        )
            self.assertGreater(side_data_count, 0)
            self.assertEqual(sorted(sequences), list(range(len(sequences))))

    def test_plain_mp4_content_extracts_with_a_missing_trailer_warning(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "plain.insv"
            source.write_bytes(self.media)
            report = insta360_rs.extract(source, root / "output")
            self.assertEqual(report.record_count, 0)
            self.assertEqual(report.stream_count, self.stream_count)
            self.assertTrue(
                any("No recognized ExtraInfo" in warning for warning in report.warnings)
            )
            container = json.loads(report.manifest_path.read_text())["inputs"][0][
                "container"
            ]
            self.assertIsNone(container["trailer"])
            self.assertEqual(container["records"], [])

    def test_existing_empty_directory_is_accepted(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "recording.insv"
            source.write_bytes(self.recording)
            output = root / "extracted"
            output.mkdir()
            self.assertTrue(insta360_rs.extract(source, output).manifest_path.is_file())

    def test_symlink_target_is_rejected_without_touching_link_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "recording.insv"
            source.write_bytes(self.recording)
            for exists in [True, False]:
                destination = root / f"destination-{exists}"
                if exists:
                    destination.mkdir()
                output = root / f"link-{exists}"
                output.symlink_to(destination, target_is_directory=True)
                with (
                    self.subTest(destination_exists=exists),
                    self.assertRaises(insta360_rs.InvalidMediaError),
                ):
                    insta360_rs.extract(source, output)
                self.assertTrue(output.is_symlink())
                self.assertEqual(destination.exists(), exists)
                if exists:
                    self.assertEqual(list(destination.iterdir()), [])

    def test_regular_file_and_source_itself_are_rejected_as_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "recording.insv"
            source.write_bytes(self.recording)
            existing = root / "keep.txt"
            existing.write_bytes(b"keep me")
            for output in [existing, source]:
                with (
                    self.subTest(output=output),
                    self.assertRaises(insta360_rs.InvalidMediaError),
                ):
                    insta360_rs.extract(source, output)
            self.assertEqual(existing.read_bytes(), b"keep me")
            self.assertEqual(source.read_bytes(), self.recording)

    def test_failed_second_input_cleans_staging_and_never_publishes_partial_results(
        self,
    ):
        for existing_output in [False, True]:
            with (
                self.subTest(existing_output=existing_output),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                primary = root / "VID_20260101_120000_00_001.insv"
                secondary = root / "VID_20260101_120000_10_001.insv"
                primary.write_bytes(self.recording)
                secondary.write_bytes(b"invalid media")
                output = root / "output"
                if existing_output:
                    output.mkdir()
                before = set(root.iterdir())
                with self.assertRaises(insta360_rs.Insta360Error):
                    insta360_rs.extract([secondary, primary], output)
                self.assertEqual(set(root.iterdir()), before)
                self.assertEqual(output.exists(), existing_output)
                if existing_output:
                    self.assertEqual(list(output.iterdir()), [])
                self.assertEqual(primary.read_bytes(), self.recording)


if __name__ == "__main__":
    unittest.main()
