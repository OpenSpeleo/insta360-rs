# Conversion settings

## User-facing controls

Keep the normal path small:

- **Optical selection**: `housing`, `environment`, `lens_accessory` and
  `mounting_accessory` default to `Auto`. Explicit components override metadata.
  Housing values distinguish standard Invisible Dive Case from Dive Case Pro.
  Ambiguous or conflicting selections fail explicitly. See
  [housings](housings.md).
- **Stabilization**: `DirectionLock` holds the initial heading with a level
  horizon; `FlowState` preserves heading while leveling; `Off` bypasses motion.
  Both enabled modes use six-axis fusion, with no absolute compass reference.
- **Rolling shutter**: `Auto` (default), `Off`, or `Required`. Auto corrects
  supported sensor readout and reports any omission. Off retains global
  stabilization only. Required fails when the metadata/coverage is insufficient
  and conflicts with stabilization Off. CLI: `--rolling-shutter`; Rust/Python:
  `StitchConfig.rolling_shutter`. See [stabilization](stabilization.md).
- **Stitch backend**: `Auto`, `Cpu`, or `Gpu`. `Auto` tries portable `wgpu`
  first and, only for a typed GPU initialization/processing failure, removes
  attempt-owned partial outputs and reruns the complete job on CPU. It never
  mixes renderers in one successful output. Explicit `Cpu` and `Gpu` are strict.
- **Media acceleration**: video-only `Auto`, `Software`, or `Hardware`,
  independent of the stitch backend. It currently selects the HEVC encoder;
  export decoding is software FFmpeg. `Hardware` is strict. `Auto` prefers
  software encoding for CPU stitching and hardware encoding for GPU stitching,
  then tries all eligible encoder candidates in priority order when an encoder
  cannot be configured/opened. Failure after encoding begins does not yet
  restart the complete export.
- **Color conversion**: `Auto`, `Preserve`, or `ILogToRec709`. Auto applies the
  bundled X5 Rec.709 LUT only to positively identified I-Log recordings.
  Preserve keeps the recorded encoding; the explicit mode handles older unmarked
  I-Log files. Both CPU and GPU exports apply the transform. See
  [asset usage](asset-usage.md).
- **Underwater color**: `Off` (default), `Legacy`, or optional `Ai`.
  `UnderwaterColorOptions` validates strength in 0–1, legacy balance in 0–1, and
  AI style 0–3. Legacy defaults to strength 0.8/balance 0.5; AI defaults to
  strength 1/style 0. Controls irrelevant to a mode are rejected.
- **Output size**: an even, 2:1 equirectangular width/height.
- **Licensed enhancement policy**: advanced, per feature: `Disabled`,
  `Automatic`, or `Required`. `Automatic` may use a present, compatible,
  integrity-checked asset and otherwise retains the deterministic fallback;
  corruption is always an error. For photogrammetry, keep AI seam, ColorPlus,
  defringe, deflicker, and denoise `Disabled` until separately qualified.
- **Video interval**: optional source-relative start and duration. The start
  defaults to zero; duration defaults to the rest of the recording.
- **Still format**: PNG, or JPEG with quality 1–100.
- **Video quality/audio**: quality 1–100. `libx265` maps this to CRF; bitrate-
  driven encoders use a nonlinear quality curve that preserves the high end for
  photogrammetry and scales sublinearly with resolution (quality 85 targets
  about 36 Mb/s at 1920×960 and 140 Mb/s at 5760×2880, 29.97 fps). `Drop` omits
  audio; `Copy` preserves compatible AAC/ALAC packets and A/V timing without
  re-encoding. Cuts keep complete packets; a silent source stays silent. See
  [sequence stitching](sequence-stitching.md) for boundary rules.

Seam mode remains fixed in 0.1. The X5 fixed mode automatically uses calibrated
optical validity, a sharp high-frequency seam, and adaptive low-frequency color
compensation. Dynamic/AI stitching and automatic accessory classification have
asset-loading policies but are not reported as available settings until their
preprocessing, inference, and temporal geometry are qualified for
photogrammetry.

## Underwater photogrammetry preset

Recommended defaults are:

```text
optical setup   explicit physical case + medium
stabilization   direction lock
seam            fixed
backend         auto
media codec     auto
video quality   85 or higher
projection      5760 × 2880 when source detail supports it
still output    PNG for highest radiometric consistency, JPEG 95 when storage matters
audio           drop for still-oriented processing; copy to retain original audio
AI/restoration  disabled
```

The recommended `backend auto` attempts GPU and transparently reruns the whole
job on CPU for a typed GPU failure. Use explicit `cpu` for a reproducible CPU
reference and explicit `gpu` when failure must be surfaced rather than retried.
Retain CPU comparison output until the target adapter/backend has passed real-X5
qualification. `media codec auto` is a separate encoder policy.

Equivalent controls are:

```text
Rust    StitchConfig.backend = ProcessingBackend::{Auto,Cpu,Gpu}
        VideoExportOptions.acceleration = MediaAcceleration::{Auto,Software,Hardware}
CLI     --backend auto|cpu|gpu
        --media-acceleration auto|software|hardware
Python  StitchConfig(backend=ProcessingBackend.AUTO|CPU|GPU)
        export_video(..., acceleration=MediaAcceleration.AUTO|SOFTWARE|HARDWARE)
```

The GPU export path supports safe YUV420P/RGB upload and CPU-visible readback.
Export decoding remains software. Native random-access previews separately
expose `PreviewAcceleration::Auto` and `Software`; `Auto` selects supported
hardware decoding with software fallback. Chroma formats beyond planar 8-bit
YUV420, asynchronous export frame slots, and native zero-copy surfaces remain
implementation and qualification work.

For the supplied dive-case recording, the recommended setting is
`--housing auto`. The recording explicitly carries X5 Dive Case Pro underwater
state 10, so the resolver converts its type-113 V6 factory offset to the
type-119 Pro underwater profile. Use an explicit optical setup only to override
missing or incorrect camera metadata.

Do not select an underwater profile simply because the footage visually looks
underwater. The profile models refraction introduced by a specific housing and
medium. A wrong choice changes ray geometry and can degrade camera alignment
even when the panorama looks plausible.

For dataset capture, keep output size, optical setup, seam mode, color pipeline,
and stabilization mode identical for every frame. Record the selected values in
the photogrammetry job manifest.
