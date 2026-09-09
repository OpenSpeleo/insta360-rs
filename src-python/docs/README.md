# insta360-rs Python documentation

`insta360-rs` installs the `insta360_rs` package: a Python 3.10+ interface to
the Rust parser, FFmpeg stream readers, extraction, and stitching pipeline.
Paths, owned bytes, typed results, and exceptions cross the PyO3 boundary. Video
processing stays in Rust; NumPy and Python callbacks are not required.

- [Installation and building](installation.md)
- [Media reading and export guide](guide.md)
- [Complete public API reference](reference.md)
- [Build, FFI, and API testing](testing.md)

## Read a recording

```python
from pathlib import Path
from insta360_rs import open_media, probe

recording = Path("recording.insv")
info = probe(recording)
print(info.camera, info.duration_seconds, info.offset_versions)

source = open_media(recording)
for stream in source.streams:
    print(stream.info.kind, stream.info.codec, stream.info.time_base)

reader = source.video_streams[0].open_video()
frame = reader.frame_at(1.5)
if frame is not None:
    print(frame.width, frame.height, len(frame.data))
```

`probe` reads INSV metadata. `open_media` accesses decodable container streams
without requiring an Insta360 trailer or stitch calibration. `extract` saves
original streams and metadata without stitching. Video/frame exports require
supported camera metadata and recorded calibration; currently the exporter
supports X5 single-file recordings with two video tracks.

## Export selected frames

```python
from insta360_rs import ProcessingBackend, Stabilization, StitchConfig, export_frames

config = StitchConfig(
    backend=ProcessingBackend.CPU,
    stabilization=Stabilization.OFF,
    width=1024,
    height=512,
)
result = export_frames(
    "recording.insv", "frames", timestamps=[0.0, 1.5], config=config
)
print(result.outputs, result.frames_written, result.backend.selected)
```

The default optical policy selects only an unambiguous recorded profile. Set
`optical_setup` explicitly for an accessory or water profile; Python does not
substitute generic calibration. Disabling stabilization in this example removes
the requirement for gyro data, which the default direction-lock mode needs. For
stitched video, `audio=AudioPolicy.COPY` preserves compatible AAC/ALAC packets
and their timing relative to video; cuts keep complete packets. Select `DROP` to
omit audio. Direct packet access and extraction also preserve audio.

See the [guide](guide.md) for underwater presets, backend selection,
cancellation, color conversion, and the distinction between reading and
stitching.
