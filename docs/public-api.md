# Public API

## Feature layers

The default crate is deliberately independent of FFmpeg and application
frameworks. It exposes:

- `container`: bounded INSV/ISO-BMFF inspection, input grouping, indexed record
  access, stitch-dispatch metadata, factory offsets, profiles, and telemetry
  descriptors. Unknown protobuf fields are retained within explicit limits.
- `profile`: ONE, ONE R/RS, ONE X through X6 and X4 Air camera aliases and
  evidence-backed lens/setup, FOV, blend-angle, projection-generation, and
  mask-recipe records.
- `calibration`: current/original offset selection, native projection parsing,
  setup validation, and camera-neutral resolved render geometry.
- `color`: verified CUBE loading, trilinear RGB sampling, and parallel RGB8
  application.
- `assets`: bundled and application-supplied model manifests/providers, policy,
  camera/lens/target compatibility, SHA-256 verification, and the trained
  linear-SVM mathematical boundary.
- `motion`: legacy raw/common gyro decoding and `Stabilizer`, plus calibrated
  Z-up `AttitudeTrack`, `FusionOptions`, X5 IMU normalization, `FrameMotion`,
  and per-lens `ReadoutPoseTable`. The legacy decoder still rejects mode 2
  because its signature has no exposure/PTS input; file exports use the complete
  map.
- `telemetry`: media-relative exposures and signed camera-clock exposure
  samples.
- `timing`: `ExposureTimeline` associates exposure records with actual video
  presentation order. `InsvReader::video_presentation_timestamps()` expands
  bounded `stts`/`ctts` tables with supported edit origins without reading
  `mdat`.
- `stitch`: owned RGB8 lens/panorama frames and deterministic CPU stitching.

The `media` feature adds FFmpeg decoding, selected PNG/JPEG export, HEVC video
export, progress, cancellation, and atomic output. The `cli` feature adds the
conversion executable. Licensed Insta360 and Studio resources are embedded in
the default build through `BundledAssetProvider`. The `gpu` feature adds adapter
discovery and an explicit portable `GpuStitcher` implemented with safe `wgpu`
compute on the platform backend.

`StitchConfig::backend` controls image reconstruction:

- `ProcessingBackend::Cpu` uses the deterministic reference renderer.
- `ProcessingBackend::Gpu` strictly requests the opt-in GPU renderer.
- `ProcessingBackend::Auto` attempts GPU, then reruns the complete operation on
  CPU only when initialization or processing returns a typed GPU failure.
  Attempt-owned image outputs or the video temporary file are removed before
  retry; non-GPU errors are returned without fallback.

`StitchConfig::rolling_shutter` selects
`RollingShutterCorrection::{Auto,Off,Required}` (default `Auto`, also when
deserializing older configurations). Stabilization `Off` bypasses motion;
combining it with required readout correction is an error.
`ExportEvent::StabilizationPrepared` describes the selected profile and timing;
`Warning` reports omitted automatic readout correction. Low-level CPU/GPU
`stitch_with_motion` takes caller-provided camera-body poses independently of
file profiles. See [stabilization](stabilization.md) for equations and limits.

`VideoExportOptions::acceleration` is independent. Its `Auto`, `Software`, and
`Hardware` policies currently control HEVC encoder selection, not stitching.
Explicit `Hardware` fails if no named hardware encoder can be opened. Export
decoding remains software, while native random-access previews have a separate
hardware-decoding policy. Native GPU codec surfaces are not exposed. `Auto`
tries all eligible encoder candidates in its preference order when
configuration/opening fails. It does not yet restart after a mid-stream encoder
failure.

`VideoExportOptions::start` and `VideoExportOptions::duration` select a
source-relative half-open interval `[start, start + duration)`. Either may be
omitted: start defaults to zero and an omitted duration runs to end of source.
The decoder seeks to an earlier keyframe when necessary, discards pre-roll, and
rebases the first encoded frame to timestamp zero.

For interactive fisheye preview, `paired::PairedPreviewReader::frame_at` returns
the first exact native A/B pair at or after a recording time. It accepts
`PreviewAcceleration::Auto` or `Software`, runs each lens on its own bounded
worker, and reuses random-seek resources. Continuous processing uses
`paired::PairedReader` instead. See
[recording sequences](recording-sequences.md).

`Exporter::from_sequence` stitches a validated `RecordingSequence` through one
renderer and MP4 writer. `preflight_video` validates the implemented options for
every chapter without writing files. `ExportEvent::EncoderSelected` reports the
opened encoder and copied audio count. `AudioPolicy::Copy` preserves compatible
AAC/ALAC packet payloads on the shared video timeline, with complete-packet
cuts; `Drop` omits audio. See [sequence stitching](sequence-stitching.md) for
preflight, chapter motion continuity, clipping, unsupported layouts and
qualification.

GPU export consumes retained decoded YUV420P planes directly when possible and
can return encoder-ready YUV420P after GPU stitching. Other decoded layouts use
CPU RGB conversion before upload. Both image RGB and video YUV results are read
back synchronously; the public API does not yet expose asynchronous GPU frame
slots or zero-copy surfaces.

`ResolvedCalibration::lens_geometry` is resolved once. CPU and WGSL consume the
same half-FOV and blend angle, while per-unit intrinsics/extrinsics remain from
the chosen offset. Recorded blend metadata takes precedence over a registry
fallback. Camera recognition covers X1-X6; this does not claim every
camera/codec/accessory combination is release-qualified.

`ParsedLens::polynomial_projection` holds shared V1/V2 radian coefficients,
dimensionless focal scale and `PolynomialCoefficientSource` provenance while
retaining raw native fields. Parsing prepares valid polynomials; after changing
native coefficients, model or lens ID, call `refresh_polynomial_projection()`.
`NormalizedPolynomialProjection::validate()` checks its numeric domain;
`ParsedLens::validate()` also verifies consistency with the native fields. Older
serialized lenses without the optional prepared field remain inspectable and
need a refresh before rendering. CPU and GPU reject invalid or stale
normalization before producing output. See [calibration](calibration.md).

`BundledAssetProvider::manifest()` returns the validated manifest for the 51
embedded Insta360/Studio payloads. `BUNDLED_MANIFEST` exposes its original JSON,
and `BundledAssetProvider` loads payloads without filesystem access.
Applications can continue to supply directory or in-memory providers.

`ModelBundle::load_verified_for` applies `Disabled`, `Automatic`, or `Required`
policy after camera/lens/target compatibility selection. It validates byte
length and SHA-256 before returning a `VerifiedAsset`. OpenCV SVM XML can be
parsed and its linear decision function evaluated for an already-preprocessed
feature vector; image preprocessing and classifier availability remain outside
the contract until qualified.

Asset qualification is explicit and default-deny; empty compatibility dimensions
become wildcards only when an application marks the complete algorithm
`Qualified`. `AssetGroupDescriptor` records every constituent of a multi-file
model, and `load_verified_group_for` returns only a complete verified group.
Imported vendor data remains `Unqualified` until the application completes its
camera/lens/target qualification.

`MediaCapabilities` reports whether GPU support was compiled, discovered adapter
descriptions, an unavailable reason, and HEVC encoder names. Adapter discovery
means a compute provider exists; it is not a claim that the adapter, driver,
camera profile, and codec combination has passed release qualification.
`ExportResult::backend` records the requested and selected stitch renderer and
adapter/fallback information.

Video output uses a sibling `<output>.insta360-rs-part` path for atomic
publication. That file is an internal, incomplete muxing artifact while the job
runs: its MP4 trailer has not been written, so renaming it to `.mp4` does not
make it a valid completion check. `ExportJob::wait()` succeeding and returning
the final requested path is the completion contract. The writer flushes the
encoder, writes the trailer, closes the muxer, and atomically publishes the file
without replacing an existing destination, including one created while the job
runs. Failure and cancellation remove the temporary artifact. Image publication
uses the same no-overwrite operation.

Both stitched exports and original stream readers propagate demuxer and file
read errors. Decoded previews and stitched exports reject frames that FFmpeg
marks corrupt, while encoded packet access preserves the original corruption
flags for inspection. Decode allocations have a 128-megapixel limit, and CPU
conversion checks allocation and scaling results before accessing output planes.

Sampled image ranges calculate each target independently from its ordinal and
milliframe rate, rounding to the nearest microsecond. This prevents cumulative
timestamp drift during long recordings. A range includes its end only when the
end lies on its sampling grid; at most one million targets are accepted.

## Original streams and component extraction

The `media` feature also exposes `MediaSource`, `MediaStream`, and `extract`.
These operations do not construct an `Exporter` and do not require camera or
calibration validation. `MediaSource::open(InputSet)` enumerates file-backed
streams; each stream opens independent encoded-packet or decoded-video readers.
Readers support seeking without producing intermediate files. Original packet
payloads retain their codec configuration and signed timestamps; explicitly
decoded frames are tightly packed RGB24.

`extract(&InputSet, output_dir)` synchronously writes all demuxed stream
packets, timing indexes, side data, codec parameters, container metadata,
ExtraInfo, and individual records. Compatible video/audio tracks get additional
playable copies without re-encoding. It returns `ExtractionReport` with absolute
file paths, counts, and warnings. The destination must be absent or empty;
staged output is published only on success. See
[stream access and extraction](extraction.md) for the complete API examples and
manifest/path contract.

## Error contract

Malformed or truncated input returns `Error::InvalidMedia`. Missing recorded
calibration and unavailable accessory conversions are distinct from an
unsupported camera or an optional backend that was not compiled. Export never
silently changes optical setup or stabilization. Explicit `Cpu` and `Gpu`
requests never change backend; `Auto` follows its documented GPU-first,
whole-job CPU fallback contract and records the selected renderer in
`ExportResult::backend`.

The error enum and enums marked `#[non_exhaustive]` require a fallback arm. Data
structures are serializable where that is useful for CLI or IPC boundaries. Core
geometry and motion APIs use library-owned values, and `wgpu` types remain
private. With `media`, `FramePair` exposes FFmpeg-owned video frames and native
timestamp rationals; `FramePairIdentity` retains those rationals, and
`PairedReader::take_audio_packets` returns original FFmpeg packets. These native
Rust interfaces are not exposed by the Python package.

## Ownership

`InputSet` owns normalized input paths. `InsvReader` works with any
`Read + Seek` source and bounds record allocations. Stitching's RGB8 image
buffers and `MediaSource` RGB24 previews are owned and tightly packed. Native
`FramePair` buffers instead retain their decoded pixel format and plane strides;
consumers must inspect those properties when accessing their pixels.

`StitchConfig::color_conversion` defaults to `ColorConversion::Auto`, which
selects the bundled X5 LUT for explicit I-Log metadata. `Preserve` disables the
transform; `ILogToRec709` selects it for unmarked X5 I-Log inputs. GPU callers
can also set a table directly through `GpuStitcher::set_color_lut`. See
[runtime asset usage](asset-usage.md) for metadata precedence and scope.

## Housing and underwater APIs

`OpticalSelection` separates `Housing`, `Environment`, `LensAccessory` and
`MountingAccessory`. The same four fields appear on `StitchConfig` and default
to Auto. Explicit components override detected components; incompatible
combinations fail with `ConflictingOptics`. The removed `OpticalSetup` enum has
no compatibility alias. Serialized unknown configuration fields are rejected.
`CalibrationResolver::inspect_metadata_optics` reports detected choices and
ambiguity without requiring a supported render model. `MediaInfo.optics` exposes
that bounded inspection; resolved calibration and exports retain
`OpticalResolution` including requested/effective values and source/target IDs.

`profile::physical_curve` and `profile::physical_curves` exposes exact recovered
physical coefficients; `profile::housing_references()` preserves additional
official references and explicit limitations. The executable registry generates
the [housing catalog](housing-catalog.md) through the `housing_catalog` example.
Its test checks the documentation equals the code-generated table.

`UnderwaterColorOptions` validates mode-specific parameters.
`underwater::UnderwaterColorSession::prepare` accepts options, dimensions,
rational frame rate and an `AssetProvider`. `process_rgb8` operates in place on
packed RGB8 and a finite timestamp; `reset` clears temporal history while
retaining prepared resources. Legacy works without native dependencies; AI
requires `underwater-ai` and `MNN_ROOT` during build. Off is the default and
does not load assets. Invalid frame lengths/timestamps fail before mutating
pixels or state. See [housings](housings.md) for limits, defaults and source
evidence.
