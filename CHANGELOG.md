# Changelog

The most important insta360-rs product updates are listed here. This is a
curated, high-level release history; routine maintenance and implementation
details are intentionally omitted.

## Unreleased

### Features

- Export compatible split recordings as continuous 360° videos with original
  audio, or extract synchronized fisheye frames and complete source archives.
  84430e5

### Performance

- Faster synchronized fisheye previews when moving through a recording. 84430e5

### Fixes

- Improve recording discovery, exact frame selection, and stabilization across
  split-video boundaries. a5aec20
- Preserve original audio timing in stitched video exports, including trimmed
  clips. bc10f30

## v0.1.0 - 2026-09-09

### Features

- Work with Insta360 recordings through Rust, Python 3.10+, and command-line
  workflows on macOS, Windows, and Linux. c43d98a
- Inspect ONE X, ONE X2, X3, X4, X5, and X6 recording metadata, including camera
  details, video tracks, capture settings, accessories, and factory calibration.
  c43d98a
- Open single-file and paired recordings, with automatic companion-file
  discovery for extraction and supported APIs. c43d98a
- Extract original encoded video, audio, and other streams with their timing and
  metadata, preserving packet payloads without transcoding. Compatible streams
  can also be saved as standalone playable files. c43d98a
- Export container metadata, proprietary recording records, and understood
  calibration data for further analysis while retaining unknown records. c43d98a
- Read encoded packets and seek decoded source frames directly from recordings
  without creating intermediate media files. c43d98a
- Access recorded gyro, acceleration, and exposure samples for motion analysis
  and synchronization. c43d98a
- Inspect V1, V2, V3, and V6 factory calibration and use V2, V3, and V6
  projection geometry through the Rust stitching API. c43d98a
- Stitch X5 single-file, dual-track recordings into 2:1 equirectangular
  panoramas using per-recording calibration, fixed seams, and overlap color
  matching for consistent photogrammetry output. c43d98a
- Resolve recorded optical setups and apply X5 Dive Case Pro air and underwater
  calibration when the required profile data is available. c43d98a
- Stabilize supported X5 recordings with Direction Lock or horizon leveling that
  preserves camera heading. Apply rolling-shutter correction when the required
  sensor profile and motion data are available. c43d98a
- Convert X5 I-Log footage to Rec.709 or preserve stitched I-Log values for
  downstream grading. c43d98a
- Export selected X5 panorama frames as PNG or JPEG through the APIs, or PNG
  through the CLI, with frame-index, timestamp, and API sampling-range selection
  and configurable output resolution. c43d98a
- Export complete X5 recordings or selected time intervals as 8-bit HEVC MP4
  video, with configurable resolution and quality. c43d98a
- Monitor export progress, poll and cancel background jobs, and publish
  completed outputs without overwriting existing files. c43d98a
- Inspect available GPU and HEVC encoding capabilities and select processing and
  encoding backends independently. c43d98a
- Access bundled camera profiles and color LUTs, or supply application-managed
  resources through the Rust API. c43d98a

### Performance

- Inspect large INSV recordings without scanning the complete video payload.
  c43d98a
- Accelerate calibrated stitching on native Metal, Direct3D 12, and Vulkan GPUs,
  with automatic CPU fallback. Real-recording GPU validation currently covers
  macOS Metal. c43d98a
- Use available hardware HEVC encoders or software encoding through FFmpeg.
  c43d98a
