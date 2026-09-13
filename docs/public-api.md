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

`gpu::GpuNv12Frame` borrows 8-bit Y and interleaved U/V planes with explicit
strides, matrix, range and chroma location.
`GpuStitcher::stitch_nv12_with_motion` and `stitch_nv12_to_yuv420_with_motion`
upload those planes directly, preserving the same calibration, masks, LUT and
motion policy as planar YUV420. The media renderer selects this path for FFmpeg
NV12 frames and leaves the borrowed original unchanged. P010 and unsupported
color layouts retain the existing conversion fallback; they are never
reinterpreted as 8-bit NV12.

`StitchConfig::backend` controls image reconstruction:

- `ProcessingBackend::Cpu` uses the deterministic reference renderer.
- `ProcessingBackend::Gpu` strictly requests the opt-in GPU renderer.
- `ProcessingBackend::Auto` attempts GPU, then reruns the complete operation on
  CPU only when initialization or processing returns a typed GPU failure.
  Attempt-owned image outputs or the video temporary file are removed before
  retry; non-GPU errors are returned without fallback.

`StitchConfig::seam_mode` selects `SeamMode::{Fixed,Dynamic,OpticalFlow,Ai}`.
`Fixed` is the default; it retains calibrated projection, housing exclusion and
the fixed seam. The other modes request adaptive alignment within the overlap,
independently of `ProcessingBackend`, optical accessories, motion and underwater
color. `SeamMode::unavailable_reason()` provides a cheap build/qualification
reason without loading a model or initializing a device. Source and runtime
validation still occurs during preparation; an explicit unavailable algorithm
does not silently fall back to Fixed. These names do not promise identical
Insta360 Studio results.

CLI exports use `--stitching-optimization off|dynamic|optical-flow|ai` (default
`off`). Python exposes `SeamMode.FIXED`, `DYNAMIC`, `OPTICAL_FLOW`, and `AI`
through `StitchConfig(seam_mode=...)`; each member provides
`unavailable_reason()`. Construction preserves an unavailable typed choice for
inspection; export validates its execution prerequisites.

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
worker, and reuses decoder resources. `next_pair` continues with exact source
samples without seeking, drains B-frames and crosses chapters. `seek_pair`
returns `None` after the last sample, distinguishing EOF from decode errors.
Packed sources retain one decoder. Export may use the single-demux
`paired::PairedReader`; interactive continuous playback can keep the same
`PairedPreviewReader` used for seeking. See
[recording sequences](recording-sequences.md).

Continuous hosts may call `prefetch_next(cancel)` before rendering their current
pair. It starts one decode and host transfer on each existing lens worker, so
preparing the next CPU-readable pair overlaps processing the retained pair.
Repeated calls are idempotent; `next_pair` consumes the pending pair with the
same exact PTS checks. Seeks, cancelled reads and reader destruction drain both
replies. Packed inputs keep their synchronous single-decoder path. Prefetch
never changes retained source pixels or supplies an application playback clock.

For playback cadence or clock catch-up, `next_selected_pair(selection, cancel)`
accepts `paired::PreviewSelection`. Its nonzero `advance` counts actual next
pairs, then its optional `not_before: Duration` advances to the first pair at or
after that recording time. An available final candidate survives EOF; an initial
EOF returns `None`. This is the same selection as repeated `next_pair` calls,
including errors for skipped pairs with missing or mismatched lenses. Lens
synchronization uses exact native PTS; `not_before` uses the pair's microsecond
display timestamp. Every returned pair remains CPU-readable.

Selection preserves decoding dependencies but avoids hardware-to-host transfers
for additional discarded candidates. The one eager prefetched pair may already
have been copied. `decode_stats()` returns cumulative `PreviewDecodeStats`
without changing selection or adding per-frame logging:

| Counter                      | Meaning                                                                                                     |
| ---------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `validated_pairs`            | Exact pairs accepted before selection, including skipped candidates.                                        |
| `materialized_pairs`         | Selected pairs returned with CPU-readable pixels.                                                           |
| `speculative_pairs`          | Completed eager-prefetch pairs consumed or drained, including discarded pairs.                              |
| `hardware_transfer_attempts` | Hardware frame transfers attempted on either lens worker, including speculative copies and failed attempts. |

Codec preroll is excluded. Prefetch counters become visible only after worker
replies are consumed or drained; discarding prefetch does not validate its pair.
Seeks retain cumulative counters. Software and packed paths have zero hardware
transfers, so `PreviewAcceleration::Auto` alone does not prove hardware use.

`RecordingFrameRenderer::render_continuous` uses a shared CPU pool capped at
four Rayon workers for seam preparation. Ordinary `render`, `render_strict`,
standalone `StitchPlanner` calls and file exports retain their usual scheduling.
This changes the interactive work budget only; prepared geometry and exact-pair
Save behavior are unchanged. No pool is initialized for Fixed stitching.

## Reusable exact-frame processing

Rust media hosts can retain a `FramePair` from either paired reader and borrow
it with `media::RecordingFrameRenderer::render`. The session accepts a
`RecordingSequence`, `StitchConfig`, output projection and cancellation flag; it
returns an owned RGB `PanoramaFrame`, actual `BackendReport`, and
`FrameRenderInfo`. The report includes resolved optics, warnings, and typed
calibration provenance (offset version/source, profile name and per-lens
polynomial normalization). Registry provenance remains available through
`profile::lens_profile`. These reports describe implemented evidence, not
physical-camera qualification.

The renderer neither decodes nor writes files. It borrows the original native
buffers without cloning or mutating them. Hosts retain responsibility for
pair/source association, scheduling, encoding, output publication and stale
preview checks. Session scalers remain on their owning processing thread.
`render` may retry an unpublished Auto frame on CPU after a typed GPU failure;
explicit GPU is strict. `render_strict` exposes processing failures so batch
hosts can roll back their owned outputs and restart the entire attempt. Existing
`Exporter` jobs retain whole-attempt fallback.

`RecordingFrameRenderer::preflight` validates every chapter's layout, decoder,
optics, motion, selected backend, output dimensions and required color assets
without creating outputs or requiring an HEVC encoder. An omitted projection
uses the config projection, then twice the decoded single-lens width by that
lens width. Hosts that will render should construct one session and call its
`prepare_all` method instead: it performs those same checks while retaining the
backend and loaded color resources for subsequent frames at the prepared size.
This avoids loading the AI models twice. Only the final chapter's motion stays
resident; earlier-frame requests replay preceding metadata. Failed or cancelled
preparation can be retried on the same session. The static `preflight` delegates
to this method and discards its session for callers needing reports only.
`inspect_frame_dimensions` performs cheaper layout-only inspection and reports
one lens's dimensions even for packed input. Its `FrameDimensions::scaled`
method resolves the native image path's even dimensions without upscaling.
Explicit panorama projections retain the SDK's existing ability to upscale;
applications can impose a no-upscale policy before calling it. Preview hosts can
skip `prepare_all` and let `render` prepare the requested chapter lazily;
exports must still preflight the entire requested scope before creating output.
Keep sessions alive across related requests to retain their motion, model and
GPU resources. Reapplying the same shared LUT (or `None`) does not invalidate
GPU frame allocations.

For an import panel, `inspect_motion_support` checks source timing and telemetry
across chapters without decoders, optical calibration, GPU initialization, or
color assets. It returns independent stabilization and rolling-shutter errors:
required readout is tried first; a failed readout check triggers a motion-only
pass so missing or unsupported readout does not hide supported stabilization.
Malformed timing remains an error under the normal motion validation rules. This
still reads bounded telemetry and fuses attitudes, so hosts should defer it when
immediate source selection matters. Only adjacent chapter motion is retained,
and cancellation is checked between synchronous chapter preparations.
`InsvInspection::audio_track_count` counts declared audio handlers without
opening decoders; `RecordingChapter::audio_track_count` sums all simultaneous
inputs during their existing header inspection. Neither establishes audio codec
or packet validity.

`validate_color_metadata(metadata, conversion, require_sdr)` checks the camera's
I-Log/Dolby declarations without loading LUTs. Set `require_sdr` for panorama
output or enabled restoration; encoded PQ/HLG checks remain in actual preflight.
`UnderwaterColorOptions::validate_dimensions` checks settings, compiled feature,
size and frame rate without models or allocation. These lightweight checks do
not replace strict runtime/resource validation before export.

`media::NativeColorProcessor` supplies the color-only route independently of
calibration and stabilization. Its `preflight` validates all chapters and
resources; `requires_processing(chapter_index)` resolves I-Log before deciding
whether the host can retain its direct native encoding path. Underwater Off
alone does not bypass an active I-Log LUT. `process` returns two packed RGB
`LensFrame`s: resize without upscaling, source matrix/range conversion, I-Log
conversion, then underwater restoration. Requested native widths are rounded
down to even dimensions; zero-sized results are rejected. Native lens geometry
is unchanged: housing exclusion masks and motion only affect panorama rendering.
HDR/PQ/HLG sources cannot enter this SDR color correction path; uncorrected
native access remains independent of panorama restrictions.

Every selected still, including batch images and both native lenses, starts with
reset restoration history. Continuous video retains history within each chapter.
Motion history is separate: file preparation continues preceding chapter
telemetry, even for a first request in a later chapter. Backward seeks replay
metadata preparation with only the active chapter retained, rather than
retaining every chapter's large telemetry arrays. Cancellation is checked
between preparation, rendering and inference operations; a single inference call
is not interruptible. Downscaled previews can differ photometrically from
full-size stills and continuous video because adaptive color depends on image
resolution and processed-frame history.

The renderer, chapter preparation, native RGB conversion and restoration stages
are shared with existing still/video exports. Video retains original audio
handling and GPU YUV output when underwater restoration is disabled.

These borrowed-FFmpeg-frame sessions are Rust host APIs. Python's existing file
export functions use the same rendering internals; the Python wrapper does not
expose native `FramePair` ownership or these session types.

## File export and calibration

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

## Continuous preview rendering

`NativeColorProcessor::process_continuous(pair, width, cancel)` and
`RecordingFrameRenderer::render_continuous(pair, projection, cancel)` process
borrowed exact pairs for a continuous preview. They neither decode nor seek.
Each native lens retains an independent restoration session; the second lens
session is allocated only when continuous processing is used. Panorama
processing retains its own history alongside reusable projection resources.

Call `reset_continuous()` before a seek or other source discontinuity. Both
renderers automatically reset for non-increasing PTS, chapter or output-size
changes, and failed/cancelled continuous frames. Reset retains loaded models and
working buffers. Typed Auto GPU fallback resets the unpublished frame's color
state before CPU retry. The existing `process`, `render` and `render_strict`
methods continue to produce independent still images; file export's existing
reference processing policy is unchanged.

Output dimensions are per-call inputs to these retained renderers. Adaptive
preview resizing keeps verified restoration assets, AI model sessions and fixed
inference tensors; Legacy resizes its image scratch while retaining the LUT.
Dimensions are validated before changing the active session, and the first
resized frame starts with fresh temporal state. Hosts should keep a renderer
when only preview size changes and recreate it when processing options change.

The new `UnderwaterColorSession::process_rgb8_continuous` uses source PTS and
frame rate to advance temporal smoothing and AI inference intervals across
frames omitted from preview presentation. Legacy smoothing exponentiates its
per-source-frame decay by elapsed frames. AI retains its 10-source-frame LUT and
60-source-frame feature intervals, performs at most one update per presented
frame, and adjusts LUT smoothing for elapsed source time. The existing
`process_rgb8` keeps per-processed-frame reference behavior. Changing policy or
resetting the session clears history. Reduced-resolution preview and dropped
frames can still produce different color from full-resolution export.

Tests compare native output with independent lens sessions, panorama output with
a separate restoration session, retained history with independent stills,
reset/policy behavior, skipped-frame smoothing and AI update cadence. Existing
reference-image and file-export tests remain required. These checks establish
temporal ownership and timing contracts; they do not establish 4K60 throughput,
physical-GPU performance or real-camera visual qualification.
