# Testing strategy

See [CI](CI.md) for the automated test matrix and commands for running it
locally.

## Public fixtures

Unit and property tests use generated ISO-BMFF boxes, trailer records,
calibration strings, gyro sequences, and dual-fisheye calibration charts. They
use generated recordings and the embedded resources. Bundled-asset tests verify
the licensed payloads checked in under `data/*/assets/`, including consistent
manifests and unique paths across both data crates.

## Licensed integration corpus

Large recordings are supplied out of tree through `INSTA360_RS_FIXTURE_DIR`. The
initial corpus contains the supplied X5 dive-case recording. Release
qualification also requires another X5 unit, air and water profiles, 8- and
10-bit media, corrupt/truncated inputs, and a legacy paired recording.

## Required checks

- Rust formatting, Clippy, unit tests, documentation tests, and release builds
  using the compiler selected by `rust-toolchain.toml`.
- Parser fuzz targets and bounded-allocation regression cases.
- Component extraction: V2/JSON and V3 indexed/sequential tails, unused zero
  directory slots, opaque/repeated records, corrupt framing, and original raw
  bytes. Compare original packet boundaries and bytes with raw artifacts and
  video/audio container copies; cover subtitles, attachments, side data, and
  unsupported muxer fallback.
- Direct stream readers: independent packet/decoder cursors, B-frame draining,
  seeking after EOF, backward seeks, per-track timing, and absence of
  intermediate files. The `read_stream` example reads real lens frames in
  memory.
- Extraction destination ownership: reject nonempty/symlink targets, publish
  only complete output, and remove partial first-input artifacts if a companion
  fails. Python extraction and stream tests run against the built extension with
  `python -m unittest discover -s src-python/tests`.
- Exact stitch-metadata tag/wire parsing, bounded unknown-field retention, and
  invalid crop/field-number cases, including tag-64 exposure-file alignment
  against actual PTS, signed pre-roll, nearest-frame anchoring, and clock drift.
- Camera alias/lens/profile registry completeness, shared-ID ambiguity, exact
  FOV/blend provenance, and recorded-blend precedence.
- Analytical gravity/gyro trajectories around every axis, irregular sample
  intervals, upside-down starts, rejected acceleration, optional stationary
  bias, angular winding, saturation, telemetry gaps, and strict coverage.
- X5 mounting against twelve independent optical camera rotations, covering both
  signs of all three axes. Consecutive real-video frames must confirm reduced
  scene rotation; a plausible upright still does not validate mounting.
- Forward-generated rolling-shutter scenes with known world rays; corrected
  error must improve over disabled correction. Check scan direction/crop,
  per-lens timing, zero-motion equivalence, and actual CPU/GPU parity.
- CPU/GPU geometric equivalence within interpolation tolerance.
- Color correction under exact panorama rotations, cyclic gain interpolation,
  exclusion of masked pixels from low-frequency blending, GPU smoothing at
  widths smaller than its kernel, and RGB outputs with odd heights.
- Unequal positive lens exposures must not become black through correction
  extrapolation. Optical-axis brightness stays neutral on CPU and GPU. Changing
  pixels in the asymmetric lower dive-case housing must not change the panorama.
- Synchronized B-frame decoding stopped at every selection endpoint, including
  decoder flush, plus excessive timestamp divergence and smaller-gap recovery.
- Manifest validation, confined asset paths, SHA-256 vectors, compatibility
  default-deny policy, complete group loading, bundled source/stored digest
  equality, embedded resource completeness, and trained linear-SVM evaluation.
- X5 V6 physical-profile conversion, lens-type/FOV selection, spherical alpha,
  finite 180-degree hard seams, source-mask radius interpolation, and
  deterministic color-adjustment ramps.
- Clean Python wheel installation and file-conversion smoke tests.
- Cancellation, backpressure, atomic output, and complete-file soak tests.
- Sequence video export: split/unsplit decoded pixel equality, a single encoder
  and renderer across boundaries, recording-wide clipping, copied audio packet
  identity and sub-millisecond A/V offsets in full and clipped exports,
  continued gravity pose/unwrapped heading, and strict rejection of conflicting
  telemetry or reset camera clocks. See
  [sequence stitching](sequence-stitching.md).

## GPU qualification status

Current focused tests cover adapter discovery, synthetic RGB CPU/GPU parity,
same-adapter repeatability, neutral planar YUV420P upload, and encoder-layout
YUV420P output. A successful test on one Metal adapter proves only that the
portable vertical slice can execute there; it does not establish cross-platform
real-X5 qualification or an end-to-end speedup.

`ProcessingBackend::Auto` now chooses GPU first. Focused fault-injection tests
cover whole-operation CPU restart for typed GPU failures, strict explicit
backends, no retry for unrelated errors, and cleanup limited to attempt-owned
outputs. Release qualification must extend that coverage to real initialization
and mid-export failures on Metal, D3D12, and Vulkan and must run the real X5
midpoint geometry, radiometry, direction-lock, and seam corpus.

Hardware decode, asynchronous two/three-slot execution, and native zero-copy
surfaces are not implemented and therefore are not qualified. Encoder testing
must treat each `MediaAcceleration` policy and codec independently: `Hardware`
and `Software` are strict, while `Auto` retries eligible candidates on
configuration/opening failure. Mid-stream encoder restart coverage remains open.
Performance qualification requires matched release-build warm-run medians;
current functional tests make no speed claim.

Bundled-asset tests load every embedded payload through the verified provider,
check source/stored digest equality and the complete eight-member CoreML group,
and parse every bundled SVM. Provider tests cover invalid paths and missing
resources. These checks use embedded resources and do not execute vendor code.
Model possession is not an inference qualification: accessory preprocessing, AI
seam tensors, and restoration stages require their own labeled/golden corpus.

Atomic-output tests treat `.insta360-rs-part` as an internal incomplete file,
not a playable preview. Video success requires encoder flush, MP4 trailer write,
muxer close, and final rename; cancellation, GPU retry, and failure must remove
the attempt-owned temporary artifact.

Real-output iteration uses a cheap still gate before each video encode. For the
supplied X5 sample, the centered acceptance clip is 15 seconds at 1920×960. Its
midpoint is inspected for full coverage, dome/rim leakage, duplicated features,
seam-gradient spikes, horizon behavior, and local luma/chroma jumps.

## Bundled color transform

The color consumer has independent trilinear references and actual original-LUT
values, bounds/malformed-input cases, and all three bundled CUBE parse checks.
Recording-marker tests distinguish I-Log from legacy log and reject conflicting
or malformed capture declarations. Media session tests prove image and video
paths apply the selected asset. GPU tests compare the converted panorama with
CPU LUT evaluation within one code value; a primary-color test checks the CPU
BT.709 encoder matrix/range. See [asset usage](asset-usage.md) for the generated
INSV/FFmpeg comparison results.
