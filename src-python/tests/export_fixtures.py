"""Small real recordings for exercising decode, calibration, stitch and encode.

The V3 record index and V6 calibration mirror tests/container.rs and
tests/common/mod.rs. No camera recording or downloaded fixture is required.
"""

import json
import shutil
import struct
import subprocess
import tempfile
from pathlib import Path

import insta360_rs as api

MEDIA_TOOLS_AVAILABLE = bool(shutil.which("ffmpeg") and shutil.which("ffprobe"))
MAGIC = b"8db42d694ccc418790edff439fe026bf"


def varint(value):
    encoded = bytearray()
    while value >= 128:
        encoded.append((value & 127) | 128)
        value >>= 7
    encoded.append(value)
    return bytes(encoded)


def protobuf_bytes(number, data):
    return varint(number << 3 | 2) + varint(len(data)) + data


def calibrated_metadata(*, camera="Insta360 X5", calibration=True, underwater=False):
    metadata = protobuf_bytes(1, b"PYTHON-SYNTHETIC-001")
    metadata += protobuf_bytes(2, camera.encode())
    metadata += protobuf_bytes(3, b"v1.2.3")
    metadata += varint(20 << 3) + varint(10)
    metadata += varint(62 << 3) + varint(1)  # X5 compact raw IMU.
    metadata += varint(24 << 3) + varint(1_000_000)  # First camera exposure, us.
    metadata += varint(64 << 3) + varint(2)  # Exposure-to-video mapping.
    metadata += protobuf_bytes(65, varint(8) + varint(8) + varint(16) + varint(2000))
    metadata += varint(25 << 3 | 1) + struct.pack("<d", 10.0)  # Readout ms.
    metadata += varint(28 << 3 | 1) + struct.pack("<d", 0.0)
    metadata += varint(130 << 3) + varint(1)  # Unrotated decoded source.
    metadata += protobuf_bytes(
        27, b"".join(varint(field << 3) + varint(64) for field in (1, 2, 3, 4))
    )
    if calibration:
        values = [2]
        for center in (32, 96):
            values.extend([2, 55.04, 55.04, center, 32, 0, 0, 0, 0, 0, 0])
            values.extend([0] * 13)
            values.extend([128, 64, 117 if underwater else 113])
        values.append((6 << 16) | 0x400)
        metadata += protobuf_bytes(111, "_".join(map(str, values)).encode())
    return metadata


def trailer(metadata, *, gyro=True, yaw_degrees_per_second=0, exposure_step_us=100_000):
    records = [(1, 1, metadata)]
    if gyro:
        # Raw stationary specific force (+X) maps to projector body +Z.
        # Positive raw X gyro is positive body yaw. Include pre-roll and
        # trailing coverage for centered readout.
        records.append(
            (
                3,
                0,
                b"".join(
                    struct.pack(
                        "<q6H",
                        time,
                        32768 + 4096,
                        32768,
                        32768,
                        32768 + round(yaw_degrees_per_second / 2000 * 32768),
                        32768,
                        32768,
                    )
                    for time in range(500_000, 4_500_001, 10_000)
                ),
            )
        )
        records.append(
            (
                4,
                0,
                b"".join(
                    struct.pack("<qd", 1_000_000 + index * exposure_step_us, 0.01)
                    for index in range(-1, 32)
                ),
            )
        )
    payload = bytearray()
    index = bytearray(31 * 10)
    for identifier, format_, data in records:
        offset = len(payload)
        payload.extend(data)
        payload.extend(struct.pack("<BBI", format_, identifier, len(data)))
        struct.pack_into(
            "<BBII", index, identifier * 10, identifier, format_, len(data), offset
        )
    payload.extend(index)
    payload.extend(struct.pack("<BBI", 0, 0, len(index)))
    payload.extend(bytes(32))
    payload.extend(struct.pack("<II", len(payload) + 8 + len(MAGIC), 3))
    payload.extend(MAGIC)
    return struct.pack(">I4s", len(payload) + 8, b"inst") + payload


def make_recording(root, *, duration=1, gop=3, b_frames=1):
    raw = root / "dual-lens.mp4"
    subprocess.run(
        [
            "ffmpeg",
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x64:rate=10",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x64:rate=10,hue=h=90",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-map",
            "0:v",
            "-map",
            "1:v",
            "-map",
            "2:a",
            "-t",
            str(duration),
            "-c:v",
            "mpeg4",
            "-g",
            str(gop),
            "-bf",
            str(b_frames),
            "-c:a",
            "aac",
            str(raw),
        ],
        check=True,
        capture_output=True,
        timeout=30,
    )
    source = root / "calibrated.insv"
    source.write_bytes(raw.read_bytes() + trailer(calibrated_metadata()))
    return source


def cpu_config(**changes):
    values = dict(
        backend=api.ProcessingBackend.CPU,
        stabilization=api.Stabilization.OFF,
        width=128,
        height=64,
    )
    values.update(changes)
    return api.StitchConfig(**values)


def media_description(path):
    return json.loads(
        subprocess.run(
            [
                "ffprobe",
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-count_frames",
                "-of",
                "json",
                str(path),
            ],
            check=True,
            capture_output=True,
            timeout=30,
        ).stdout
    )


def rgb_pixels(path):
    return subprocess.run(
        [
            "ffmpeg",
            "-v",
            "error",
            "-nostdin",
            "-i",
            str(path),
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ],
        check=True,
        capture_output=True,
        timeout=30,
    ).stdout


class ExportFixtureMixin:
    @classmethod
    def setUpClass(cls):
        cls.fixture_directory = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.fixture_directory.cleanup)
        cls.fixture_root = Path(cls.fixture_directory.name)
        cls.source = make_recording(cls.fixture_root)

    def setUp(self):
        self.output_directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.output_directory.cleanup)
        self.root = Path(self.output_directory.name)

    def assert_cpu_result(self, result, count):
        self.assertIsInstance(result, api.ExportResult)
        self.assertEqual(result.frames_written, count)
        self.assertGreaterEqual(result.elapsed_seconds, 0)
        self.assertIsInstance(result.backend, api.BackendReport)
        self.assertEqual(result.backend.requested, api.ProcessingBackend.CPU)
        self.assertEqual(result.backend.selected, api.EffectiveBackend.CPU)
        self.assertIsNone(result.backend.adapter)
        self.assertIsNone(result.backend.fallback)
        self.assertTrue(
            all(isinstance(path, Path) and path.is_file() for path in result.outputs)
        )
