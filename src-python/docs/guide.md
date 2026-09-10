# Python guide

The `insta360-rs` Python distribution exposes direct stream readers, decoded
video frames, and file conversion. The extension is built with PyO3's Python
3.10 stable ABI and maturin.

`open_media()` returns the recording's video, audio, and other streams directly
from the original input files. Opening the source probes stream headers;
`open_video()` initializes a decoder, and `read_frame()`/`frame_at()` decode
frames. No intermediate files are created, and neither camera identification nor
calibration is required:

```python
from insta360_rs import open_media

source = open_media("recording.insv")
for stream in source.streams:
    print(stream.info.input_index, stream.info.stream_index,
          stream.info.kind, stream.info.codec)

lens = source.video_streams[0]
frames = lens.open_video()
preview = frames.frame_at(12.5)
if preview is not None:
    print(preview.width, preview.height, preview.timestamp_seconds)
    rgb = preview.data  # Python bytes, tightly packed RGB24

frames.seek(0.0)
while (frame := frames.read_frame()) is not None:
    print(frame.timestamp_seconds, len(frame.data))
```

`frame_at(seconds)` seeks and returns the first frame at or after that time.
`seek(seconds)` sets the same position for the next `read_frame()`. Positions
are relative to the selected stream's declared start, or zero when its start is
absent. Seeking flushes pending frames and supports revisiting a reader after
EOF. Reads return `None` at clean EOF; read/decode and I/O failures raise the
existing typed exceptions. Times must be finite and nonnegative.

Each video frame owns tightly packed, row-major, 8-bit RGB bytes with length
`width * height * 3`. `timestamp_seconds` is optional when a usable timestamp is
absent or lies in negative preroll; `pts` and `time_base` also retain the
original timing. This preview conversion uses software decoding, supported
recorded YUV matrices, and the recorded range. An unspecified matrix defaults to
BT.601; unsupported declared matrices raise `MissingCapabilityError`. It reduces
higher bit depths to RGB24 and does not stitch, rotate, stabilize, tone-map HDR,
or apply the I-Log color transform. See
[stream access](../../docs/extraction.md#direct-stream-access) for the matrix
support list.

Encoded packet readers retain the original compressed representation, timing,
flags, side data, and bit depth without decoding or re-encoding:

```python
packets = lens.open_packets()
configuration = lens.info.codec_extradata  # Python bytes
while (packet := packets.read_packet()) is not None:
    print(packet.pts, packet.dts, packet.time_base, len(packet.data))

packets.seek(12.5)  # Keyframe at or before 12.5 seconds, including preroll
packet = packets.read_packet()
```

`StreamInfo` exposes `input_index`, `stream_index`, `kind`, `codec`, `codec_id`,
`time_base`, `start_time`, `duration`, `width`, `height`, and `codec_extradata`.
`kind` is `"video"`, `"audio"`, `"data"`, `"subtitle"`, `"attachment"`, or
`"unknown"`. `MediaStream.source_path` is the canonical original file path. Each
time base is a `(numerator, denominator)` tuple in seconds per tick. Stream
start and duration, packet PTS/DTS/duration, and frame PTS use these raw ticks;
unknown timestamps are `None` and unknown packet duration is zero. Packet DTS
can be negative. `EncodedPacket` also exposes `input_index`, `stream_index`,
`flags`, `key_frame`, `corrupt`, `source_position`, and `side_data`; each
`StreamSideData` contains its numeric `kind` and exact Python `bytes` payload.
Audio and data streams support packet access; `open_video()` requires a video
stream.

A single input path discovers its conventional `_00_`/`_10_` companion; explicit
pairs use a sequence of paths. `source.streams` and `source.video_streams`
preserve original input and stream order. Each `open_packets()` or
`open_video()` creates an independent read cursor. A reader remains usable after
its source object is released; the original files must remain present and
unchanged. Stream information, packets, and frames have read-only properties and
expose owned data without native pointers. Opening, decoding, reading, seeking,
and waiting for a reader's mutex release the GIL. Concurrent operations on the
same reader are serialized; independent readers can run concurrently.

`extract()` writes the recording's streams, audio, metadata, telemetry, and
original trailer records into a target folder without calibration or stitching:

```python
from pathlib import Path
from insta360_rs import extract

report = extract(Path("recording.insv"), Path("recording-extracted"))
print(report.manifest_path)
print(report.input_count, report.stream_count, report.record_count)
for warning in report.warnings:
    print(warning)
```

A single path discovers the matching legacy `_00_`/`_10_` sibling. To pass a
pair explicitly, supply a sequence of paths. Extraction preserves raw stream
packets with timing indexes, codec configuration, the original trailer, and
individual records; it also writes decoded metadata and playable stream copies
where supported. Media is copied without decoding or re-encoding. The JSON
manifest maps these artifacts back to each input. Unsupported record encodings
remain available as raw bytes. V2 and V3 tails can be extracted independently of
camera and calibration support.

The target must be absent or empty and must not be a symbolic link. Successful
extraction publishes the completed folder atomically. `ExtractionReport` has
read-only `output_dir`, `manifest_path`, `input_count`, `stream_count`,
`record_count`, `files`, and `warnings` properties; its paths are absolute. The
call blocks and releases the GIL while the Rust implementation runs.

```python
from insta360_rs import Housing, StitchConfig, export_frames, probe

info = probe("recording.insv")
config = StitchConfig.underwater_photogrammetry(
    housing=Housing.DIVE_CASE_PRO
)
result = export_frames(
    "recording.insv",
    "frames",
    timestamps=[1.0, 2.5, 4.0],
    config=config,
)
```

Video intervals use seconds and follow the same half-open semantics as Rust and
the CLI:

```python
from insta360_rs import (
    AudioPolicy,
    MediaAcceleration,
    Environment,
    Housing,
    ProcessingBackend,
    StitchConfig,
    export_video,
)

config = StitchConfig(
    housing=Housing.DIVE_CASE_PRO,
    environment=Environment.UNDERWATER,
    backend=ProcessingBackend.AUTO,
)

export_video(
    "recording.insv",
    "middle-minute.mp4",
    start=635.298,
    duration=60.0,
    audio=AudioPolicy.DROP,
    acceleration=MediaAcceleration.AUTO,
    config=config,
)
```

`ProcessingBackend` selects the stitch renderer. `AUTO` attempts GPU first and
reruns the complete export on CPU after a typed GPU initialization/processing
failure. `CPU` and `GPU` are strict; explicit `GPU` raises `GpuUnavailableError`
or `GpuProcessingError` rather than retrying. `MediaAcceleration` is separate
and currently selects the HEVC encoder: `SOFTWARE` and `HARDWARE` restrict the
candidate class, while `AUTO` tries all eligible candidates in preference order
when configuration/opening fails. It does not enable hardware decoding or
restart after a mid-stream encoder error.

The Python API exposes both blocking `export_video` and job-based
`start_export_video`; both accept the same `acceleration=` keyword, defaulting
to `MediaAcceleration.AUTO`. `capabilities()` reports compiled/discovered GPU
state and encoder names. The Python extension enables both core `media` and
`gpu` features, so it uses the same automatic backend behavior as Rust and the
CLI. Cross-platform wheel qualification remains release work.

During a video job, `<output>.insta360-rs-part` is intentionally incomplete and
may be rejected by VLC even after manually renaming it. Wait for
`export_video()` or `ExportJob.wait()` to return: only then has FFmpeg written
the MP4 trailer and the exporter atomically published the artifact to the final
path without replacing any existing file. Failed, cancelled, and automatically
retried attempts remove their own temporary file.

Long-running native operations release the GIL. Asynchronous jobs expose
polling, waiting, and cancellation; Rust worker threads never call arbitrary
Python callbacks.

`probe()` canonicalizes the registered ONE, ONE X, ONE R/RS, X2–X6 and X4 Air
camera aliases. `info.optics` reports detected housing/environment/accessories,
the encoded lens ID and any ambiguity. Successful exports expose `result.optics`
with requested, detected and effective values. Housing, environment, lens
accessory and mount are independent enums; only established calibrations and
conversions are accepted. See [housings](../../docs/housings.md).

`StitchConfig(color_conversion=ColorConversion.AUTO)` uses the bundled X5
I-Log-to-Rec.709 table when recording metadata identifies I-Log. Use
`ColorConversion.PRESERVE` for downstream grading or `I_LOG_TO_REC709` for an
older I-Log file with missing metadata. The option also applies to
`StitchConfig.underwater_photogrammetry()`. CPU and GPU exports both execute the
transform. See [runtime asset usage](../../docs/asset-usage.md).

Underwater restoration is opt-in and leaves pixel geometry unchanged.
`UnderwaterColorOptions` is immutable; assign a new options value to the mutable
`config.underwater_color` property when changing settings:

```python
from insta360_rs import UnderwaterColorMode, UnderwaterColorOptions
config.underwater_color = UnderwaterColorOptions(
    mode=UnderwaterColorMode.LEGACY, strength=0.8, balance=0.5
)
# AI uses the optional independent MNN engine; packaged wheels include it.
config.underwater_color = UnderwaterColorOptions(
    mode=UnderwaterColorMode.AI, strength=1.0, style=0
)
```

Use Off for consistent un-restored photogrammetry data. Color processing retains
job-local temporal state and resets across independent images and recording
boundaries. Missing/corrupt required resources fail explicitly. The general Rust
asset provider is not a Python export argument. SVM accessory classification, AI
seams and ColorPlus remain unavailable.

Wheels are intended for macOS ARM64/x86_64, Windows x86_64, and manylinux
x86_64. Their FFmpeg runtime is capability-pruned and carries its third-party
notices. The Rust dependency carries the licensed resource bundle; no vendor
library is linked. See the
[literal asset copy inventory](../../docs/sdk-provenance.md). The wheel includes
the project `NOTICE.md` alongside the Apache-2.0 license file.

See [installation](installation.md), the [API reference](reference.md), and
[testing](testing.md) for build commands and executable contract coverage.
