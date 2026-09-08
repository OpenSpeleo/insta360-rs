# Public API reference

The package's [`__init__.pyi`](../python/insta360_rs/__init__.pyi) is the
complete typed interface. All names below are exported from `insta360_rs`;
`__version__` matches installed distribution metadata. Native implementation
names prefixed with `_` are private.

## Inputs and ownership

`PathInput = str | os.PathLike[str]`;
`Inputs = PathInput | Sequence[PathInput]`. Use strings, `pathlib.Path`, or
objects returning strings from `__fspath__`. Bytes paths are outside this
contract. Empty sequences and missing inputs raise typed errors. A single input
discovers its conventional `_00_`/`_10_` sibling; explicit pairs are validated
and sorted into canonical input order.

Result paths are `pathlib.Path`. Native result objects have read-only properties
and no public constructor. List and byte getters return owned Python values;
editing a returned list does not mutate native state. `StitchConfig` is mutable.
Frame and packet bytes remain valid after subsequent reads and after the reader
or source is collected. Open readers own independent positions and require the
original input files to remain present and unchanged.

## Functions

| Function                                         | Behavior and return value                                                                                                                                              |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `probe(input)`                                   | Read INSV metadata without decoding; returns `MediaInfo`.                                                                                                              |
| `open_media(input)`                              | Open original container streams without calibration or materializing files; returns `MediaSource`.                                                                     |
| `extract(input, output_dir)`                     | Preserve streams, metadata, and trailer records; returns `ExtractionReport`. Target must be absent or empty and cannot be a symlink; successful publication is atomic. |
| `capabilities()`                                 | Detect compiled media/GPU support and available encoder names; returns `Capabilities`. Encoder registration does not guarantee the device can open an export.          |
| `export_frames(input, output_dir, *, ...)`       | Block until selected stitched images are written; returns `ExportResult`.                                                                                              |
| `start_export_frames(input, output_dir, *, ...)` | Same parameters as `export_frames`, returns `ExportJob`.                                                                                                               |
| `export_video(input, output, *, ...)`            | Block until a stitched HEVC MP4 is completed; returns `ExportResult`.                                                                                                  |
| `start_export_video(input, output, *, ...)`      | Same parameters as `export_video`, returns `ExportJob`.                                                                                                                |

### Frame options

Exactly one nonempty selection is required. Indices are zero-based. Selections
are sorted and deduplicated by the native exporter.

Sampled targets are independently rounded to the nearest microsecond; rounding
does not accumulate across a recording. Selections are limited to one million
targets, and timestamp microseconds must fit a signed 64-bit integer. Rates
above one million frames per second exceed the timestamp precision and are
rejected.

| Keyword               | Default           | Meaning                                                                                                                                                                                                          |
| --------------------- | ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `indices`             | `None`            | Sequence of unsigned 64-bit frame indices.                                                                                                                                                                       |
| `timestamps`          | `None`            | Sequence of finite nonnegative seconds; select frames at or after each time.                                                                                                                                     |
| `start`, `end`, `fps` | `None`            | All three required for a sampled range including `end` when it lands on the sampling grid; `end > start`. Positive finite FPS is rounded to integer milliframes/second and must fit `1..2**32-1` after rounding. |
| `config`              | `None`            | Equivalent to a fresh default `StitchConfig`.                                                                                                                                                                    |
| `format`              | `ImageFormat.PNG` | PNG or JPEG file encoding.                                                                                                                                                                                       |
| `quality`             | `95`              | Integer 1–100; JPEG quality (PNG remains lossless).                                                                                                                                                              |
| `scale_width`         | `None`            | Optional positive even unsigned 32-bit output width; keeps panorama aspect ratio.                                                                                                                                |

### Video options

| Keyword        | Default                  | Meaning                                                                                           |
| -------------- | ------------------------ | ------------------------------------------------------------------------------------------------- |
| `config`       | `None`                   | Equivalent to default `StitchConfig`.                                                             |
| `quality`      | `90`                     | Integer 1–100; mapped to the selected encoder's quality control.                                  |
| `audio`        | `AudioPolicy.COPY`       | Currently unsupported for stitched video; select `DROP` explicitly.                               |
| `start`        | `None`                   | Start in finite nonnegative seconds; omitted means the beginning.                                 |
| `duration`     | `None`                   | Finite positive seconds; omitted means the remaining recording. The interval is half-open.        |
| `acceleration` | `MediaAcceleration.AUTO` | Encoder candidate policy, independent of the stitch backend. Decoding currently remains software. |

Output MP4 is published only after successful finalization. Temporary
`<output>.insta360-rs-part` files are incomplete. Failed/cancelled attempts
clean up their own temporary files; do not use them as previews. Publication
never replaces an existing final path, including a file created by another
process while the export runs. Demuxer, I/O, and decoded-frame corruption errors
fail the export and remove its incomplete outputs.

## Configuration and enums

`StitchConfig(*, optical_setup=None, stabilization=None, rolling_shutter=None, backend=None, color_conversion=None, width=None, height=None)`
accepts only keyword arguments. Omitted values and explicit `None` select
defaults:

| Property           | Default                                   |
| ------------------ | ----------------------------------------- |
| `optical_setup`    | `OpticalSetup.STRICT_AUTO`                |
| `stabilization`    | `Stabilization.DIRECTION_LOCK`            |
| `rolling_shutter`  | `RollingShutterCorrection.AUTO`           |
| `backend`          | `ProcessingBackend.AUTO`                  |
| `color_conversion` | `ColorConversion.AUTO`                    |
| `width`, `height`  | `None`, `None` (native projection choice) |

Dimensions must be specified together and form a nonzero 2:1 panorama.
Construction validates dimensions; after assigning mutable properties, the
configuration is validated again when passed to an export. Each export takes a
configuration snapshot, so later assignments do not change an active job.

`StitchConfig.underwater_photogrammetry(*, optical_setup=None, rolling_shutter=None, backend=None, color_conversion=None, width=None, height=None)`
uses direction-lock and defaults to `INVISIBLE_DIVE_CASE_UNDERWATER`. It also
accepts `BARE_UNDERWATER`, `WATERPROOF_CASE`, and `DIVE_CASE_UNDERWATER`; other
optical setups are rejected. The preset still requires corresponding recorded
calibration and motion data.

`RollingShutterCorrection.AUTO` uses a supported recorded readout profile when
available and reports omissions as job warnings. `OFF` keeps only global
stabilization; `REQUIRED` fails if readout correction cannot be established and
cannot be combined with `Stabilization.OFF`.

| Enum                       | Values                                                                                                                                                                                                                                                                                                   |
| -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `OpticalSetup`             | `STRICT_AUTO`, `BARE_AIR`, `BARE_UNDERWATER`, `WATERPROOF_CASE`, `DIVE_CASE_AIR`, `DIVE_CASE_UNDERWATER`, `INVISIBLE_DIVE_CASE_AIR`, `INVISIBLE_DIVE_CASE_UNDERWATER`, `CLIP_ON_LENS_GUARD`, `ADHESIVE_SPHERE_LENS_GUARD`, `PROTECTOR_A`, `PROTECTOR_S`, `PROTECTOR_AS`, `ND16`, `ND32`, `ND64`, `ND128` |
| `Stabilization`            | `OFF`, `FLOW_STATE`, `DIRECTION_LOCK`                                                                                                                                                                                                                                                                    |
| `RollingShutterCorrection` | `AUTO`, `OFF`, `REQUIRED`                                                                                                                                                                                                                                                                                |
| `ProcessingBackend`        | `AUTO`, `CPU`, `GPU`                                                                                                                                                                                                                                                                                     |
| `ColorConversion`          | `AUTO`, `PRESERVE`, `I_LOG_TO_REC709`                                                                                                                                                                                                                                                                    |
| `EffectiveBackend`         | `CPU`, `GPU`, `UNKNOWN`                                                                                                                                                                                                                                                                                  |
| `ImageFormat`              | `PNG`, `JPEG`                                                                                                                                                                                                                                                                                            |
| `AudioPolicy`              | `COPY`, `DROP`                                                                                                                                                                                                                                                                                           |
| `MediaAcceleration`        | `AUTO`, `SOFTWARE`, `HARDWARE`                                                                                                                                                                                                                                                                           |
| `ExportPhase`              | `PROBING`, `DECODING`, `STITCHING`, `ENCODING`, `FINALIZING`, `UNKNOWN`                                                                                                                                                                                                                                  |

Use enum members as arguments, rather than strings or integers. These are PyO3
enum classes, not subclasses of Python's `enum.Enum`. `ProcessingBackend.AUTO`
tries GPU and reruns the operation on CPU after a typed GPU failure; explicit
CPU/GPU choices are strict. Encoder `AUTO` tries eligible encoders when opening
fails; it does not retry after encoding begins. Color `AUTO` applies the bundled
supported I-Log table only when metadata identifies I-Log; `PRESERVE` disables
that transform.

## Metadata and stream objects

| Object              | Read-only properties                                                                                                                                                                                                                                                                                                                                                                      |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `MediaInfo`         | `inputs: list[Path]`, `camera: str`, `camera_name`, `serial`, `firmware` (optional strings), `duration_seconds`, `fps` (optional floats), `video_tracks: list[VideoTrackInfo]`, `offset_versions: list[int]`, `optical_profiles: list[str]`, `gyro_sample_count`, `exposure_sample_count` (integers), `trailer: TrailerInfo`.                                                             |
| `VideoTrackInfo`    | `index`, `width`, `height` (integers), `codec: str`.                                                                                                                                                                                                                                                                                                                                      |
| `TrailerInfo`       | `offset`, `size` (integer byte counts), `version`, `record_count` (integers).                                                                                                                                                                                                                                                                                                             |
| `MediaSource`       | `streams`, `video_streams`: lists of `MediaStream` in original input/stream order.                                                                                                                                                                                                                                                                                                        |
| `MediaStream`       | `info: StreamInfo`, `source_path: Path`; `open_packets()` returns `PacketReader`, `open_video()` returns `VideoFrameReader` and rejects non-video streams.                                                                                                                                                                                                                                |
| `StreamInfo`        | `input_index`, `stream_index`, `codec_id`, `width`, `height` (integers), `kind`, `codec` (strings), `time_base: tuple[int, int]`, `start_time`, `duration` (optional integer ticks), `codec_extradata: bytes`.                                                                                                                                                                            |
| `EncodedPacket`     | `input_index`, `stream_index`, `duration`, `flags` (integers), `data: bytes`, `pts`, `dts`, `source_position` (optional integers), `time_base: tuple[int, int]`, `key_frame`, `corrupt` (booleans), `side_data: list[StreamSideData]`.                                                                                                                                                    |
| `StreamSideData`    | `kind: int` (native side-data identifier), `data: bytes`.                                                                                                                                                                                                                                                                                                                                 |
| `DecodedVideoFrame` | `data: bytes` (packed RGB24), `width`, `height` (integers), `timestamp_seconds: float                                                                                                                                                                                                                                         \| None`, `pts: int \| None`, `time_base: tuple[int, int]`. |

`StreamInfo.kind` is `video`, `audio`, `data`, `subtitle`, `attachment`, or
`unknown`. Time bases are seconds per tick. PTS/DTS use raw stream ticks (DTS
can be negative); `source_position` is the original byte offset. Packet duration
is zero when unknown. Frame data length is exactly `width * height * 3`, in
row-major RGB order with no padding. See the [guide](guide.md) for color-matrix
and high-bit-depth preview limitations.

| Reader             | Methods                                                                                                                                                                                             |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `PacketReader`     | `info` property; `read_packet() -> EncodedPacket    \| None`; `seek(seconds) -> None` seeks to a preceding keyframe and may include preroll.                                                        |
| `VideoFrameReader` | `info` property; `read_frame() -> DecodedVideoFrame \| None`; `seek(seconds) -> None`positions the next read at or after the time;`frame_at(seconds) -> DecodedVideoFrame \| None` seeks and reads. |

Seek arguments are finite nonnegative seconds relative to the selected stream's
declared start (zero if absent). Clean EOF returns `None` repeatedly; seeking
backward works after EOF. Read/decode and I/O failures raise typed exceptions;
marked-corrupt packets can be returned with `corrupt=True`. Native reader
operations and mutex waits release the GIL; calls on one reader serialize, while
independent readers can advance concurrently.

## Reports, capabilities, and jobs

| Object             | Read-only properties                                                                                                                                                                                                                                              |
| ------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ExtractionReport` | `output_dir`, `manifest_path: Path`; `input_count`, `stream_count`, `record_count: int`; `files: list[Path]`; `warnings: list[str]`.                                                                                                                              |
| `ExportResult`     | `outputs: list[Path]`, `frames_written: int`, `elapsed_seconds: float`, `backend: BackendReport`.                                                                                                                                                                 |
| `BackendReport`    | `requested: ProcessingBackend`, `selected: EffectiveBackend`, `adapter: GpuAdapterInfo                                                    \| None`, `fallback: GpuFailure       \| None`.                                                                         |
| `GpuAdapterInfo`   | `name`, `backend`, `device_type`, `driver`, `driver_info: str`; `vendor`, `device: int`.                                                                                                                                                                          |
| `GpuFailure`       | `code`, `stage`, `message: str`; `adapter: GpuAdapterInfo                                                                                 \| None`.                                                                                                               |
| `Capabilities`     | `image_export`, `video_export`, `gpu_compiled`, `gpu_available: bool`; `gpu_adapters: list[GpuAdapterInfo]`; `gpu_unavailable_reason: str \| None`; `hevc_encoders: list[str]`.                                                                                   |
| `ExportProgress`   | `phase: ExportPhase`, `completed: int`, `total: int                                                                                       \| None`, `media_time_seconds: float  \| None`, `elapsed_seconds: float`, `estimated_remaining_seconds: float \| None`. |

`ExportJob` has no public constructor; use a `start_export_*` function.

| Method                                  | Contract                                                                                                                                                                                    |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cancel() -> None`                      | Request cooperative cancellation. Safe repeatedly and while another thread waits. A job that completed before cancellation may still succeed.                                               |
| `is_finished() -> bool`                 | Whether the worker finished, including failure/cancellation. Does not consume the result.                                                                                                   |
| `progress() -> ExportProgress  \| None` | Latest received progress snapshot; initially may be absent.                                                                                                                                 |
| `backend() -> BackendReport    \| None` | Latest backend report; may be absent before initialization. Available after successful `wait()`.                                                                                            |
| `take_warnings() -> list[str]`          | Drain warnings received so far. Repeated calls return an empty list unless new warnings arrive.                                                                                             |
| `wait() -> ExportResult`                | Release the GIL and wait for completion or raise the worker's typed error. Exactly one caller may consume the result; later/concurrent waits raise `RuntimeError`, including after failure. |

Progress is throttled and its event queue is bounded, so intermediate snapshots
can be dropped. `wait()` retains received progress and warnings for subsequent
polling. `None` for total or estimated remaining time means no estimate is
available. No Python callback executes on a Rust worker thread.

```python
from insta360_rs import start_export_frames

job = start_export_frames("recording.insv", "frames", indices=[0, 30])
result = job.wait()
print(result.frames_written, job.backend().selected)
for warning in job.take_warnings():
    print(warning)
```

## Exceptions

All native operational exceptions inherit from `Insta360Error(Exception)`.

| Exception                    | Meaning                                                          |
| ---------------------------- | ---------------------------------------------------------------- |
| `Insta360IOError`            | Filesystem access failure; it is not an `OSError` subclass.      |
| `InvalidMediaError`          | Invalid input, framing, selection, dimensions, or interval.      |
| `UnsupportedCameraError`     | Camera unsupported by the requested operation.                   |
| `MissingCalibrationError`    | Required recorded calibration is unavailable.                    |
| `AmbiguousOpticalSetupError` | Automatic selection cannot choose one optical profile.           |
| `MissingCapabilityError`     | The operation requires unimplemented or unavailable support.     |
| `GpuUnavailableError`        | GPU initialization unavailable; also a `MissingCapabilityError`. |
| `MediaProcessingError`       | Native decode/encode/media processing failure.                   |
| `GpuProcessingError`         | GPU processing failure; also a `MediaProcessingError`.           |
| `CancelledError`             | Worker honored cancellation.                                     |

Invalid Python argument types raise `TypeError`; integers outside native
unsigned ranges raise `OverflowError` at conversion. In-range invalid values,
such as quality 0 or 101, raise `InvalidMediaError`. A duplicate `wait()` raises
`RuntimeError`. Preserve the message when reporting an error; it includes native
context.

`ExportJob.stabilization() -> str | None` reports the prepared file motion
profile, exposure clock strategy, and sensor readout status. `take_warnings()`
includes omitted automatic readout correction and retained factory calibration
whose bias semantics are unverified.
[Stabilization conventions](../../docs/stabilization.md) describe the X5
profile, six-axis limits, and low-level pose-table interface.
