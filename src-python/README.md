# insta360-rs for Python

Python 3.10+ file-oriented bindings for the portable `insta360-rs` INSV parser
and stitch pipeline. See the Rust crate documentation for supported cameras,
calibration policy, and current exporter capabilities.

Read the [Python documentation](docs/README.md) for installation, examples, the
complete API reference, and testing. The binding sources live in `src-python/`.

From the repository root, with the FFmpeg development environment configured:

```sh
python3 -m venv src-python/.venv
. src-python/.venv/bin/activate
python -m pip install 'maturin>=1.9,<2' build ruff
maturin develop --manifest-path src-python/Cargo.toml
python -m unittest discover -s src-python/tests -v
python src-python/scripts/test.py
```

The final command builds and inspects both distributions, rebuilds the source
archive, and runs the native API suite in isolated wheel installations. See
[testing](docs/testing.md) for prerequisites and multiple-interpreter checks.

```python
from insta360_rs import probe

info = probe("recording.insv")
print(info.camera_name, info.video_tracks)
```

Extract all streams, audio, metadata, telemetry, and original trailer records
without stitching or calibration:

```python
from pathlib import Path
from insta360_rs import extract

report = extract(Path("recording.insv"), Path("recording-extracted"))
print(report.manifest_path, report.stream_count, report.record_count)
```

The target folder must be absent or empty and must not be a symbolic link. A
single path discovers its legacy sibling; an explicit pair can be supplied as a
list. Extraction preserves raw packets and records, writes decoded metadata and
playable stream copies where supported, and publishes a JSON manifest. Video and
audio are copied without decoding or re-encoding. The call releases the GIL and
returns an `ExtractionReport` containing absolute output paths, counts, and
warnings.

Media acceleration is selected separately from the stitching backend. The
current implementation uses software decoding; this setting selects the HEVC
encoder. The default is automatic selection, and callers can explicitly require
hardware or software encoding:

```python
from insta360_rs import AudioPolicy, MediaAcceleration, export_video

export_video(
    "recording.insv",
    "stitched.mp4",
    acceleration=MediaAcceleration.HARDWARE,
    audio=AudioPolicy.DROP,
)
```

Stitched audio copy is not implemented; the default `AudioPolicy.COPY` raises
`MissingCapabilityError`. Select `DROP` explicitly for video exports. Extraction
and original stream readers retain audio packets.

Stitching is selected with `StitchConfig(backend=ProcessingBackend.GPU)`. The
default `ProcessingBackend.AUTO` attempts GPU and reruns the complete export on
CPU after a typed GPU initialization/processing failure. Explicit CPU/GPU
requests remain strict. The Python extension enables the Rust `gpu` feature,
although Metal, D3D12, and Vulkan real-X5 qualification is still in progress.
Both `export_video` and `start_export_video` accept the same `acceleration=`
keyword; encoder `AUTO` retries eligible candidates when configuration/opening
fails, but not after encoding has begun.

An `<output>.insta360-rs-part` file is an incomplete internal muxing artifact,
not a preview. It may not open even if renamed to `.mp4` because the MP4 trailer
is written only at successful completion. Wait for the export API to return the
atomically published final path.

Wheels include the same licensed Insta360 and Studio data assets embedded by the
Rust crate, including the model files. They do not contain or link vendor
executables or runtime libraries. Project-authored code is licensed only under
Apache-2.0; the bundled vendor assets retain their original licensing as
described in the project `NOTICE.md` and provenance inventory.

X5 direction lock, gravity-referenced leveling, and supported sensor readout
correction run on both CPU and GPU. Use
`StitchConfig(stabilization=Stabilization.DIRECTION_LOCK, rolling_shutter=RollingShutterCorrection.REQUIRED)`
to require the complete motion path. `AUTO` permits unavailable readout
correction with a warning; `OFF` applies only global stabilization. See
[stabilization](../docs/stabilization.md) for exact metadata requirements and
six-axis heading limitations.
