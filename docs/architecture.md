# Architecture

## Intent

`insta360-rs` separates the proprietary file-format knowledge from media I/O and
application frameworks. The standalone crate can therefore be used from Python,
command-line tools, or another Rust application without pulling an application
framework into the dependency graph.

The supported 0.1 data flow is:

```text
INSV → bounded container parser → camera registry + calibration/profile resolver
     → synchronized dual decoder → gyro/rolling-shutter transform
     → fixed equirectangular projection and calibrated blend
     → image or HEVC export
```

## Boundaries

- `container` owns ISO-BMFF discovery, INSV trailer records, and input grouping.
- `profile` owns the ONE, ONE R/RS, ONE X through X6 and X4 Air aliases, lens
  identifiers, projection generations, optical-setup mappings, fallback
  FOV/blend values, mask recipes, and evidence provenance. It contains no
  per-unit calibration.
- `calibration` parses recorded offset strings and selects an explicit optical
  profile. It resolves render geometry once and never substitutes a generic
  camera calibration.
- `assets` owns bundled and application-supplied `ModelBundle` manifests,
  compatibility selection, default-deny qualification, complete multi-file model
  groups, confined providers, SHA-256 verification, and portable parsing of the
  vendor's OpenCV linear SVM data.
- `optics` keeps housing, environment, lens accessory and mount independent,
  with requested/detected/effective reports.
- `underwater` owns reusable scalar Legacy and optional independent MNN AI
  sessions. Restoration changes RGB values after stitching without moving
  pixels.
- `color` verifies and parses bundled 3D CUBEs and applies trilinear RGB color
  transforms. `media` selects the X5 I-Log table from recording metadata and
  configures the CPU or GPU export session.
- `motion` owns quaternion primitives, X5 sensor normalization, gravity fusion,
  relative legacy integration, and bounded sensor readout pose tables. It has no
  decoder dependency and explicitly uses body-to-world poses with world Z up.
- `timing` pairs raw exposure timestamps with bounded BMFF presentation tables.
  `media::stabilization` combines that map with the sensor profile and full IMU
  pre-roll once per export attempt. Renderers receive poses, not metadata
  clocks.
- `stitch` accepts library frame buffers. It owns optical validity, the fixed
  geometric seam, and radiometric overlap compensation, but has no FFmpeg or
  application types in its API.
- `gpu` is optional and owns the safe `wgpu` compute renderer, GPU-visible
  calibration data, input uploads, reusable frame resources, and readback. It
  does not own FFmpeg or native codec handles.
- `media` is optional and owns FFmpeg, bounded pipeline queues, jobs, progress,
  cancellation, and atomic outputs.
- `stream` exposes file-backed stream descriptors and independent packet/video
  readers. Seeking and decoded RGB frame access create no intermediate files;
  encoded packet access does not decode or re-encode.
- `extraction` copies all demuxed streams, container metadata, and V2/V3 tail
  records into an owned staging directory, then publishes a complete folder and
  manifest. Stream-copy extraction has no calibration or camera-family gate.
- `src-python` maps file-oriented operations to PyO3. It exposes owned packet
  and RGB frame bytes, never native-library pointers.

The main media layers return the crate's typed `Error`; optional assets use the
more specific `AssetError`. Vendor binaries are static evidence or external test
oracles and are never loaded by the production library. Licensed data assets are
embedded by the five data crates and served through `BundledAssetProvider`;
applications can also provide external bundles.

## Calibration policy

Factory calibration is part of the recording. The resolver preserves every
available offset and selects the newest valid representation in this order: V6,
V3, V2, then V1. It retains recorded crop/layout for dispatch, applies recorded
track order, and validates the explicitly selected accessory/medium profile. The
shared resolver normalizes supported sensor windows before housing conversion.
Source housing exclusion remains a radial prepared mask. Both renderers consume
the same resolved coordinates and mask field.

Resolution authority is explicit: an explicit caller optical setup, then the
recorded `offset_state` and conclusive automatic `guard_detected_type`, then the
encoded lens ID. Fallback FOV and blend constants do not replace per-unit
calibration. Supported housing conversion derives target intrinsics and radial
distortion from the selected source calibration and verified physical model,
preserving principal points and extrinsics. INSV tag 128 overrides the
registered blend-angle fallback.

Automatic optical selection resolves an explicit recorded accessory state before
considering the lens ID. If an automatic guard state has no conclusive detection
result, or the requested conversion is not implemented, it fails before
rendering and requires the caller to choose. This prevents an air calibration
from being silently used for an underwater dive-case recording.

## Geometric stability

The stitcher uses one static projection and high-frequency seam layout for the
entire recording. Gyro correction changes camera orientation but not local scene
geometry. Low-frequency color compensation is estimated per frame, but it
changes only radiometry: detail ownership and projected coordinates remain
fixed. AI and dynamic optical-flow seams are excluded until they can be shown
not to regress downstream reconstruction.

CPU and GPU consume the same `ResolvedLensGeometry`; neither renderer contains a
camera-ID FOV/blend switch. Accessory-specific source masks are constructed from
the selected registry recipe and the recording's calibration.

Radiometric statistics and gain ramps live on a sphere whose north pole is the
first calibrated lens's optical axis. Rendering converts the rotated camera ray
into that same sphere and interpolates cyclic longitude, so stabilization
carries color correction with the scene and the ramps stay neutral near each
lens axis. Low-frequency blending also respects the source masks and normalizes
valid filter support.

## Performance

Container probing uses bounded reads. Media processing demuxes once and keeps at
most a bounded set of unmatched decoded frames while synchronizing the two
tracks. CPU rows use Rayon; a sparse deterministic overlap prepass estimates
color gains.

The optional GPU renderer uses Metal on macOS, D3D12 on Windows, and Vulkan on
Linux through `wgpu`. It retains a device, queue, compiled pipelines, textures,
buffers, and bindings across equal-size frames. Its current data flow is:

```text
software FFmpeg decode to retained AVFrames
  -> direct YUV420P plane upload, or CPU RGB conversion for another format
  -> GPU radiometric passes + projection/mask/seam/two-band blend
  -> RGB readback for stills
     or GPU RGB-to-YUV420 + YUV readback/copy into an FFmpeg frame for video
  -> software or hardware HEVC encoder selected by MediaAcceleration
```

GPU submission and readback are synchronous and use one reusable resource set,
so decode, compute, readback, and encode do not yet overlap. Export decoding
uses software FFmpeg; random-access previews separately support hardware
decoding with software fallback. Two/three-slot asynchronous export pipelining,
native surface import/export, and zero-copy encoding are not implemented.

`ProcessingBackend::Auto` attempts GPU first. A typed GPU initialization or
processing failure discards every output owned by that attempt and reruns the
complete requested export on CPU. It does not fall back for cancellation,
invalid media/calibration, unrelated I/O, or codec errors. Explicit `Cpu` and
`Gpu` remain strict. This whole-job boundary prevents a successful
photogrammetry result from mixing renderer output.

Cross-platform real-X5 qualification remains incomplete even though automatic
fallback is implemented. GPU submission/readback is still synchronous, and no
end-to-end speedup has been established.

Video is muxed into `<output>.insta360-rs-part`. This working file intentionally
lacks the final MP4 trailer while encoding is active and is not a playable
deliverable, even if its suffix is changed to `.mp4`. Only after the encoder is
flushed and the trailer is written does the exporter close the muxer and
atomically rename it to the requested output. The temporary-file guard removes
an incomplete job-owned file on failure, cancellation, or an automatic CPU
restart.
