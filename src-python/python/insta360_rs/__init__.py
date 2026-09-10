"""Media reading and file conversion interface to :mod:`insta360-rs`.

Processing stays inside the native Rust implementation. The Python layer only
normalizes a single path into the same path sequence accepted for legacy paired
recordings.
"""

from __future__ import annotations

from collections.abc import Sequence
from os import PathLike, fspath
from typing import TypeAlias

from ._native import (
    AmbiguousOpticalSetupError,
    AudioPolicy,
    BackendReport,
    CancelledError,
    Capabilities,
    ColorConversion,
    ConflictingOpticsError,
    DecodedVideoFrame,
    EffectiveBackend,
    EncodedPacket,
    Environment,
    ExportJob,
    ExportPhase,
    ExportProgress,
    ExportResult,
    ExtractionReport,
    GpuAdapterInfo,
    GpuFailure,
    GpuProcessingError,
    GpuUnavailableError,
    Housing,
    ImageFormat,
    Insta360Error,
    Insta360IOError,
    InvalidMediaError,
    LensAccessory,
    MediaAcceleration,
    MediaInfo,
    MediaProcessingError,
    MediaSource,
    MediaStream,
    MissingCalibrationError,
    MissingCapabilityError,
    MountingAccessory,
    OpticalInspection,
    OpticalResolution,
    OpticalSelection,
    PacketReader,
    ProcessingBackend,
    RollingShutterCorrection,
    Stabilization,
    StitchConfig,
    StreamInfo,
    StreamSideData,
    TrailerInfo,
    UnderwaterColorMode,
    UnderwaterColorOptions,
    UnsupportedCameraError,
    VideoFrameReader,
    VideoTrackInfo,
    __version__,
    _export_frames,
    _export_video,
    _extract,
    _open_media,
    _probe,
    _start_export_frames,
    _start_export_video,
    capabilities,
    mnn_runtime_version,
)

PathInput: TypeAlias = str | PathLike[str]
Inputs: TypeAlias = PathInput | Sequence[PathInput]


def _normalize_inputs(value: Inputs) -> list[str]:
    if isinstance(value, (str, PathLike)):
        return [fspath(value)]
    return [fspath(path) for path in value]


def probe(input: Inputs) -> MediaInfo:
    """Inspect an INSV recording without decoding its video streams."""

    return _probe(_normalize_inputs(input))


def extract(input: Inputs, output_dir: PathInput) -> ExtractionReport:
    """Extract streams, metadata, and original trailer records without stitching.

    A single input discovers its legacy sibling when present. ``output_dir``
    must be absent or empty; symbolic links and nonempty targets are rejected.
    The completed directory includes a JSON manifest and preserved raw data.
    This call blocks until completion and releases the GIL during extraction.
    """

    return _extract(_normalize_inputs(input), fspath(output_dir))


def open_media(input: Inputs) -> MediaSource:
    """Open recording streams for direct packet access and decoded video frames.

    No intermediate files are created. Each opened reader has its own position.
    A single path discovers its legacy sibling when present. Packet timestamps
    use the stream time base; frame and seek timestamps use seconds.
    """

    return _open_media(_normalize_inputs(input))


def export_video(
    input: Inputs,
    output: PathInput,
    *,
    config: StitchConfig | None = None,
    quality: int = 90,
    audio: AudioPolicy = AudioPolicy.COPY,
    start: float | None = None,
    duration: float | None = None,
    acceleration: MediaAcceleration = MediaAcceleration.AUTO,
) -> ExportResult:
    """Export a stitched video interval and block until it completes.

    ``acceleration`` controls encoder selection independently of the stitching
    backend selected by :class:`StitchConfig`. Decoding currently uses software.
    """

    return _export_video(
        _normalize_inputs(input),
        fspath(output),
        config,
        quality,
        audio,
        start,
        duration,
        acceleration,
    )


def start_export_video(
    input: Inputs,
    output: PathInput,
    *,
    config: StitchConfig | None = None,
    quality: int = 90,
    audio: AudioPolicy = AudioPolicy.COPY,
    start: float | None = None,
    duration: float | None = None,
    acceleration: MediaAcceleration = MediaAcceleration.AUTO,
) -> ExportJob:
    """Start a stitched-video export and return a cancellable job."""

    return _start_export_video(
        _normalize_inputs(input),
        fspath(output),
        config,
        quality,
        audio,
        start,
        duration,
        acceleration,
    )


def export_frames(
    input: Inputs,
    output_dir: PathInput,
    *,
    indices: Sequence[int] | None = None,
    timestamps: Sequence[float] | None = None,
    start: float | None = None,
    end: float | None = None,
    fps: float | None = None,
    config: StitchConfig | None = None,
    format: ImageFormat = ImageFormat.PNG,
    quality: int = 95,
    scale_width: int | None = None,
) -> ExportResult:
    """Export selected stitched frames and block until completion.

    Exactly one selection mode is required: ``indices``, ``timestamps``, or the
    complete ``start``/``end``/``fps`` sampled range.
    """

    return _export_frames(
        _normalize_inputs(input),
        fspath(output_dir),
        config,
        list(indices) if indices is not None else None,
        list(timestamps) if timestamps is not None else None,
        start,
        end,
        fps,
        format,
        quality,
        scale_width,
    )


def start_export_frames(
    input: Inputs,
    output_dir: PathInput,
    *,
    indices: Sequence[int] | None = None,
    timestamps: Sequence[float] | None = None,
    start: float | None = None,
    end: float | None = None,
    fps: float | None = None,
    config: StitchConfig | None = None,
    format: ImageFormat = ImageFormat.PNG,
    quality: int = 95,
    scale_width: int | None = None,
) -> ExportJob:
    """Start a selected-frame export and return a cancellable job."""

    return _start_export_frames(
        _normalize_inputs(input),
        fspath(output_dir),
        config,
        list(indices) if indices is not None else None,
        list(timestamps) if timestamps is not None else None,
        start,
        end,
        fps,
        format,
        quality,
        scale_width,
    )


__all__ = [
    "__version__",
    "AmbiguousOpticalSetupError",
    "AudioPolicy",
    "BackendReport",
    "CancelledError",
    "Capabilities",
    "ColorConversion",
    "DecodedVideoFrame",
    "EffectiveBackend",
    "EncodedPacket",
    "ExportJob",
    "ExportPhase",
    "ExportProgress",
    "ExportResult",
    "ExtractionReport",
    "GpuAdapterInfo",
    "GpuFailure",
    "GpuProcessingError",
    "GpuUnavailableError",
    "ImageFormat",
    "Insta360Error",
    "Insta360IOError",
    "Inputs",
    "InvalidMediaError",
    "MediaInfo",
    "MediaAcceleration",
    "MediaProcessingError",
    "MediaSource",
    "MediaStream",
    "MissingCalibrationError",
    "MissingCapabilityError",
    "Housing",
    "Environment",
    "LensAccessory",
    "MountingAccessory",
    "UnderwaterColorMode",
    "UnderwaterColorOptions",
    "OpticalSelection",
    "OpticalInspection",
    "OpticalResolution",
    "ConflictingOpticsError",
    "PacketReader",
    "PathInput",
    "ProcessingBackend",
    "Stabilization",
    "RollingShutterCorrection",
    "StitchConfig",
    "StreamInfo",
    "StreamSideData",
    "TrailerInfo",
    "UnsupportedCameraError",
    "VideoTrackInfo",
    "VideoFrameReader",
    "capabilities",
    "mnn_runtime_version",
    "export_frames",
    "export_video",
    "extract",
    "open_media",
    "probe",
    "start_export_frames",
    "start_export_video",
]
