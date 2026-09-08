"""Direct stream-object API checks against the installed native extension."""

import gc
import hashlib
import shutil
import struct
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch, sentinel

import insta360_rs
from media_fixtures import (
    assert_readonly,
    ffprobe,
    generate_media,
    read_all,
    run_tool,
)


class StreamWrapperTests(unittest.TestCase):
    def test_normalizes_paths_and_pairs_without_an_output_argument(self):
        for inputs, expected in [
            ("single.insv", ["single.insv"]),
            (Path("single.insv"), ["single.insv"]),
            (
                [Path("VID_00_001.insv"), "VID_10_001.insv"],
                ["VID_00_001.insv", "VID_10_001.insv"],
            ),
        ]:
            with self.subTest(inputs=inputs):
                with patch.object(
                    insta360_rs, "_open_media", return_value=sentinel.source
                ) as native:
                    source = insta360_rs.open_media(inputs)
                self.assertIs(source, sentinel.source)
                native.assert_called_once_with(expected)

    def test_invalid_input_returns_typed_errors(self):
        with self.assertRaises(insta360_rs.InvalidMediaError):
            insta360_rs.open_media([])
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(insta360_rs.Insta360IOError):
                insta360_rs.open_media(Path(directory) / "missing.insv")
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_non_media_and_invalid_pair_inputs_raise_typed_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first, second = root / "first.insv", root / "second.insv"
            first.write_bytes(b"not a media container")
            second.write_bytes(b"not a media container")
            with self.assertRaises(insta360_rs.MediaProcessingError):
                insta360_rs.open_media(first)
            for inputs in [[first, second], [first, first], [first] * 3]:
                with (
                    self.subTest(inputs=inputs),
                    self.assertRaises(insta360_rs.InvalidMediaError),
                ):
                    insta360_rs.open_media(inputs)
            wrong_extension = root / "recording.mp4"
            first.rename(wrong_extension)
            with self.assertRaises(insta360_rs.InvalidMediaError):
                insta360_rs.open_media(wrong_extension)
            with self.assertRaises(insta360_rs.Insta360IOError):
                insta360_rs.open_media(root)


class StreamIntegrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture_directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.fixture_directory.cleanup)
        cls.root = Path(cls.fixture_directory.name)
        cls.recording = cls.root / "recording.insv"
        cls.original = generate_media(cls.recording)
        cls.reference = ffprobe(cls.recording)

    def test_enumerates_safe_stream_descriptors_without_writing_files(self):
        before = {path.name: path.stat().st_size for path in self.root.iterdir()}
        source = insta360_rs.open_media(self.recording)
        self.assertIsInstance(source, insta360_rs.MediaSource)
        self.assertEqual(len(source.streams), 3)
        self.assertEqual(len(source.video_streams), 2)
        self.assertEqual(
            [stream.info.kind for stream in source.streams], ["video", "video", "audio"]
        )
        info = source.video_streams[0].info
        self.assertIsInstance(info, insta360_rs.StreamInfo)
        self.assertEqual((info.width, info.height), (32, 16))
        self.assertEqual(info.codec, "mpeg4")
        self.assertIsInstance(info.codec_extradata, bytes)
        self.assertGreater(len(info.codec_extradata), 0)
        self.assertEqual(len(info.time_base), 2)
        self.assertGreater(info.time_base[1], 0)
        with self.assertRaises(AttributeError):
            info.width = 12
        self.assertEqual(
            before, {path.name: path.stat().st_size for path in self.root.iterdir()}
        )

    def test_decodes_and_seeks_each_video_stream_in_memory(self):
        before = set(self.root.iterdir())
        source = insta360_rs.open_media(self.recording)
        red, blue = [stream.open_video() for stream in source.video_streams]
        first = red.read_frame()
        other = blue.read_frame()
        self.assertIsInstance(first, insta360_rs.DecodedVideoFrame)
        self.assertIsInstance(first.data, bytes)
        self.assertEqual(len(first.data), first.width * first.height * 3)
        self.assertEqual((first.width, first.height), (32, 16))
        self.assertGreater(first.data[0], first.data[2])
        self.assertGreater(other.data[2], other.data[0])
        self.assertEqual(first.timestamp_seconds, 0.0)

        selected = red.frame_at(0.45)
        self.assertGreaterEqual(selected.timestamp_seconds, 0.45)
        self.assertLess(selected.timestamp_seconds, 0.61)
        red.seek(0.0)
        reset = red.read_frame()
        self.assertEqual(reset.data, first.data)
        self.assertEqual(reset.timestamp_seconds, first.timestamp_seconds)
        frames = [reset]
        while (frame := red.read_frame()) is not None:
            frames.append(frame)
        self.assertEqual(len(frames), 10)
        self.assertIsNone(red.read_frame())
        self.assertIsNone(red.frame_at(2.0))
        red.seek(0.0)
        self.assertIsNotNone(red.read_frame())
        self.assertEqual(before, set(self.root.iterdir()))

    def test_encoded_packet_access_preserves_bytes_timing_and_independent_positions(
        self,
    ):
        source = insta360_rs.open_media(self.recording)
        stream = source.video_streams[0]
        first_reader = stream.open_packets()
        second_reader = stream.open_packets()
        first = first_reader.read_packet()
        same = second_reader.read_packet()
        self.assertIsInstance(first, insta360_rs.EncodedPacket)
        self.assertIsInstance(first.data, bytes)
        self.assertTrue(first.data)
        self.assertEqual(first.data, same.data)
        self.assertEqual(first.pts, same.pts)
        self.assertEqual(first.dts, same.dts)
        self.assertGreater(first.duration, 0)
        self.assertTrue(first.key_frame)
        self.assertIsInstance(first.flags, int)
        packets = [first]
        while (packet := first_reader.read_packet()) is not None:
            packets.append(packet)
        self.assertEqual(len(packets), 10)
        self.assertIsNone(first_reader.read_packet())
        first_reader.seek(0.0)
        self.assertEqual(first_reader.read_packet().data, first.data)
        audio = next(stream for stream in source.streams if stream.info.kind == "audio")
        self.assertIsInstance(audio.open_packets().read_packet().data, bytes)
        with self.assertRaises(insta360_rs.InvalidMediaError):
            audio.open_video()

    def test_reader_serializes_native_access_across_python_threads(self):
        reader = insta360_rs.open_media(self.recording).video_streams[0].open_video()
        with ThreadPoolExecutor(max_workers=4) as workers:
            frames = list(workers.map(lambda _: reader.read_frame(), range(12)))
        frames = [frame for frame in frames if frame is not None]
        self.assertEqual(len(frames), 10)
        self.assertEqual(len({frame.timestamp_seconds for frame in frames}), 10)

    def test_every_stream_property_matches_independent_ffprobe(self):
        source = insta360_rs.open_media(self.recording)
        self.assertEqual(len(source.streams), len(self.reference["streams"]))
        for stream, expected in zip(source.streams, self.reference["streams"]):
            info = stream.info
            with self.subTest(stream=info.stream_index):
                self.assertEqual(stream.source_path, self.recording.resolve())
                self.assertEqual(info.input_index, 0)
                self.assertEqual(info.stream_index, expected["index"])
                self.assertEqual(info.kind, expected["codec_type"])
                self.assertEqual(info.codec, expected["codec_name"])
                self.assertIsInstance(info.codec_id, int)
                self.assertGreater(info.codec_id, 0)
                self.assertEqual(
                    info.time_base, tuple(map(int, expected["time_base"].split("/")))
                )
                self.assertEqual(info.start_time, expected.get("start_pts"))
                self.assertEqual(info.duration, expected.get("duration_ts"))
                self.assertEqual(info.width, expected.get("width", 0))
                self.assertEqual(info.height, expected.get("height", 0))
                self.assertEqual(len(info.codec_extradata), expected["extradata_size"])
                self.assertEqual(
                    "SHA256:" + hashlib.sha256(info.codec_extradata).hexdigest(),
                    expected["extradata_hash"],
                )
                assert_readonly(
                    self,
                    info,
                    [
                        "input_index",
                        "stream_index",
                        "kind",
                        "codec",
                        "codec_id",
                        "time_base",
                        "start_time",
                        "duration",
                        "width",
                        "height",
                        "codec_extradata",
                    ],
                )
                assert_readonly(self, stream, ["info", "source_path"])
        assert_readonly(self, source, ["streams", "video_streams"])
        source.streams.clear()
        source.video_streams.clear()
        self.assertEqual(len(source.streams), 3)
        self.assertEqual(len(source.video_streams), 2)

    def test_all_encoded_packet_fields_match_source_bytes_and_ffprobe(self):
        source = insta360_rs.open_media(self.recording)
        for stream in source.streams:
            reader = stream.open_packets()
            self.assertIsInstance(reader, insta360_rs.PacketReader)
            self.assertEqual(reader.info.stream_index, stream.info.stream_index)
            assert_readonly(self, reader, ["info"])
            actual = read_all(reader, "read_packet")
            expected = [
                packet
                for packet in self.reference["packets"]
                if packet["stream_index"] == stream.info.stream_index
            ]
            self.assertEqual(len(actual), len(expected))
            for packet, reference in zip(actual, expected):
                with self.subTest(stream=stream.info.stream_index, pts=packet.pts):
                    self.assertEqual(packet.input_index, 0)
                    self.assertEqual(packet.stream_index, reference["stream_index"])
                    self.assertEqual(packet.time_base, stream.info.time_base)
                    self.assertEqual(packet.pts, reference.get("pts"))
                    self.assertEqual(packet.dts, reference.get("dts"))
                    self.assertEqual(packet.duration, reference.get("duration", 0))
                    self.assertEqual(packet.source_position, int(reference["pos"]))
                    self.assertEqual(len(packet.data), int(reference["size"]))
                    self.assertEqual(packet.key_frame, "K" in reference["flags"])
                    self.assertEqual(packet.corrupt, "C" in reference["flags"])
                    self.assertEqual(bool(packet.flags & 1), packet.key_frame)
                    self.assertEqual(bool(packet.flags & 2), packet.corrupt)
                    self.assertEqual(
                        "SHA256:" + hashlib.sha256(packet.data).hexdigest(),
                        reference["data_hash"],
                    )
                    start = packet.source_position
                    self.assertEqual(
                        packet.data, self.original[start : start + len(packet.data)]
                    )
            assert_readonly(
                self,
                actual[0],
                [
                    "input_index",
                    "stream_index",
                    "data",
                    "pts",
                    "dts",
                    "duration",
                    "time_base",
                    "flags",
                    "key_frame",
                    "corrupt",
                    "source_position",
                    "side_data",
                ],
            )
            self.assertIsNone(reader.read_packet())
        video_packets = read_all(source.video_streams[0].open_packets(), "read_packet")
        self.assertTrue(any(packet.dts < 0 for packet in video_packets))
        self.assertTrue(any(packet.pts != packet.dts for packet in video_packets))

    def test_aac_skip_sample_side_data_is_preserved_as_owned_bytes(self):
        source = insta360_rs.open_media(self.recording)
        audio = next(stream for stream in source.streams if stream.info.kind == "audio")
        reader = audio.open_packets()
        packet = reader.read_packet()
        reference = next(
            packet
            for packet in self.reference["packets"]
            if packet["stream_index"] == audio.info.stream_index
        )
        expected = next(
            entry
            for entry in reference["side_data_list"]
            if entry["side_data_type"] == "Skip Samples"
        )
        self.assertEqual(len(packet.side_data), 1)
        side_data = packet.side_data[0]
        self.assertIsInstance(side_data, insta360_rs.StreamSideData)
        self.assertIsInstance(side_data.kind, int)
        self.assertIsInstance(side_data.data, bytes)
        self.assertEqual(
            side_data.data,
            struct.pack(
                "<IIBB",
                expected["skip_samples"],
                expected["discard_padding"],
                expected["skip_reason"],
                expected["discard_reason"],
            ),
        )
        assert_readonly(self, side_data, ["kind", "data"])
        packet.side_data.clear()
        self.assertEqual(len(packet.side_data), 1)
        preserved = side_data.data
        del source, audio, reader, packet
        gc.collect()
        self.assertEqual(side_data.data, preserved)

    def test_decoded_frames_have_original_pts_seconds_and_tightly_packed_rgb(self):
        stream = insta360_rs.open_media(self.recording).video_streams[0]
        reader = stream.open_video()
        self.assertIsInstance(reader, insta360_rs.VideoFrameReader)
        self.assertEqual(reader.info.codec_extradata, stream.info.codec_extradata)
        assert_readonly(self, reader, ["info"])
        frames = read_all(reader, "read_frame")
        self.assertEqual(
            len(frames), 10
        )  # Includes delayed B-frames during decoder drain.
        for index, frame in enumerate(frames):
            self.assertEqual((frame.width, frame.height), (32, 16))
            self.assertEqual(frame.time_base, stream.info.time_base)
            self.assertAlmostEqual(frame.timestamp_seconds, index / 10)
            self.assertAlmostEqual(
                (frame.pts - stream.info.start_time)
                * frame.time_base[0]
                / frame.time_base[1],
                frame.timestamp_seconds,
            )
            self.assertEqual(len(frame.data), 32 * 16 * 3)
            self.assertTrue(
                all(
                    r > 200 and g < 30 and b < 30
                    for r, g, b in zip(
                        frame.data[::3], frame.data[1::3], frame.data[2::3]
                    )
                )
            )
        assert_readonly(
            self,
            frames[0],
            ["data", "width", "height", "timestamp_seconds", "pts", "time_base"],
        )
        self.assertIsNone(reader.read_frame())
        self.assertIsNone(reader.read_frame())

    def test_stream_descriptors_readers_and_owned_results_outlive_their_parents(self):
        source = insta360_rs.open_media(self.recording)
        stream = source.video_streams[0]
        del source
        gc.collect()
        packets, frames = stream.open_packets(), stream.open_video()
        info = stream.info
        del stream
        gc.collect()
        packet, frame = packets.read_packet(), frames.read_frame()
        encoded, rgb = packet.data, frame.data
        read_all(packets, "read_packet")
        read_all(frames, "read_frame")
        del packets, frames
        gc.collect()
        self.assertEqual(packet.data, encoded)
        self.assertEqual(frame.data, rgb)
        self.assertEqual(info.codec, "mpeg4")

    def test_packet_reader_serializes_reads_across_threads(self):
        reader = insta360_rs.open_media(self.recording).video_streams[0].open_packets()
        with ThreadPoolExecutor(max_workers=4) as workers:
            packets = list(workers.map(lambda _: reader.read_packet(), range(14)))
        packets = [packet for packet in packets if packet is not None]
        self.assertEqual(len(packets), 10)
        self.assertEqual(len({packet.pts for packet in packets}), 10)

    def test_independent_video_readers_have_independent_seek_positions(self):
        stream = insta360_rs.open_media(self.recording).video_streams[0]
        early, late = stream.open_video(), stream.open_video()
        self.assertAlmostEqual(early.read_frame().timestamp_seconds, 0)
        self.assertAlmostEqual(late.frame_at(0.7).timestamp_seconds, 0.7)
        self.assertAlmostEqual(early.read_frame().timestamp_seconds, 0.1)
        self.assertAlmostEqual(late.read_frame().timestamp_seconds, 0.8)

    def test_random_access_returns_first_frame_at_or_after_each_target(self):
        stream = insta360_rs.open_media(self.recording).video_streams[0]
        sequential = read_all(stream.open_video(), "read_frame")
        reader = stream.open_video()
        # Visit forwards and backwards across GOP boundaries, including B-frames.
        for target in [0.7, 0.1, 0.35, 0.8, 0, 0.2, 0.55, 0.9, 0.45, 0.3, 0.95]:
            with self.subTest(target=target):
                expected = next(
                    (
                        frame
                        for frame in sequential
                        if frame.timestamp_seconds >= target
                    ),
                    None,
                )
                actual = reader.frame_at(target)
                if expected is None:
                    self.assertIsNone(actual)
                else:
                    self.assertIsNotNone(actual)
                    self.assertEqual(actual.pts, expected.pts)
                    self.assertEqual(actual.data, expected.data)

    def test_packet_seek_uses_seconds_and_lands_on_a_preceding_keyframe(self):
        reader = insta360_rs.open_media(self.recording).video_streams[0].open_packets()
        reader.seek(0.75)
        packet = reader.read_packet()
        self.assertTrue(packet.key_frame)
        seconds = packet.pts * packet.time_base[0] / packet.time_base[1]
        self.assertGreater(seconds, 0)
        self.assertLessEqual(seconds, 0.75)

    def test_frame_and_seek_seconds_are_relative_to_nonzero_stream_start(self):
        with tempfile.TemporaryDirectory() as directory:
            shifted = Path(directory) / "shifted.insv"
            run_tool(
                "ffmpeg",
                "-v",
                "error",
                "-nostdin",
                "-i",
                self.recording,
                "-map",
                "0",
                "-c",
                "copy",
                "-output_ts_offset",
                "5",
                "-f",
                "mp4",
                shifted,
            )
            for stream in insta360_rs.open_media(shifted).video_streams:
                self.assertGreater(stream.info.start_time, 0)
                packet = stream.open_packets().read_packet()
                self.assertGreaterEqual(
                    packet.pts * packet.time_base[0] / packet.time_base[1], 5
                )
                reader = stream.open_video()
                first = reader.frame_at(0)
                self.assertEqual(first.timestamp_seconds, 0)
                self.assertEqual(first.pts, stream.info.start_time)
                self.assertAlmostEqual(reader.frame_at(0.45).timestamp_seconds, 0.5)
                self.assertIsNone(reader.frame_at(10))
                self.assertEqual(reader.frame_at(0).data, first.data)

    def test_changed_or_deleted_source_is_detected_when_opening_new_readers(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "mutable.insv"
            path.write_bytes(self.original)
            stream = insta360_rs.open_media(path).video_streams[0]
            path.unlink()
            for method in [stream.open_packets, stream.open_video]:
                with (
                    self.subTest(method=method),
                    self.assertRaises(insta360_rs.Insta360IOError),
                ):
                    method()
            generate_media(path, width=48, height=32)
            for method in [stream.open_packets, stream.open_video]:
                with (
                    self.subTest(method=method),
                    self.assertRaises(insta360_rs.InvalidMediaError),
                ):
                    method()

    def test_rejects_invalid_seek_times(self):
        stream = insta360_rs.open_media(self.recording).video_streams[0]
        for reader in [stream.open_packets(), stream.open_video()]:
            for timestamp in [-1.0, float("inf"), -float("inf"), float("nan"), 1e300]:
                with self.subTest(reader=type(reader), timestamp=timestamp):
                    with self.assertRaises(insta360_rs.InvalidMediaError):
                        reader.seek(timestamp)
        reader = stream.open_video()
        for timestamp in [-1, float("nan"), float("inf"), 1e300]:
            with (
                self.subTest(timestamp=timestamp),
                self.assertRaises(insta360_rs.InvalidMediaError),
            ):
                reader.frame_at(timestamp)
        for method in [reader.seek, reader.frame_at, stream.open_packets().seek]:
            for value in [None, "0.5", object()]:
                with (
                    self.subTest(method=method, value=value),
                    self.assertRaises(TypeError),
                ):
                    method(value)

    def test_legacy_pair_keeps_original_input_and_stream_indices(self):
        with tempfile.TemporaryDirectory() as directory:
            primary = Path(directory) / "VID_20260101_120000_00_001.insv"
            secondary = Path(directory) / "VID_20260101_120000_10_001.insv"
            shutil.copyfile(self.recording, primary)
            shutil.copyfile(self.recording, secondary)
            for inputs in [secondary, [secondary, primary]]:
                with self.subTest(inputs=inputs):
                    source = insta360_rs.open_media(inputs)
                    self.assertEqual(
                        [
                            (stream.info.input_index, stream.info.stream_index)
                            for stream in source.streams
                        ],
                        [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)],
                    )
                    self.assertEqual(source.streams[0].source_path, primary.resolve())
                    self.assertEqual(source.streams[3].source_path, secondary.resolve())
                    self.assertIsNotNone(
                        source.video_streams[-1].open_video().read_frame()
                    )
            self.assertEqual(set(Path(directory).iterdir()), {primary, secondary})


if __name__ == "__main__":
    unittest.main()
