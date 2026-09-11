"""Small, reproducible media fixtures; no camera recordings or downloads needed."""

import json
import shutil
import struct
import subprocess
from pathlib import Path

MAGIC = b"8db42d694ccc418790edff439fe026bf"


def run_tool(name, *arguments):
    executable = shutil.which(name)
    if executable is None:
        raise RuntimeError(
            f"{name} is required for native integration tests; install FFmpeg "
            "and put ffmpeg and ffprobe on PATH"
        )
    result = subprocess.run(
        [executable, *map(str, arguments)], capture_output=True, timeout=60
    )
    if result.returncode:
        raise RuntimeError(f"{name} failed: {result.stderr.decode(errors='replace')}")
    return result.stdout


def generate_media(path, *, timecode=False, width=32, height=16):
    """One second, two distinguishable MPEG-4 B-frame tracks and AAC audio."""
    arguments = [
        "-v",
        "error",
        "-nostdin",
        "-f",
        "lavfi",
        "-i",
        f"color=c=red:s={width}x{height}:r=10",
        "-f",
        "lavfi",
        "-i",
        f"color=c=blue:s={width}x{height}:r=10",
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
        "1",
        "-c:v",
        "mpeg4",
        "-g",
        "3",
        "-bf",
        "1",
        "-c:a",
        "aac",
        "-metadata",
        "title=Python native integration fixture",
    ]
    if timecode:
        arguments += ["-timecode", "00:00:00:00"]
    run_tool("ffmpeg", *arguments, "-f", "mp4", path)
    return Path(path).read_bytes()


def ffprobe(path, *, ignore_editlist=False):
    return json.loads(
        run_tool(
            "ffprobe",
            "-v",
            "error",
            "-show_streams",
            "-show_packets",
            "-show_data_hash",
            "sha256",
            "-of",
            "json",
            *(["-ignore_editlist", "1"] if ignore_editlist else []),
            path,
        )
    )


def varint(value):
    result = bytearray()
    while value >= 128:
        result.append((value & 127) | 128)
        value >>= 7
    result.append(value)
    return bytes(result)


def protobuf_bytes(field, value):
    if isinstance(value, str):
        value = value.encode()
    return varint((field << 3) | 2) + varint(len(value)) + value


def metadata_record(camera="Insta360 X5", *, populated=True):
    result = b"" if camera is None else protobuf_bytes(2, camera)
    if populated:
        result += protobuf_bytes(1, "TEST-X5-PYTHON")
        result += protobuf_bytes(3, "v1.2.3_test")
        result += varint(62 << 3) + varint(1)  # Packed raw gyro samples, 20 bytes each.
        for field, version in [(111, 6), (5, 1), (54, 3), (53, 2)]:
            result += protobuf_bytes(field, f"2_{version}.0_2.0_3.0")
        profiles = b"".join(
            protobuf_bytes(1, protobuf_bytes(1, name))
            for name in ["bare", "InvisibleDiveWater", "bare"]
        )
        result += protobuf_bytes(145, profiles)
    return result


def bmff_box(kind, payload):
    return struct.pack(">I", len(payload) + 8) + kind + payload


def v3_tail(records=None, *, indexed=True):
    """Build actual inst framing, record footers and indexed/legacy V3 tails."""
    if records is None:
        records = [(1, 1, metadata_record()), (3, 0, bytes(40)), (4, 0, bytes(32))]
    payload = bytearray()
    directory = bytearray(10)  # Reserved absent entry, as found in camera files.
    for record_id, encoding, data in records:
        directory += struct.pack("<BBII", record_id, encoding, len(data), len(payload))
        payload += data + struct.pack("<BBI", encoding, record_id, len(data))
    if indexed:
        directory += bytes(10)
        payload += directory + struct.pack("<BBI", 0, 0, len(directory))
    payload += bytes(32)
    payload += struct.pack("<II", len(payload) + 40, 3) + MAGIC
    return bmff_box(b"inst", payload)


def v2_tail(metadata):
    return metadata + struct.pack("<II", len(metadata), 2) + MAGIC


def synthetic_recording(
    *,
    camera="Insta360 X5",
    sample_count=300,
    tracks=2,
    populated=True,
    indexed=True,
    metadata=None,
):
    """Parser-only BMFF fixture; has track declarations but no encoded media."""
    mdhd = bytes(12) + struct.pack(">II", 30_000, sample_count * 1001) + bytes(4)
    hdlr = bytes(8) + b"vide" + bytes(12)
    sample = bytearray(36)
    sample[:8] = struct.pack(">I", 36) + b"hvc1"
    sample[32:] = struct.pack(">HH", 2880, 2880)
    stsd = bytes(4) + struct.pack(">I", 1) + sample
    stts = bytes(4) + struct.pack(">III", 1, sample_count, 1001)
    stbl = bmff_box(b"stsd", stsd) + bmff_box(b"stts", stts)
    mdia = bmff_box(b"mdhd", mdhd) + bmff_box(b"hdlr", hdlr)
    mdia += bmff_box(b"minf", bmff_box(b"stbl", stbl))
    movie = bmff_box(b"moov", bmff_box(b"trak", bmff_box(b"mdia", mdia)) * tracks)
    records = [
        (
            1,
            1,
            metadata
            if metadata is not None
            else metadata_record(camera, populated=populated),
        )
    ]
    if populated:
        records += [(3, 0, bytes(40)), (4, 0, bytes(32))]
    return (
        bmff_box(b"ftyp", b"isom\0\0\0\0isom")
        + movie
        + v3_tail(records, indexed=indexed)
    )


def read_all(reader, method):
    result = []
    while (value := getattr(reader, method)()) is not None:
        result.append(value)
    return result


def assert_readonly(test, value, attributes):
    for name in attributes:
        with test.subTest(type=type(value).__name__, property=name):
            original = getattr(value, name)
            with test.assertRaises(AttributeError):
                setattr(value, name, original)
            with test.assertRaises(AttributeError):
                delattr(value, name)
