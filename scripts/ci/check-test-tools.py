"""Require working fixture tools before Rust tests can skip missing media coverage."""

import json
from pathlib import Path
import shutil
import subprocess
import tempfile


ENCODERS = ("mpeg4", "aac", "alac", "ac3", "libx265")


def run(*command):
    result = subprocess.run(
        list(map(str, command)), check=True, capture_output=True, text=True, timeout=60
    )
    return result.stdout + result.stderr


def verify():
    missing = [name for name in ("ffmpeg", "ffprobe") if not shutil.which(name)]
    if missing:
        raise RuntimeError("Missing media fixture tools: " + ", ".join(missing))
    for name in ("ffmpeg", "ffprobe"):
        print(run(name, "-version").splitlines()[0], flush=True)
    for encoder in ENCODERS:
        description = run("ffmpeg", "-hide_banner", "-h", f"encoder={encoder}")
        # FFmpeg can return success even when the requested encoder is absent.
        if f"Encoder {encoder} " not in description:
            raise RuntimeError(f"Fixture FFmpeg is missing the {encoder} encoder")
        if encoder == "libx265" and "yuv420p10le" not in description:
            raise RuntimeError("Fixture libx265 must support 10-bit YUV420 input")

    with tempfile.TemporaryDirectory(prefix="insta360-test-tools-") as temporary:
        fixture = Path(temporary) / "fixture.mp4"
        run(
            "ffmpeg", "-v", "error", "-nostdin",
            "-f", "lavfi", "-i", "testsrc2=size=32x32:rate=10",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100",
            "-t", "0.2", "-vf", "hue=h=90", "-af", "asetpts=PTS+0.05/TB",
            "-c:v", "mpeg4", "-bf", "1", "-c:a", "aac", fixture,
        )
        description = json.loads(run(
            "ffprobe", "-v", "error", "-show_streams", "-of", "json", fixture
        ))
        codecs = {stream["codec_name"] for stream in description["streams"]}
        if codecs != {"mpeg4", "aac"}:
            raise RuntimeError(f"Unexpected generated fixture codecs: {sorted(codecs)}")
    print("Media fixture encoders, filters, muxing and probing are available", flush=True)


if __name__ == "__main__":
    verify()
