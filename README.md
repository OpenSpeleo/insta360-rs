# insta360-rs

Portable Rust tooling for inspecting, calibrating, stitching, and exporting
Insta360 INSV media on macOS, Windows, and Linux.

`insta360-rs` is designed for geometry-stable 360° output, with underwater
photogrammetry as its primary use case. It reads the factory calibration and
capture metadata stored in each recording, stitches through a deterministic CPU
renderer or a portable `wgpu` compute renderer, and can export equirectangular
images or HEVC MP4 video through FFmpeg.

> **Project status: experimental 0.1.** Packet-preserving stream and metadata
> extraction is camera-independent and accepts one- or two-file input sets.
> Decoded stitching, image export, and stitched MP4 export are currently
> implemented only for X5 single-file, dual-track recordings. The parser
> recognizes ONE X through X6 metadata and calibration records. The API may
> change before 1.0.

The project is independent and is not affiliated with or endorsed by Insta360.
It does not link or execute vendor runtime libraries. Licensed Insta360 and
Studio data resources are embedded in an integrity-checked bundle; possessing
those assets does not imply that their algorithms are implemented or qualified.

## What it does

- Probes large INSV files without scanning their complete video payload.
- Opens file-backed stream objects for encoded packet access and seekable frame
  decoding without intermediate files.
- Extracts every demuxed stream, packet timing/index, codec extradata, side
  data, container metadata, and proprietary ExtraInfo record without decoding,
  stitching, or transcoding. Compatible streams also receive best-effort
  standalone codec-copy remuxes.
- Parses ISO-BMFF tracks and the indexed Insta360 trailer.
- Retains camera name, firmware, serial, layout, codec, crop, rotation, timing,
  gyro/exposure record descriptors, accessory state, optical profiles, and
  current/original factory offsets.
- Parses V1, V2, V3, and V6 offset layouts; V2/V3/V6 have portable projection
  implementations, while V1 remains inspection-only.
- Uses per-recording intrinsics, distortion, principal points, extrinsics, and
  embedded optical-profile curves instead of substituting generic calibration.
- Resolves X5 Dive Case Pro underwater metadata to the correct refractive
  profile when the required physical curves are present.
- Produces fixed-geometry 2:1 equirectangular panoramas with deterministic
  masks, seams, and low-frequency overlap color matching.
- Converts identified X5 I-Log footage to Rec.709 through the bundled Studio 3D
  LUT on both CPU and GPU, or leaves stitched I-Log values LUT-untransformed for
  downstream grading.
- Exports selected PNG/JPEG frames through the Rust API and PNG frames through
  the CLI.
- Exports finalized 8-bit YUV420 HEVC MP4 video through available FFmpeg
  encoders.
- Runs calibrated stitching on CPU everywhere or through native `wgpu`
  Metal/D3D12/Vulkan compute backends.
- Provides progress events, cancellation, bounded pipeline queues, whole-job
  GPU-to-CPU fallback, and atomic output publication.
- Offers PyO3 bindings for Python 3.10+.

Packet-preserving extraction does not spatially split a packed dual-fisheye
frame. The crate does **not** currently copy audio into stitched MP4 output, use
hardware decoding, run AI seam inference or ColorPlus, apply general crop-aware
optical projection, or preserve 10-bit depth in stitched output. Supported X5
recordings have gravity-referenced stabilization and sensor readout correction
on both CPU and GPU; see [stabilization](docs/stabilization.md).

## Camera support

Legend:

- ✅ — implemented in the current public API for the stated scope.
- ❌ — unavailable in the current implementation.

The distinction matters: recognizing a camera, lens ID, or offset layout is not
the same as being able to decode and export that camera's recording. Firmware
and recording mode can also change the physical layout and codec.

Housing columns report camera-scoped calibration-profile recognition, not
high-level file export. High-level image and video export remains X5-only.

| Camera | Typical layout                           | Metadata probe | Encoded camera-stream extraction⁷ | Container / ExtraInfo metadata extraction⁷ | Offset/distortion parsing | Calibrated high-level render | Stitch / image / MP4 export | Overlap color matching¹ | I-Log → Rec.709¹ | ColorPlus / AI color¹ | CPU file export | wgpu file export | Direction Lock / FlowState² | Rolling shutter⁸ | 10-bit stitched output | Waterproof profile | Classic dive air profile | Classic dive underwater profile | X3 Invisible Dive Case air profile | X3 Invisible Dive Case underwater profile | X5 Dive Case Pro air profile | X5 Dive Case Pro underwater profile |
| ------ | ---------------------------------------- | :------------: | :-------------------------------: | :----------------------------------------: | :-----------------------: | :--------------------------: | :-------------------------: | :---------------------: | :--------------: | :-------------------: | :-------------: | :--------------: | :-------------------------: | :--------------: | :--------------------: | :----------------: | :----------------------: | :-----------------------------: | :--------------------------------: | :---------------------------------------: | :--------------------------: | :---------------------------------: |
| ONE X  | Split pair ≥5.7K; one packed file below³ |       ✅       |                ✅⁷                |                    ✅⁷                     |            ✅⁴            |              ❌              |             ❌              |           ❌            |        ❌        |          ❌           |       ❌        |        ❌        |             ❌              |        ❌        |           ❌           |        ✅⁵         |           ✅⁵            |               ✅⁵               |                 ❌                 |                    ❌                     |              ❌              |                 ❌                  |
| ONE X2 | Split pair ≥5.7K; one packed file below³ |       ✅       |                ✅⁷                |                    ✅⁷                     |            ✅⁴            |              ❌              |             ❌              |           ❌            |        ❌        |          ❌           |       ❌        |        ❌        |             ❌              |        ❌        |           ❌           |         ❌         |           ✅⁵            |               ✅⁵               |                 ❌                 |                    ❌                     |              ❌              |                 ❌                  |
| X3     | Split pair ≥5.7K; one packed file below³ |       ✅       |                ✅⁷                |                    ✅⁷                     |            ✅⁴            |              ❌              |             ❌              |           ❌            |        ❌        |          ❌           |       ❌        |        ❌        |             ❌              |        ❌        |           ❌           |         ❌         |           ✅⁵            |               ✅⁵               |                ✅⁵                 |                    ✅⁵                    |              ❌              |                 ❌                  |
| X4     | Single-file dual-track                   |       ✅       |                ✅⁷                |                    ✅⁷                     |            ✅⁴            |              ❌              |             ❌              |           ❌            |        ❌        |          ❌           |       ❌        |        ❌        |             ❌              |        ❌        |           ❌           |         ❌         |            ❌            |               ❌                |                 ❌                 |                    ❌                     |              ❌              |                 ❌                  |
| X5     | Single-file dual-track                   |       ✅       |                ✅⁷                |                    ✅⁷                     |            ✅⁴            |              ✅              |             ✅              |           ✅            |        ✅        |          ❌           |       ✅        |        ✅        |             ✅²             |       ✅⁸        |           ❌           |         ❌         |            ❌            |               ❌                |                 ❌                 |                    ❌                     |             ✅⁶              |                 ✅⁶                 |
| X6     | Single-file dual-track                   |       ✅       |                ✅⁷                |                    ✅⁷                     |            ✅⁴            |              ❌              |             ❌              |           ❌            |        ❌        |          ❌           |       ❌        |        ❌        |             ❌              |        ❌        |           ❌           |         ❌         |            ❌            |               ❌                |                 ❌                 |                    ❌                     |              ❌              |                 ❌                  |

1. X5 file export performs fixed-seam, low-frequency overlap radiometric
   matching. `ColorConversion::Auto` applies the bundled X5 I-Log-to-Rec.709
   CUBE when nested recorded-color metadata identifies I-Log, with the legacy
   exact `I_Log` gamma string as a fallback. `Preserve` disables conversion;
   `ILogToRec709` requests it explicitly and rejects conflicting Standard/Dolby
   metadata. This is not factory sensor profiling, ColorPlus, or AI color.
2. X5 Direction Lock fixes the initial heading and levels the horizon.
   FlowState- style leveling preserves camera heading. Both use compact raw IMU
   samples, recorded ranges, reliable initial gravity, and validated video
   timing, including tag-64 value 2 exposure mapping. This is our six-axis
   filter; absolute compass heading and vendor FlowState equivalence are not
   claimed.
3. ONE X through X3 use a `_00_`/`_10_` pair at 5.7K and above, and one packed
   file below 5.7K. Probe the actual inputs rather than selecting layout from
   camera name alone. Pairs are accepted by parser/probe, not by the current
   high-level exporter.
4. V1/V2/V3/V6 records can be inspected. V2/V3/V6 projection data is renderable
   by the low-level stitcher; V1 is rejected by stitch preflight. Only X5 V6 has
   real-recording stitch qualification.
5. The registry recognizes an offset already encoded for this housing. X1-X3
   housing conversion and high-level file export are not implemented.
6. X5 accepts already-converted lens types 117/118. It can also convert a V6
   bare calibration when both source and target six-coefficient physical curves
   are present. `StrictAuto` uses only conclusive recorded accessory metadata.
   CLI/API select these Pro profiles as `invisible-dive-case-air` and
   `invisible-dive-case-underwater`; no separate `x5-dive-case-pro-*` option
   exists.

7. Extraction requires `media` and a suitable FFmpeg demuxer/muxer build, but no
   supported camera profile, calibration, stabilization, encoder, or GPU. It
   preserves every demuxed packet payload with boundaries/timestamps, codec
   extradata, side data, stream/container metadata, non-`mdat` boxes, and raw
   ExtraInfo bytes without decoding or re-encoding. V2/V3 metadata is also
   emitted as JSON/calibration artifacts when understood; unknown data remains
   raw with warnings. Compatible video/audio streams receive a best-effort
   codec-copy MP4/MKV/M4A/MKA. Packed modes remain packed. The generic path has
   synthetic packet-equality and V2/V3 coverage, not a six-camera release
   corpus.

8. Automatic readout correction requires an established source-sensor profile,
   duration, exposure timing, and complete gyro coverage. `Required` makes
   unavailable correction an error; `Auto` reports an omission. This does not
   establish arbitrary rotated/cropped recording support. See the exact
   [profile constraints](docs/stabilization.md).

Additional registered optical profiles include X2 adhesive spherical and clip-on
guards; X3 A/S/AS protectors; X4 A/S/AS protectors; X5 A protector; and X5
bare-underwater lens type 114 when already encoded. ND16/32/64/128 states are
parsed, but portable X5 conversion to those filters is not implemented.

Input codec support for decoded stitching depends on recording mode and the
linked FFmpeg build. Packet-preserving extraction does not decode or re-encode
streams and is not restricted to X5; playable convenience remuxes remain
codec/muxer-dependent. The only real stitched-export corpus exercised so far is
X5 dual-track 8-bit HEVC. Probe reports indexed gyro/exposure descriptors and
counts; use `InsvReader::read_record_payload` with `decode_motion_record` or
`decode_exposure_record` to obtain samples.

## Installation

### Rust library from crates.io

Once published, add the parser and low-level CPU primitives with:

```sh
cargo add insta360-rs
```

For packet-preserving extraction and CPU file workflows:

```sh
cargo add insta360-rs --features media
```

Add portable GPU stitching with:

```sh
cargo add insta360-rs --features media,gpu
```

The package name uses a hyphen; Rust code imports it as `insta360_rs`. The
minimum supported Rust version is 1.88.

### Command-line application

CPU-capable CLI:

```sh
cargo install insta360-rs --features cli --locked
```

CLI with the native `wgpu` backend for the current platform:

```sh
cargo install insta360-rs --features cli,gpu --locked
```

The `media` and `cli` features require FFmpeg headers and linkable `avcodec`,
`avformat`, `avutil`, and `swscale` libraries at build time. If FFmpeg is
dynamically linked, its shared libraries must also be discoverable at runtime.
HEVC video export requires at least one usable HEVC encoder in that FFmpeg
build, either software such as libx265 or a supported platform encoder.
Extraction requires demuxer/muxer support, but no decoder, encoder, GPU, camera
profile, or calibration.

After installation, inspect the actual host rather than assuming acceleration is
available:

```sh
insta360-rs capabilities
insta360-rs capabilities --json
```

### From source

From the standalone `insta360-rs` checkout, first make FFmpeg development and
link libraries discoverable by `ffmpeg-next`, then install the CLI:

```sh
cargo install --path . --features cli,gpu --locked
```

For an in-tree build and capability smoke test:

```sh
cargo run --features cli,gpu -- capabilities
```

Run the test suite with all optional paths enabled:

```sh
cargo test --all-features
```

## Cargo features

| Feature | Default | Purpose                                                                                                                                   | Extra runtime requirement                                        |
| ------- | :-----: | ----------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------- |
| none    |   ✅    | Bounded INSV parser, metadata, profiles, calibration, telemetry, motion, and deterministic CPU stitch primitives                          | None                                                             |
| `media` |   ❌    | Direct stream readers, packet/metadata extraction and codec-copy remux, decoded frame export, HEVC MP4 export, progress, and cancellation | Linkable FFmpeg; deploy shared libraries when dynamically linked |
| `gpu`   |   ❌    | Safe `wgpu` compute stitcher and adapter discovery                                                                                        | Compatible native GPU adapter and driver                         |
| `cli`   |   ❌    | Builds the `insta360-rs` executable; implies `media`                                                                                      | FFmpeg                                                           |

`gpu` does not imply `media`: applications can use the low-level GPU stitcher
with their own decoded frames. Enable both for GPU file conversion.

## Direct stream access and extraction

```sh
insta360-rs extract recording.insv extracted
```

Use `extract(&InputSet, output_dir)` from Rust or
`insta360_rs.extract(input, output_dir)` from Python. To read packets or seek
and decode unstitched frames directly from the original file, use `MediaSource`
and `MediaStream`; no intermediate MP4 is created. See
[stream access and extraction](docs/extraction.md) for API examples, the output
layout, preservation guarantees, and storage requirements.

## Rust quick start

### Probe without decoding video

The default feature set is sufficient:

```rust
use insta360_rs::{probe, InputSet};

fn main() -> insta360_rs::Result<()> {
    let inputs = InputSet::discover("recording.insv")?;
    let info = probe(&inputs)?;

    println!("camera: {:?}", info.camera);
    println!("video tracks: {}", info.video_tracks.len());
    println!("offset versions: {:?}", info.offset_versions);
    println!("optical profiles: {:?}", info.optical_profiles);
    Ok(())
}
```

`InputSet::discover` finds the matching `_00_` or `_10_` file for legacy paired
recordings when it exists. The CLI `extract` command performs this discovery for
one input; `probe` requires both paths explicitly.

### Extract encoded streams and metadata

This requires `features = ["media"]` and works independently of decoded
stitching support:

```rust
use insta360_rs::{extract, InputSet};

fn main() -> insta360_rs::Result<()> {
    let inputs = InputSet::discover("recording.insv")?;
    let report = extract(&inputs, "recording-extracted")?;

    println!(
        "{} streams, {} ExtraInfo records",
        report.stream_count, report.record_count
    );
    println!("manifest: {}", report.manifest_path.display());
    for warning in report.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}
```

The destination must be absent or an empty, non-symlink directory. Work is
staged beside it and published only after every input succeeds. The report
contains absolute output, manifest, and artifact paths plus counts and warnings.

### Export an X5 video

This example requires `features = ["media", "gpu"]` and explicitly disables
stabilization. Use `Stabilization::DirectionLock` for gravity-referenced output
with a fixed initial heading; exposure-file PTS mapping is supported.

```rust
use std::time::Duration;

use insta360_rs::{
    AudioPolicy, ColorConversion, EquirectangularProjection, Exporter,
    InputSet, MediaAcceleration, OpticalSetup, ProcessingBackend,
    Stabilization, StitchConfig, VideoExportOptions,
};

fn main() -> insta360_rs::Result<()> {
    let inputs = InputSet::discover("recording.insv")?;
    let config = StitchConfig {
        optical_setup: OpticalSetup::StrictAuto,
        stabilization: Stabilization::Off,
        backend: ProcessingBackend::Auto,
        color_conversion: ColorConversion::Auto,
        ..StitchConfig::default()
    };
    let exporter = Exporter::new(inputs, config)?;

    let result = exporter
        .export_video(
            "stitched.mp4",
            VideoExportOptions {
                quality: 90,
                audio: AudioPolicy::Drop,
                acceleration: MediaAcceleration::Auto,
                projection: Some(EquirectangularProjection {
                    width: 5760,
                    height: 2880,
                }),
                start: Some(Duration::from_secs(120)),
                duration: Some(Duration::from_secs(60)),
            },
        )
        .wait()?;

    println!("wrote {:?} with {:?}", result.outputs, result.backend.selected);
    Ok(())
}
```

`ExportJob` also exposes bounded progress-event polling and cancellation. A
successful `wait()` is the publication contract.

## CLI usage

```text
insta360-rs extract <INPUT> [<SECOND_INPUT>] <OUTPUT_DIR> [--json]
insta360-rs probe <INPUT>... [--json]
insta360-rs export-frames <INPUT> <OUTPUT_DIR> \
  (--indices <N,...> | --timestamps <SECONDS,...>) [OPTIONS]
insta360-rs export-video <INPUT> <OUTPUT.mp4> [OPTIONS]
insta360-rs capabilities [--json]
```

Run `insta360-rs <COMMAND> --help` for the generated reference.

### Extract encoded streams and metadata

With one input, the matching legacy sibling is discovered automatically:

```sh
insta360-rs extract recording.insv recording-extracted
```

Or supply a split pair explicitly and return the completed report as JSON:

```sh
insta360-rs extract \
  VID_20240101_120000_00_001.insv \
  VID_20240101_120000_10_001.insv \
  recording-extracted \
  --json
```

`extract` accepts exactly one or two inputs and no stitch, color, quality, or
GPU options. With two inputs it validates and orders `_00_` before `_10_`. The
destination must be absent or an empty, non-symlink directory; sibling staging
is atomically published only after every input succeeds.

Each source is written below `input-00`, `input-01`, and so on. Every stream
directory contains `packets.bin`, `packets.jsonl`, `extradata.bin`,
`side_data.bin`, and `metadata.json`; a compatible codec-copy operation also
adds `media.mp4`, `.mkv`, `.m4a`, or `.mka`. Container artifacts retain
non-`mdat` boxes, `mdat` headers, the raw ExtraInfo tail and records, decoded
known metadata JSON, and calibration/profile payloads. The root `manifest.json`
describes every artifact and preservation limit. This is component extraction,
not a byte-for-byte backup of unused `mdat` space, and raw plus playable copies
can require roughly twice the encoded media size.

Without `--json`, stdout reports input/stream/record counts and output paths;
warnings use stderr. With `--json`, stdout is an `ExtractionReport` containing
`output_dir`, `manifest_path`, `input_count`, `stream_count`, `record_count`,
`files`, and `warnings`.

### Inspect an INSV

```sh
insta360-rs probe recording.insv
insta360-rs probe recording.insv --json
```

For a legacy split recording, pass the pair in primary/secondary order:

```sh
insta360-rs probe VID_20240101_120000_00_001.insv \
  VID_20240101_120000_10_001.insv --json
```

Probe reads the ISO-BMFF headers, movie metadata, trailer index, and bounded
metadata record. It does not decode every frame or scan the entire media
payload.

### Export stitched frames

By timestamps:

```sh
insta360-rs export-frames recording.insv frames \
  --timestamps 1.0,2.5,4.0 \
  --width 5760 \
  --optical-setup strict-auto \
  --stabilization off \
  --backend auto
```

By zero-based decoded frame indices:

```sh
insta360-rs export-frames recording.insv frames \
  --indices 0,30,60 \
  --stabilization off \
  --backend cpu
```

Exactly one of `--indices` or `--timestamps` is required. CLI frame export
writes `frame_<selection>.png`. The Rust and Python APIs additionally expose
JPEG output. `--width` must be a non-zero even panorama width; height is always
`width / 2`. Without it, the default panorama is twice the fisheye track width.

### Convert an INSV to stitched MP4

```sh
insta360-rs export-video recording.insv stitched.mp4 \
  --start 120 \
  --duration 60 \
  --width 5760 \
  --quality 90 \
  --audio drop \
  --optical-setup strict-auto \
  --stabilization off \
  --color-conversion auto \
  --backend auto \
  --media-acceleration auto
```

The current command accepts one X5 INSV containing exactly two synchronized
video tracks. It writes an HEVC MP4 with 8-bit YUV420 video. Existing output
files are never overwritten.

`--start` and `--duration` are source-relative seconds. The interval is
half-open, `[start, start + duration)`, and the first output frame is rebased to
timestamp zero. Omit `--duration` to continue to the end.

During export, `<output>.insta360-rs-part` is deliberately incomplete and will
usually not open in VLC even if renamed to `.mp4`: FFmpeg has not written the
MP4 trailer. On success the encoder is flushed, the trailer is written, and the
temporary file is atomically renamed. Failure or cancellation removes it.

### CLI options

Shared stitch options:

| Option               | Values                                                                                                                                                                                                                                                                                                   | Default          | Meaning                                                                                                                                                                 |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--optical-setup`    | `strict-auto`, `bare-air`, `bare-underwater`, `waterproof-case`, `dive-case-air`, `dive-case-underwater`, `invisible-dive-case-air`, `invisible-dive-case-underwater`, `clip-on-lens-guard`, `adhesive-sphere-lens-guard`, `protector-a`, `protector-s`, `protector-as`, `nd16`, `nd32`, `nd64`, `nd128` | `strict-auto`    | Requests an exact setup. It succeeds only when the recorded lens type matches or an evidence-backed conversion exists; `strict-auto` uses conclusive recorded metadata. |
| `--stabilization`    | `off`, `flow-state`, `direction-lock`                                                                                                                                                                                                                                                                    | `direction-lock` | Gravity-referenced correction with validated exposure/video timing.                                                                                                     |
| `--rolling-shutter`  | `auto`, `off`, `required`                                                                                                                                                                                                                                                                                | `auto`           | Source-sensor motion correction; requires an enabled stabilization mode.                                                                                                |
| `--backend`          | `auto`, `cpu`, `gpu`                                                                                                                                                                                                                                                                                     | `auto`           | Stitch renderer. `auto` attempts GPU and may restart the whole job on CPU. Explicit choices are strict.                                                                 |
| `--color-conversion` | `auto`, `preserve`, `i-log-to-rec709`                                                                                                                                                                                                                                                                    | `auto`           | Converts positively identified X5 I-Log with the bundled Rec.709 LUT, leaves stitched values LUT-untransformed, or explicitly requests X5 I-Log conversion.             |

Frame-selection options:

| Option         | Values                               | Default | Meaning                                                                                                   |
| -------------- | ------------------------------------ | ------- | --------------------------------------------------------------------------------------------------------- |
| `--indices`    | comma-separated integers             | none    | Zero-based synchronized decoded-frame indices. Conflicts with `--timestamps`.                             |
| `--timestamps` | comma-separated non-negative seconds | none    | Selects the first synchronized frame at or after each source-relative target. Conflicts with `--indices`. |

Output-size option for frames and video:

| Option    | Values                | Default        | Meaning                                |
| --------- | --------------------- | -------------- | -------------------------------------- |
| `--width` | non-zero even integer | source-derived | Equirectangular width; height is half. |

Video-only options:

| Option                 | Values                         | Default | Meaning                                                        |
| ---------------------- | ------------------------------ | ------- | -------------------------------------------------------------- |
| `--quality`            | `1..=100`                      | `90`    | HEVC quality target.                                           |
| `--start`              | non-negative seconds           | `0`     | Source-relative start.                                         |
| `--duration`           | positive seconds               | to end  | Requested interval length; zero is rejected.                   |
| `--audio`              | `drop`, `copy`                 | `drop`  | Only `drop` is implemented; `copy` returns a capability error. |
| `--media-acceleration` | `auto`, `software`, `hardware` | `auto`  | HEVC encoder selection, independent of the stitch backend.     |

`--media-acceleration hardware` requires an eligible hardware HEVC encoder;
`software` requires a software encoder. `auto` tries eligible encoders in
priority order. Decode is currently software in every mode, and an encoder
failure after frames have already been submitted does not restart the job.

### Direction Lock

Direction Lock preflight validates the gyro data and timestamp mapping:

```sh
insta360-rs export-video recording.insv locked.mp4 \
  --audio drop \
  --stabilization direction-lock
```

Both modes use the full recording's IMU pre-roll and actual presentation
samples, so selected timestamps and video trims retain the same heading anchor.
`--rolling-shutter auto` is the default; use `required` to demand sensor readout
correction or `off` to apply only global stabilization. `--stabilization off`
bypasses motion entirely and conflicts with `--rolling-shutter required`.
Missing timing, unknown sensor profiles, unreliable initial gravity, saturation,
and telemetry gaps fail preflight. Auto reports unavailable readout correction.

See [stabilization conventions and limits](docs/stabilization.md) for metadata
requirements and the distinction between horizon leveling and absolute heading.

## GPU processing

The GPU path is compute-only and uses safe `wgpu`; no vendor runtime or graphics
API type crosses the public API boundary.

```text
INSV → FFmpeg software decode
     → direct 8-bit YUV420 upload, or CPU swscale to RGB
     → wgpu projection + distortion + masks + radiometry + fixed seam blend
     → optional bundled X5 I-Log 3D LUT
     → GPU RGB still, or BT.709 limited-range YUV420 video
     → synchronous CPU-visible readback
     → Rust PNG/JPEG encoder, or FFmpeg HEVC encoder
```

The adapter, device, pipelines, bind groups, and dimension-dependent buffers are
retained and reused for a job. Projection, bilinear sampling, optical validity
masks, overlap statistics, color gains, fixed high-frequency seam, two-band
blend, optional 3D LUT, and video RGB-to-YUV420 conversion run on the GPU.

Current transfer boundaries are important: decoding remains on the CPU, each
frame is synchronously read back, and FFmpeg receives CPU-visible output. There
are no hardware decode surfaces, native decoder-to-wgpu sharing, zero-copy
encoder surfaces, or asynchronous frame slots yet.

`--backend auto` attempts one complete GPU export. Only a typed GPU
initialization or processing failure triggers cleanup and a complete restart on
CPU; it never mixes CPU and GPU frames in one result. `--backend gpu` and
`--backend cpu` never fall back.

### GPU platform support

This crate uses wgpu's native backend names but deliberately enables only one
backend per supported desktop OS. Upstream wgpu may support additional targets
or APIs that are not compiled here.

| Target  | wgpu backend compiled by `insta360-rs` | GPU stitching | Real X5 media exercised | CPU fallback |
| ------- | -------------------------------------- | :-----------: | :---------------------: | :----------: |
| macOS   | Metal                                  |      ✅       | ✅ One Apple/Metal host |      ✅      |
| Windows | Direct3D 12                            |      ✅       |       ❌ Pending        |      ✅      |
| Linux   | Vulkan                                 |      ✅       |       ❌ Pending        |      ✅      |

OpenGL/GLES is not enabled. A compiled backend still requires a compatible
adapter and driver; check `insta360-rs capabilities` on the target machine.

GPU stitching and hardware encoding are independent. Depending on the FFmpeg
build and host, encoder discovery may find VideoToolbox, Media Foundation,
NVENC, AMF, VAAPI, libx265, or libkvazaar. `capabilities` reports the exact
encoders visible at runtime.

### Current performance reference

Single-run X5 measurements on one Apple Metal host are included only as an
implementation reference, not a cross-platform promise:

| Output    | CPU + libx265 |    wgpu + libx265 | wgpu + VideoToolbox |
| --------- | ------------: | ----------------: | ------------------: |
| 1920×960  |     10.38 fps | 13.79 fps (1.33×) |   40.33 fps (3.89×) |
| 5760×2880 |      1.28 fps |  2.54 fps (1.99×) |  27.58 fps (21.57×) |

These are matched 15-second runs, but not three-run medians. See
[performance details](docs/performance.md) for quality metrics, bitrate, and
measurement limitations.

## Calibration and underwater capture

Factory calibration belongs to the recording. Resolution follows this order:

1. explicit caller optical setup;
2. conclusive recorded accessory/offset state and automatic guard result;
3. lens type already encoded in the current offset;
4. an explicit ambiguity or unsupported-conversion error.

Registry values supply camera-family FOV, blend angle, lens identity, and mask
recipes that are not per-device measurements. They never replace the recording's
intrinsics, distortion coefficients, principal points, or extrinsics. A valid
recorded blend angle takes precedence unless the optical setup was converted, in
which case the target profile's fallback remains in control.

For the supplied X5 Dive Case Pro underwater recording, `strict-auto` reads
offset state 10 and converts the type-113 V6 factory calibration to the type-117
`InvisibleDiveWater` profile. Do not select an underwater profile only because a
scene visually contains water: the setting describes the camera, housing, and
medium that created the refractive geometry.

For photogrammetry, keep output dimensions, optical setup, stabilization, seam,
and color pipeline identical across the dataset. The default fixed seam avoids
dynamic optical-flow changes in high-frequency feature ownership.

See [calibration](docs/calibration.md) and [settings](docs/settings.md) for the
offset layouts, profile conversion, masks, and recommended capture policy.

## Bundled licensed Insta360 and Studio assets

The library ships 41 original Insta360 and Studio data resources through the
`insta360-rs-data-core` and `insta360-rs-data-enhancement` dependencies (about
20.8 MiB uncompressed). The files live under `data/*/assets/` in this
repository; each published crate stays below 10 MB. They are compiled into the
library with `include_bytes!` and available directly at runtime.

The bundle currently contains:

- camera configuration JSON for ONE X (One2), ONE X2, OneR/OneRS, X3, X4, X4
  Air, X5, and X6;
- X5, Ace Pro 2, and Luna I-Log-to-Rec.709 LUTs;
- the ISO/FOV sharpening parameter file;
- seven camera-accessory SVM files and five cooling-shell SVM files; and
- AI-seam, ColorPlus, deflicker, defringe, and JPEG-denoise model payloads.

`BundledAssetProvider::manifest()` returns the validated manifest.
`BundledAssetProvider` then serves only manifest-declared paths and verifies
each requested payload's byte length and SHA-256 digest before returning it:

```rust
use insta360_rs::assets::{AssetPolicy, BundledAssetProvider, OpenCvLinearSvm};

fn main() -> insta360_rs::assets::AssetResult<()> {
    let bundle = BundledAssetProvider::manifest()?;
    let asset = bundle
        .load_verified(
            &BundledAssetProvider,
            "camera-accessory-svm-0db3a7a0-xml",
            AssetPolicy::Required,
        )?
        .expect("required bundled asset");
    let svm = OpenCvLinearSvm::parse_xml(&asset.bytes)?;

    println!(
        "{} resources; {} support vectors",
        bundle.assets.len(),
        svm.support_vectors.len()
    );
    Ok(())
}
```

Applications may alternatively use `DirectoryAssetProvider` or
`InMemoryAssetProvider` for an application-controlled bundle. Paths are confined
below the provider root, and compatibility checks can restrict an asset to a
camera, lens ID, and Rust target.

Bundling is not algorithm qualification. Every copied model is **unqualified by
default**. The crate can parse OpenCV linear-SVM payloads and validate complete
CoreML/Espresso groups, but it does not yet implement the camera-specific SVM
feature extractor, AI seam inference, ColorPlus, deflicker, defringe, or denoise
execution. The deterministic stitcher therefore does not silently invoke those
resources. An application must qualify the complete preprocessing, inference,
and output behavior before selecting a model at runtime.

The project Apache-2.0 license covers project-authored code. The original
Insta360 resources retain their vendor licensing, and downstream distributors
remain responsible for ensuring that their use and redistribution are covered.
See [licensed assets](docs/licensed-assets.md), [packaging](docs/packaging.md),
the [literal copy inventory](docs/sdk-provenance.md), and
[NOTICE.md](NOTICE.md).

## Python

The PyO3 package targets Python 3.10+ and exposes `probe`, blocking
packet-preserving `extract`, `capabilities`, frame/video export, job polling and
cancellation, camera metadata, optical-setup and color-conversion enums, backend
selection, and media-acceleration policy. Wheels are not yet release-qualified
or published.

When wheels become available, installation will use:

```sh
pip install insta360-rs
```

The distribution name is `insta360-rs`; import it as `insta360_rs`:

```python
from insta360_rs import extract

report = extract("recording.insv", "recording-extracted")
print(report.manifest_path, report.stream_count, report.warnings)
```

Python extraction releases the GIL and uses the same sibling discovery and
destination rules as the CLI. Stitched-video calls must currently select
`AudioPolicy.DROP`; extraction still preserves audio packets and attempts a
standalone codec-copy remux. The asset-provider layer is not yet exposed as a
Python conversion argument.

See [Python bindings](docs/python.md) for examples and wheel targets.

## Current limitations

- High-level media export is restricted to X5 single-file, two-track input.
- Packed ONE X-X3 video is preserved as one encoded stream; extraction does not
  synthesize separate decoded lens tracks from that packed frame.
- V1 calibration is parse-only and fails stitch preflight.
- Stabilization currently resolves X5 compact raw IMU profiles only. Edited or
  unsupported recording clocks and unknown sensor transforms fail explicitly.
- General crop-aware optical projection is not implemented. Sensor readout uses
  only established crop/rotation conventions; see the stabilization profile.
- Recorded factory gyro calibration values are retained; their undocumented bias
  ordering is not guessed. Six-axis fusion cannot remove absolute yaw drift.
- Input decoding is software-only; GPU output still requires synchronous
  readback.
- Stitched video output is 8-bit YUV420 HEVC; extraction preserves encoded
  10-bit packets without converting them.
- Audio copy/remux into stitched MP4 is not implemented; extraction preserves
  audio packets and attempts a standalone M4A/MKA codec-copy remux.
- AI seam, ColorPlus, defringe, deflicker, denoise, and accessory-image
  classification are not runtime capabilities.
- X1-X4 and X6 need decoded stitching/export support and real-camera golden
  fixtures; their encoded streams and metadata can already be extracted.
- Windows D3D12 and Linux Vulkan paths compile but still need real-X5 release
  qualification.

## Documentation

- [INSV format](docs/INSV_FORMAT.md)
- [Architecture](docs/architecture.md)
- [Public API](docs/public-api.md)
- [Stream access and extraction](docs/extraction.md)
- [Calibration](docs/calibration.md)
- [Settings](docs/settings.md)
- [GPU and performance](docs/performance.md)
- [Testing](docs/testing.md)
- [Python bindings](docs/python.md)
- [Packaging](docs/packaging.md)
- [Bundled asset usage](docs/asset-usage.md)
- [Licensed asset architecture](docs/licensed-assets.md)
- [Literal asset copy inventory](docs/sdk-provenance.md)

## Development

This independent Cargo workspace contains the library, two data crates, and
Python bindings. The root `[workspace.package]` table shares version, author,
repository, edition, and minimum Rust version; all members use one `Cargo.lock`.

From the repository root:

```sh
cargo fmt --all -- --check
cargo test --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Media tests require the FFmpeg build environment described under
[Installation](#from-source). Some real-media tests run only when their fixture
environment variable is configured. The Python binding is built and tested from
`src-python`. Default workspace commands select the library and data crates; run
`cargo test --locked -p insta360-rs-python` to test the bindings separately.

No automated or production path invokes an Insta360 executable or library.

## License

Project-authored code is licensed solely under the
[Apache License, Version 2.0](LICENSE.md).

See [NOTICE.md](NOTICE.md) for trademark and resource-provenance information.

## CI and releases

See [CI setup and checks](docs/CI.md) and
[releasing to crates.io and PyPI](docs/RELEASE.md).
