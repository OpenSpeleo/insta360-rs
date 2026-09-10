# Runtime use of bundled assets

The high-level renderer accepts validated panorama layouts and registered
calibration profiles. One implemented processing asset is Studio's X5
I-Log-to-Rec.709 CUBE. It now changes image and video exports on CPU and GPU.
The other two CUBEs also have a working RGB/GPU consumer for callers processing
those camera images; they are not selected for X5 recordings.

All 51 stored payloads remain available through the same provider and asset IDs.
The five [data crates](packaging.md) preserve original vendor bytes; model
availability still does not imply a working inference pipeline.

## Implemented color conversion

`StitchConfig::color_conversion` controls the transform:

- `Auto` (default) selects the X5 LUT when recording metadata explicitly
  identifies I-Log. Standard, unknown, and unmarked recordings are preserved.
- `Preserve` leaves the recorded color encoding for downstream grading.
- `ILogToRec709` explicitly selects the X5 LUT for older I-Log recordings
  lacking the marker. Explicit standard/Dolby metadata is an error.

Two source-backed markers identify I-Log:

- ExtraMetadata `gamma_mode`, tag 22, equals the exact string `I_Log`. Studio
  5.9.10's `InstaHelper::FootageSupportLut` tests that spelling. The older
  string `log` does not identify the same curve and does not enable automatic
  conversion.
- ExtraMetadata `shooting_param_info`, tag 212, contains `color_mode`, nested
  tag 8, with value 2 (`COLOR_MODE_ILOG`). The iOS SDK 1.10.4 protobuf
  descriptor declares unknown=0, standard=1, ILog=2, and Dolby=3. This numbering
  differs from the separate editing-recipe color mode.

Malformed/conflicting capture color declarations are distinguished from missing
metadata and reject conversion. They cannot activate the legacy gamma fallback.
`Preserve` explicitly opts out. Unknown future enum values do not activate Auto.
The original gamma and capture submessage bytes remain available in the
container's retained fields.

The selected table is integrity-checked and parsed once per export attempt.
`CubeLut` implements bounded 3D CUBE parsing, per-channel domains, red-fastest
sample ordering, and trilinear interpolation. CPU applies it in parallel to
stitched RGB8 pixels. GPU uploads its samples once per frame-resource set and
applies the same table inside the stitch shader, before RGB readback or the
existing RGB-to-YUV420 pass. No GPU frame detour through a CPU color filter is
required. GPU fallback recreates the same selected transform in the CPU session.

Decoded YUV range/matrix are honored when producing RGB inputs. Converted video
uses limited-range BT.709 YUV and declares BT.709 primaries/transfer/matrix in
the HEVC stream. Unconverted video is not assigned a Rec.709 transfer curve or
gamut merely because the GPU uses a BT.709 RGB-to-YUV matrix. Conversion changes
color only; it does not alter calibration, projection, lens masks,
stabilization, or seam ownership. The existing RGB8 processing boundary remains;
this does not add a 10-bit preservation pipeline.

```sh
insta360-rs export-frames input.insv frames --indices 0 --color-conversion auto
insta360-rs export-video input.insv output.mp4 --audio drop \
  --color-conversion preserve
```

```python
from insta360_rs import ColorConversion, StitchConfig

config = StitchConfig(color_conversion=ColorConversion.AUTO)
# For an older I-Log file without a recognized marker:
config.color_conversion = ColorConversion.I_LOG_TO_REC709
```

Low-level callers can load any of the three CUBEs explicitly with
`color::CubeLut::load_bundled`, call `apply_rgb8`/`sample`, or install it with
`GpuStitcher::set_color_lut`. Camera/profile selection is the low-level caller's
responsibility. Generic asset manifests and providers remain available for
inspection and model development; their model qualification flags do not
advertise additional inference capabilities.

## Assessment of bundled payloads

| Payloads                                                             | Count | Runtime decision                                                                                                                                                                        |
| -------------------------------------------------------------------- | ----: | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| X5 I-Log CUBE                                                        |     1 | Selected by the high-level image/video exporter; CPU and GPU execution implemented.                                                                                                     |
| Ace Pro 2 and Luna I-Log CUBEs                                       |     2 | Supported by the explicit RGB/GPU LUT APIs; those cameras are outside the current high-level panorama renderer.                                                                         |
| CameraSDK camera configurations                                      |    10 | Capture modes and setting dependencies, not optical calibration. Do not restrict panorama output dimensions or replace recorded offsets with these values.                              |
| Accessory/cooling-shell SVMs                                         |    12 | Numeric parsing and linear evaluation work. Automatic detection still needs matching camera-specific remapping, BGR-to-gray/HOG features, thresholds, and multi-patch voting.           |
| Model catalog                                                        |     1 | Model/algorithm discovery data; does not execute a transform. Its encoded catalog identifies ColorPlus model variants.                                                                  |
| AI stitch, ColorPlus, defringe, deflicker, JPEG denoise `.ins` files |     6 | Wrapped learned-model resources; require model decoding, inference, and the matching image/temporal pipeline.                                                                           |
| AI seam CoreML/Espresso constituents                                 |     8 | Complete model data, but no portable graph executor, seam-strip extraction, flow scaling, or compositing implementation.                                                                |
| Underwater restoration resources                                     |    10 | Original Legacy ILUT and complete nine-member AI group; explicit restoration through scalar CPU and optional independent MNN.                                                           |
| Studio sharpening JSON                                               |     1 | X5 perspective-output tuning at heights 1080/2160 and FOVs 20/40/60/75 degrees. Strength becomes zero at 75 degrees; applying it to 360-degree output would add no intended sharpening. |

The SVM reader accepts the bundled schema's direct support-vector/alpha order.
It rejects indexed classifier decision functions, whose vector indirection is
defined separately by
[OpenCV's reader](https://github.com/opencv/opencv/blob/4.x/modules/ml/src/svm.cpp).
Prediction checks public mutable model dimensions and rejects nonfinite inputs
or results. Tests cover wrong vector order, truncated model arrays, and numeric
overflow alongside every bundled model's parsing and digest verification.

ColorPlus's `.ins` payload is an adaptive-LUT prediction model, not a CUBE
sample table. The vendor implementation separately specifies its model path,
generated LUT path, recalculation cadence, local contrast, and panorama-specific
seam handling. Deflicker additionally requires motion alignment and frame
history. JPEG denoising uses sensor/ISO-dependent, overlapping tile processing.
Feeding these bytes into the CUBE consumer or applying arbitrary substitute
filters would not implement their algorithms.

## Verification

Tests cover malformed CUBEs and domains, asymmetric independent interpolation
examples, original-table reference values, recording-marker parsing and
conflicts, CLI/Python option mappings, and actual image/video session output.
The bundled X5 table maps RGB8 `[96, 112, 128]` to `[29, 75, 112]`. GPU tests
compare transformed output with CPU application of the same table to the
unconverted panorama, allowing at most one 8-bit code value per channel. A
BT.709 primary-color test verifies the CPU encoder matrix and range.

Generated dual-track INSV exports were also compared against FFmpeg's
independent `lut3d` filter with trilinear interpolation. For all three generated
frames on CPU and Metal, automatic output differs from FFmpeg's reference by at
most one 8-bit code value per channel. These tests validate the transform and
integration; they do not establish learned-model support or cross-platform
release qualification. CPU and Metal HEVC exports also decode all three frames
with the expected Auto and Preserve color tags. Video tests used VideoToolbox
because the local bundled FFmpeg runtime does not provide a software HEVC
encoder.

## Underwater restoration

`StitchConfig.underwater_color` defaults to Off. Legacy and AI are explicit
selections with strict mode-specific controls. The shared
`UnderwaterColorSession` prepares verified resources once, processes packed RGB8
without moving pixels, and retains bounded temporal state. Export jobs reset it
at recording boundaries and before independent selected images; GPU stitching
uses the same CPU restoration after readback. Typed GPU retry recreates the
entire attempt, including color state. See [housings](housings.md) for exact
resource provenance, formulas, defaults and qualification limits.

The legacy underwater ILUT is distinct from ordinary I-Log conversion and from
Studio's alternate `Contents/data/models/underwater.ilut`. AI loads verified
original model 197/198, the diving feature database and all four style presets.
No Insta360 runtime library is loaded. MNN is an optional independently compiled
CPU dependency, pinned by source commit and archive digest.
