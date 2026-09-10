# Housings and underwater correction

The [generated catalog](housing-catalog.md) records the camera/housing table,
optical IDs, native curves, source masks and evidence paths. Its source is the
Rust registry, not a separately maintained documentation table. Paths identify
the SDK distribution or application bundle and contain no workstation paths.
Official SDK/app constants are used as specifications; synthetic validation and
real-recording qualification are reported separately.

## Selection and overrides

`StitchConfig` separates `housing`, `environment`, `lens_accessory` and
`mounting_accessory`. Each defaults to `Auto`. The old `OpticalSetup` enum,
`optical_setup` field and `--optical-setup` flag have been removed. Old
serialized configuration fields produce an error instead of being silently
ignored.

Housing values are `Auto`, `None`, `VentureCase`, `DiveCase`,
`SphericalDiveCase`, `InvisibleDiveCase` and `DiveCasePro`. Environment is
`Auto`, `Air` or `Underwater`. Lens guards and ND filters are separate lens
accessories. `DiveBuddy` is a mounting accessory; Studio exposes it separately
and routes it through underwater housing correction. No distinct Dive Buddy
physical curve has been established. Recorded mount detection is not
established, so `Auto` currently resolves the mount to `None`; select
`DiveBuddy` explicitly when needed.

Each explicit component overrides its detected component. Detection uses the
recorded offset/accessory state, then an unambiguous encoded lens ID when that
state is absent. Stored guard results can resolve an automatic guard state.
Image classifiers are not run. Merely finding optional calibration profiles in
metadata does not prove which accessory was selected. Inconclusive detection and
incompatible combinations produce typed errors. A fully explicit request can
override inconclusive recorded accessory detection.

`MediaInfo.optics` reports detected choices, evidence, encoded lens ID and any
ambiguity during bounded probing, including recordings whose rendering is
unsupported. `ResolvedCalibration.optical_resolution` and `ExportResult.optics`
retain requested, detected and effective settings, source/target IDs and whether
sensor-crop normalization was applied. A recorded selection is evidence of a
firmware setting, not an independent visual measurement of installed hardware.

```rust
use insta360_rs::{Environment, Housing, StitchConfig};
let config = StitchConfig {
    housing: Housing::DiveCasePro,
    environment: Environment::Underwater,
    ..StitchConfig::default()
};
```

```python
from insta360_rs import Environment, Housing, StitchConfig
config = StitchConfig(housing=Housing.DIVE_CASE_PRO,
                      environment=Environment.UNDERWATER)
```

CLI equivalents are `--housing dive-case-pro --environment underwater`.
`underwater_photogrammetry(Housing::Auto)` in Rust and
`StitchConfig.underwater_photogrammetry()` in Python preserve housing detection,
select the underwater environment, and leave color restoration off.

## X5 standard and Pro correction

| Housing             | Underwater ID | Air ID | Native conversion selectors | Lower contour (azimuth, half FOV), degrees |
| ------------------- | ------------- | ------ | --------------------------- | ------------------------------------------ |
| Invisible Dive Case | 117           | 118    | 54 / 55                     | (0,90.5), (10,90.5), (20,91.8), (60,94)    |
| Dive Case Pro       | 119           | 120    | 56 / 57                     | (0,91), (10,91), (32,92.5), (55,93.5)      |

Metadata offset states 10/11 identify Pro underwater/air. They are distinct from
states for the earlier housing. The former implementation incorrectly mapped
state 10 to 117 and applied the earlier contour. Pro conversion now uses 119/120
and their exact native physical curves. Generic embedded profile names
`InvisibleDiveWater` and `InvisibleDiveAir` do not establish the Pro revision
and are not rebound to it by name. An offset already carrying the requested ID
is not converted twice. Per-unit principal points, extrinsics and non-radial
distortion remain unchanged. The target mirror parameter and radial fit come
from the first lens; each lens retains its own measured source scale.

Standard housing Objective-C defaults provide full FOV 190/200 and blend 186.
For Pro, the inspected iOS template pipeline initializes overlap to 10 degrees;
`getLeftSphereAlpha` divides it into two 5-degree seam half-widths, represented
as full blend 190 in this library. Provenance is `TemplateBlenderBase`
constructor 0x16c9974, `getMapAndAlpha` 0x16cd480 and `getLeftSphereAlpha`
0x16cfb14 in SDK 1.10.4 arm64. This is a native template default, not proof that
every Studio UI/render-model override chooses it. A valid explicit metadata
blend angle takes precedence.

## Radial masks and sensor coordinates

Housing exclusion operates in source-fisheye coordinates. The variable-length
angular contour is projected using each lens's calibration, rasterized and
eroded on the lower hemisphere. Feathering uses the native L2 mask5 chamfer
distance field multiplied by `0.24390244483947754` and clamped to 1. CPU and GPU
consume the same prepared field. Invalid mask taps are excluded from bilinear
sampling and color statistics; valid taps are renormalized. The cache retains
one prepared pair per stitcher, bounded by the source dimensions. Cloned CPU
stitchers share that cache; `CpuStitcher` is no longer `Copy`.

The sensor crop in metadata is a separate coordinate mapping, not a rectangular
housing mask or an output crop. Native crop conversion scales pixel centers as
`(center+0.5)*scale-0.5`, subtracts centered crop margins and recorded offsets,
and updates source canvas dimensions before housing conversion. A current
calibration already in the destination canvas is not cropped again. V1 crop
conversion and unsupported source orientations are rejected explicitly. The
supplied X5 Pro recording uses a 5376→5312 sensor window and 2880-pixel decoded
lens images; those are different stages of the source mapping.

## Other cameras and remaining evidence limits

Registered ONE, ONE X, ONE R/RS panorama modules, X2, X3, X4, X4 Air, X5 and X6
can use supported V1/V2/V3/V6 encoded calibrations with validated decoded
layouts. Dual-track, two-file and explicitly identified horizontal packed
panorama sources share the same pairing path. Motion profiles remain separate:
use stabilization `Off` when no supported motion profile exists. HDR/PQ/HLG and
10-bit stitched output are not implemented. V1 uses the recovered native lens-ID
degree-coefficient dispatcher and shares normalized projection with V2. The
resolved polynomial provenance identifies dedicated V1 table rows, explicit
native generic defaults and recorded V2 coefficients; it does not claim
per-camera or physical-housing qualification. V1 sensor-crop conversion remains
unsupported. See [polynomial normalization](calibration.md) for the equations,
native evidence and calibration mutation/serialization contract.

X4 Invisible Dive Case uses encoded lens IDs 86 underwater and 87 in air, with
its own eleven-point Method2 source mask. The resolver converts supported X4 V6
calibration to either target. X6 uses 198 underwater and 199 in air, with its
own four-point Method2 mask and V6 converter. X6 water conversion sets the
verified target mirror parameter and radial model; air conversion fits the
verified physical curve. These routes preserve measured principal points and
extrinsics. Already encoded matching housing calibrations avoid conversion.
Converting a bare V3 offset to these housings remains unsupported.

Method2 interpolates boundary angles before projection; X5 Method3 interpolates
projected squared radii. CPU and GPU share the prepared result. The numerical
models, conversion tables and native provenance are documented in
[calibration](calibration.md#x4-and-x6-housing-conversion). Synthetic
calibration and CPU/GPU parity tests establish implementation contracts;
physical X4/X6 housing recordings have not been qualified.

X4 Air's bare V6 suppliers 131 (HJ) and 142 (LG) convert to 147/148 underwater
or 149/150 in air, respectively. The four encoded housing profiles use the
verified eleven-point Method2 mask. Air FOV is 200 degrees and water FOV 190;
both use the native template's 190-degree full blend support unless metadata
provides an override. Target fitting shares the first lens's mirror parameter
while retaining each lens's measured source scale. Other X4 Air conversion
routes remain explicit errors. See the
[native supplier evidence](calibration.md#x4-air-supplier-housing-profiles).
Physical X4 Air housing recordings remain unqualified.

The catalog preserves further evidence without claiming complete algorithms: X2
spherical housing conversion is explicitly unsupported by the native SDK. G01
module classifiers expose distortion/environment selectors but do not supply an
interchangeable physical calibration. Missing values remain explicit.

## Opt-in underwater color restoration

`UnderwaterColorOptions` selects `Off`, `Legacy` or `Ai`. The default is `Off`;
selecting a housing never enables color restoration. Strength is finite and in
`[0,1]`; legacy balance has the same range. Legacy accepts strength and balance,
while AI accepts strength and style index 0–3. Controls for another mode are
rejected. Legacy defaults to strength 0.8/balance 0.5; AI defaults to strength
1/style 0. Strength zero preserves input bytes.

The FFmpeg export layer applies restoration to the stitched RGB8 panorama after
any requested I-Log conversion. GPU stitching remains available, but enabled
restoration uses RGB readback and CPU processing instead of direct YUV output.
Restoration never changes projected coordinates, source selection or seam
ownership. Color changes can still affect photometric matching, so the
underwater photogrammetry preset leaves restoration off.

The core `underwater::UnderwaterColorSession` API has no FFmpeg dependency.
Prepare it once with options, dimensions, a positive rational frame rate and an
`AssetProvider`, then call `process_rgb8` with packed RGB bytes and a finite
presentation time in seconds. Sessions retain their prepared buffers. Call
`reset` between unrelated images or recording chapters; non-increasing times
also reset history. Smoothing follows processed frames rather than elapsed
seconds. Frames must match the prepared dimensions, which are limited to 64
megapixels; legacy requires each dimension to be at least 64 pixels.

### Legacy scalar CPU reference

The implementation follows the scalar `UnderwaterCorrectionCpuV2` path in SDK
1.10.4 `INSCoreMedia`, using the original Studio 5.9.10 resource
`Contents/data/b56efdaa/underwater.ilut`. Its SHA-256 is
`fd203f878cccf93c844f90db2d49c1508ba380458582cee7a0af12383269acde`. The separate
`Contents/data/models/underwater.ilut` has a different format and caller history
and is not substituted.

The stages are channel compensation, auto-brightening, haze removal and an
integer tetrahedral 3D LUT. RGB public buffers are converted to the BGR ordering
used by the reference LUT. The ILUT header is three little-endian integers
`16,256,4`, followed by a 65³ table with the third channel varying fastest, and
a 128-byte description plus four-byte tag. Signed interpolation divides the
weighted delta before adding its origin; negative division truncates toward
zero. Strength blends the original LUT against its identity grid during
preparation, with terminal coordinates clamped to 255 and ties-to-even rounding.

Native `SetParams` adjusts the first two channel coefficients around balance 0.5
and scales all channel coefficients and haze amount by strength. Brightness uses
`1 + (smoothed_gamma-1)*strength`. Gamma and atmospheric-light history use
`0.98*previous + 0.02*current`; reset discards that history. Degenerate channel
statistics preserve the reference whole-pipeline bypass. Scalar CPU equations
are the reference here: the vendor SIMD and Metal implementations fuse stages
differently and are not asserted byte-identical.

Evidence addresses in the SDK arm64 `INSCoreMedia` image are
`UnderwaterCorrectionCpuV2::SetParams` 0x1f63f30, `ProcessFrame` 0x1f6b6b8,
`AdjustLut` 0x1f64b6c and the integer LUT merge worker 0x1f65850. Public default
strength/balance come from `INSUnderwaterInfo`'s `initLutFilePath:` at 0xc0ebc
and constant 0x506a7d0, rather than the lower-level CPU constructor's initial
strength 1.

### AI models and execution

The optional `underwater-ai` feature executes independent, source-built MNN
3.6.1 CPU sessions. It never loads or links an Insta360 runtime library.
Original encrypted model bytes and the complete style group are integrity
checked before native parsing. Model 197 is split across two publishable data
crates; reconstruction checks the whole original identity before decoding. All
original bytes, licenses, source-relative paths and hashes remain in the asset
manifests. Camera/platform qualification stays `Unqualified`: explicit
processing does not imply physical-camera or Studio-export qualification.

| Studio model       | Original bytes | Original SHA-256                                                   | Selected output          |
| ------------------ | -------------: | ------------------------------------------------------------------ | ------------------------ |
| 197, neural preset |     15,009,214 | `53a24a86a41673adfbb56709cda53ec16584801d8918ad5ee0306f5027a92d5e` | `output`, `[1,3,289,17]` |
| 198, deep features |      7,574,990 | `0d9998552303ed52e7da489e126d6ac0884e7990fda05c14ae0e7ebb62afaf52` | `avgpool`, `[1,576]`     |

The desktop V1 wrapper encrypts the first and last 2000 model bytes using
AES-256 ECB. The decoded MNN payloads have their own verified hashes. Model
198's 365-component classifier output is not used. Feature input is RGB NCHW
`[1,3,224,224]`, normalized with ImageNet means `[0.485,0.456,0.406]` and
standard deviations `[0.229,0.224,0.225]`, then L2-normalized after inference.
Style matching uses the original nine-record, 576-component database: retain the
chosen style's original vector when its reference record is in the nearest five;
otherwise select the nearest record's transferred 256-component vector.

Model 197 takes `ctt_h_img=[1,3,289,17]` (the 17³ identity grid),
`ctt_l_img=[1,3,256,256]` (the resized image) and `sty_l_img=[1,1,1,256]`. Image
inputs are RGB NCHW divided by 255. Bilinear resizing uses pixel centers and
11-bit interpolation weights. The recovered precomputation defaults update the
model LUT every ten processed frames and rematch style every sixty frames; new
LUTs use 0.8 previous + 0.2 current smoothing. Reset starts with the first new
LUT. CIELab luminance blending uses 0.5, followed by the selected total strength
and an RGB integer tetrahedral LUT with stride 16.

Evidence includes Studio 5.9.10
`FilterHelper::CreateUnderwaterColorPrecompution` at arm64 address 0x100293bb0;
SDK `NeuralLutWrapper::Open` at 0x20a39d4, `GetStyleVecByIndex` at 0x20a4510,
`Process` at 0x20a48f8, `RunDeepFeatureExtractor` at 0x2d15994 and
`NeuralLutSmoothBase::GetFinalLUT` at 0x1d62d04. These describe the recovered
CPU precomputation settings, not every Studio editor override, cached analysis
workflow or GPU backend.

The private C++ shim validates input names, shapes, float types and output
shapes, copies tensors through persistent host buffers, and catches exceptions
before returning through C. Rust verifies slice lengths and finite inputs;
non-finite outputs become typed errors. Models are exclusively owned by one job,
can move between threads, and cannot run concurrently through shared access. No
Rust buffer pointer is retained by native code.

Tests cover original/decoded identities, malformed resources, channel order, all
six tetrahedral branches, independent scalar arithmetic and MNN tensor reference
values, all four styles, temporal updates/reset, invalid public options and
stable AI buffer allocations across repeated frames. These tests establish those
contracts. Full numerical parity with Studio exports, real underwater recordings
and physical GPU paths has not been demonstrated.

### Classifier evidence

Native artifacts also contain image-driven accessory/environment classifiers,
underwater-scene models and preview-only capability checks. Those are recorded
as research evidence, not automatically enabled algorithms. Android preview
restrictions do not define offline export support. See
[asset usage](asset-usage.md), [calibration](calibration.md) and
[testing](testing.md) for implementation and qualification boundaries.
