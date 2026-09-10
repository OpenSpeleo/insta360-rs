# Insta360 calibration offsets and camera profiles

`insta360-rs` treats an INSV offset as camera geometry, not as a generic list of
pinhole coefficients. Each offset generation has a different projection model
and a different number of fields.

## Evidence and wire layouts

The layout was reconstructed from three agreeing sources:

- `INSCoreMedia` debug symbols name the four parser families
  `ParsePinholePoly1`, `ParsePinholePoly2`, `ParseOmniRadtan`, and
  `ParseOmniRadtanPro`. Their local coefficient types are respectively empty,
  `Vector4d`, `Vector5d`, and `Vector13d`.
- `Insta360Lens::InstrinsicParams` symbols name `fx`, `fy`, `center`, `xi`,
  `coeffs`, `len_type`, and `model_type`. The vendor model enum identifies V3
  and V6 as radtan omni models 3 and 6.
- `VID_20181001_225939_00_002.insv` contains valid V1/V2/V3/V6 strings with
  exactly 16, 34, 40, and 56 underscore-delimited fields including the leading
  lens count.

The decoded two-lens layouts are:

| Version | Per-lens fields                                                                                             | Global fields                         | Projection            |
| ------- | ----------------------------------------------------------------------------------------------------------- | ------------------------------------- | --------------------- |
| V1      | radius, center x/y, three Euler values                                                                      | width, height, packed lens type/flags | polynomial pinhole V1 |
| V2      | radius, center x/y, three Euler values, translation x/y/z, 4 coefficients, width, height, lens type         | packed flags/version                  | polynomial pinhole V2 |
| V3      | xi, focal x/y, center x/y, three Euler values, translation x/y/z, 5 coefficients, width, height, lens type  | packed flags/version                  | omni radtan           |
| V6      | xi, focal x/y, center x/y, three Euler values, translation x/y/z, 13 coefficients, width, height, lens type | packed flags/version                  | omni radtan pro       |

For V2, V3, and V6 the high 16 bits of the trailing word equal the offset
version. The supplied sample's trailing words are `0x20400`, `0x30400`, and
`0x60400`. V1 instead places the lens identifier in the low ten bits of its
trailing word; the sample has `0x471`, which is flags `0x400` plus lens
type 113.

`ParsedLens::distortion_coefficients` retains every native coefficient. `k1`,
`k2`, and `k3` remain aliases for the first three values only for API
compatibility; they do not fully describe V3 or V6.

## Camera/lens registry

One reviewed registry identifies ONE, ONE X, ONE R/RS, ONE X2, X3, X4, X4 Air,
X5/A3, and X6/C9 aliases and maps their evidence-backed lens IDs to optical
setups, full FOV, blend angle, accepted projection generations, and optional
source-mask recipes. Every value retains header or static-binary provenance.
Shared lens identifiers remain camera-scoped; when a camera name is absent, a
value is accepted only when all matching registry records agree.

Optical inspection compares housing, environment and lens-accessory identities
separately from render geometry. Shared lens IDs 86/87 identify the invisible
dive housing in water/air without a camera name, while rendering still requires
the camera name to select the different X3 and X4 source masks.

The registry is not a calibration database. The recording's current/original
offset remains the sole source of per-unit intrinsics, distortion, principal
points, and extrinsics. `ResolvedCalibration` stores the selected camera family
and `ResolvedLensGeometry` so CPU and GPU consume identical angles without a
per-pixel camera-ID switch. A valid positive `blendAngle` metadata value from
tag 128 overrides the registry fallback.

`ResolvedCalibration.raw_offset` retains the selected source text unless a
housing conversion replaces it with the converted native V6 offset. Sensor-crop
normalization and edits to public calibration fields do not synchronize that
string. Renderers use the resolved lens and canvas fields; serialize the
complete resolved structure when retaining its normalized geometry.

A recorded blend angle of exactly 180 degrees represents zero angular overlap.
CPU and GPU render it as the same finite hard seam, with equal ownership only on
the seam plane, instead of dividing by the zero-width overlap belt.

The parser also retains crop, rotation, file category, stream layout/order,
codec, capture-offset version, offset/accessory state, guard detection, timing,
and unknown protobuf fields. These values inform dispatch, source-layout
validation and explicit sensor-crop normalization. See
[housing coordinates](housings.md#radial-masks-and-sensor-coordinates).

## Current versus original

`CalibrationResolver::resolve_metadata` requires an explicit `OffsetSource`. It
never falls back between current and original offsets. The distinction is
semantic: an original offset is the factory copy, while a current offset may
already have been converted for a lens accessory. Falling back could therefore
silently use the wrong refraction model.

The resolver accepts an already-encoded matching lens profile. Housing,
environment, lens accessories and mounting are separate typed fields in
`OpticalSelection`. Requested components override conclusive detection; unknown
or incompatible combinations fail explicitly. `MediaInfo.optics` reports bounded
inspection without requiring a renderable model.

| X5 lens ID | Housing             | Environment      |
| ---------- | ------------------- | ---------------- |
| 113        | None                | Air              |
| 114        | None                | Underwater       |
| 117 / 118  | Invisible Dive Case | Underwater / Air |
| 119 / 120  | Dive Case Pro       | Underwater / Air |

Pro conversion uses the exact native physical curves for source and target IDs.
The earlier standard conversion uses the recording's embedded physical curves.
Neither route replaces measured extrinsics, principal points or non-radial
parameters. Already-matching IDs are not converted again. The
[generated catalog](housing-catalog.md) includes the complete registry, exact
coefficients, source software/version and artifact-relative paths.

## Embedded profile descriptors

The supplied sample's named profile submessages have one of two protobuf shapes:

- field 1 string plus six repeated field 2 fixed64 doubles; or
- field 1 string plus one field 2 varint classifier value.

`ParsedEmbeddedProfile` validates and preserves both shapes. Static tracing of
`OffsetConvert::getPhysical2PixelScale`,
`OffsetConvert::getV6DistortAndFocalFromLens`, and
`OffsetConvert::converOffsetNormal` establishes that the six doubles are an
angle-to-physical-radius polynomial evaluated in degrees.

For X5 V6 conversion, the resolver first fits the recorded Omni model to the
source physical curve to recover pixels per physical-radius unit. It then fits
the target curve against `[u, u^3, u^5, u^7, u^9]`, where
`u = sin(theta) / (cos(theta) + xi)`. The target fit uses the first lens's `xi`
once; both converted lenses receive that `xi` and the same radial slots0–4.
Their individual source scales still use each original lens's `xi`, radial terms
and `sqrt(fx*fy)`, producing independent pixel focal lengths. Principal points,
measured extrinsics and tangential/thin-prism slots5–12 survive. In iOS SDK
1.10.4's artifact identified below, selectors 54–57 map targets 117–120 through
table 0x529e4d8 into the V6 branch0x1e2e0b4. The shared target fit is at
0x1e2e18c–0x1e2e1a8 and target `xi` stores at 0x1e2e34c–0x1e2e358. The
regenerated offset carries the target lens type and validates like a native V6
offset.

## X4 and X6 housing conversion

X4 uses lens 86 underwater and 87 in air, also used by X3; the camera identity
selects the X4-specific mask. X6/C9 uses 198 underwater and 199 in air.
Conversion from supported V6 source optics is prepared once. Already encoded
matching V2/V3/V6 housing offsets retain their per-unit data; converting a bare
V3 offset through these new routes is not enabled.

All native offsets below refer to the ARM64 image in **Insta360 iOS SDK
1.10.4**, artifact
`iOS_v1.10.4/INSCameraSDKSample-bluetooth/Frameworks/INSCoreMedia.xcframework/ios-arm64/INSCoreMedia.framework/INSCoreMedia`
(SHA256 `3b905b46e46053d9c426c4af8bb28e449666d3ddf1c03419ab564ed2b0a01409`).
These are recovered equations and numeric parameters, implemented without a
vendor runtime dependency.

| Route              | Native evidence                                                                                                            | Prepared behavior                                                                                                                                   |
| ------------------ | -------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| X4 water86 / air87 | `OffsetConvert::convertOffset` selectors 47/48, target stores 0x1e31d10/0x1e31d20; V6 branch 0x1e32000–0x1e3257c           | Source physical-scale least-squares fit, then target radial fit with recorded `xi`; preserve principal points, extrinsics and distortion slots 5–12 |
| X6 water198        | `convertOffset` selector 76, target table 0x529e4d8; source-scale branch 0x1e2e218; `getPhysical2Pixel90DegScale`0x1e3d678 | Source scale at 90 degrees; apply the fixed target model below                                                                                      |
| X6 air199          | `convertOffset` selector 77, same table; target model lookup 0x1e2e184 falls through to radial fit                         | Source physical-scale fit for each lens; shared target radial fit and `xi` from the first lens                                                      |

The fitted target model is computed once from the first lens's `xi`. X4 shares
its target radial coefficients and physical focal while retaining each source
lens's `xi` (fit 0x1e32140–0x1e32154, radial stores 0x1e32244–0x1e32260). X6 air
also shares the first lens's target `xi` (fit 0x1e2e18c–0x1e2e1a8, `xi` stores
0x1e2e34c–0x1e2e358). Every source lens contributes its own measured focal, `xi`
and radial model to its physical-to-pixel scale.

The converter first asks `ins::Lens::get6thOrderPolynomialCoeffs`0x1e4e650. The
following seven coefficients, in ascending powers of an angle in degrees,
override the generic five-coefficient `PhysicalCurve`. They are conversion data;
the public generic-curve table keeps its separate meaning. Other verified lens
IDs use the generic polynomial padded with zeros.

| Lens ID | Coefficients `[c0,c1,c2,c3,c4,c5,c6]`                                                         | Native case / first six doubles |
| ------- | --------------------------------------------------------------------------------------------- | ------------------------------- |
| 106     | `[0,0.0222,-1.318e-5,1.702e-6,-3.694e-8,4.445e-10,-2.221e-12]`                                | 0x1e4e828 /0x529df30            |
| 107     | `[0,0.02208,-1.568e-5,1.84e-6,-4e-8,4.792e-10,-2.359e-12]`                                    | 0x1e4e870 /0x529df00            |
| 108     | `[0,0.02203,-9.484e-6,1.516e-6,-3.26e-8,4.019e-10,-2.059e-12]`                                | 0x1e4e708 /0x529ded0            |
| 193     | `[0,0.0446046592,-2.21051865e-5,2.45643803e-6,-2.58126467e-8,1.61274114e-10,-1.04220336e-12]` | 0x1e4e6a8 /0x529dea0            |
| 197     | `[0,0.0454209969,-2.66048702e-5,2.86818832e-6,-3.44498211e-8,2.66650288e-10,-1.60467867e-12]` | 0x1e4e750 /0x529de70            |
| 198     | `[0,0.0484677266,-0.000221657224,1.51604144e-5,-2.78962065e-7,1.94921255e-9,-4.66743434e-12]` | 0x1e4e798 /0x529de40            |
| 199     | `[0,0.0459657399,-0.000404190186,3.03215887e-5,-6.40814172e-7,5.3997503e-9,-1.64037143e-11]`  | 0x1e4e7e0 /0x529de10            |

The seventh value is stored as a literal in each case. X6 water's fixed model
comes from `getV6distort`0x1e2cfd4, data 0x529dc40 and literal stores
0x1e2cff0–0x1e2d014: `xi=2.45543`, physical focal `9.2635`, and radial slots 0–4
`[2.799666,-19.355603,32.47295,92.466926,0]`. Source pixels per physical unit
are `f*R(u)/P(90)`, with `f=sqrt(fx*fy)`, `u=1/(xi+cos(pi/2))`, recorded radial
polynomial `R(u)=u+k0*u^3+...+k4*u^11`, and the source conversion curve `P`. The
new pixel focal is this scale times 9.2635. Recorded principal points,
extrinsics and distortion slots 5–12 are preserved; `xi` changes to 2.45543.
Lens197 is included as recovered conversion data, without claiming a complete
registered X6 guard profile.

X4 housing mask selection is at 0x16d4a14 in `calcFisheyeMaskONEX4AndProtector`;
it loads eleven knots and calls `fisheyeCircleMaskFromOffsetErodeMethod2` at
0x16d537c. X6 selection is at 0x16d7ee0 in `calcFisheyeMaskC9AndProtector`,
loads four knots from 0x5149e50 and calls Method2 at 0x16d8834. Both interpolate
half-FOV angles before calibrated projection, then share integer rasterization,
erosion and L2/mask5 feathering with scalar 0.24390244483947754. This
interpolation order differs from X5 Method3, which interpolates already
projected squared radii. Exact knot lists, FOV/blend defaults and their separate
provenance are in the generated [housing catalog](housing-catalog.md).

Independent QR fit goldens, the analytic X6 water equation, serialized-offset
idempotence, repeated preparation and CPU/GPU mask parity cover these routes.
They do not establish physical-camera qualification for X4 or X6 housing media.

## X4 Air supplier housing profiles

The original **Insta360 Android SDK 2.1.5** demo APK contains the complete B2
housing branches in
`AndroidSDKDemo/app-debug-2.1.5_1787657291340.apk!/lib/arm64-v8a/libarvbmg.so`
(SHA256 `6cea9beda80ffe53eea85f04503a07cd25ff7a573b466df6d8e54aec7575a7e0`). The
extracted library was hash-checked against that APK member. B2's X4 Air identity
is explicit in the APK's `CameraType.X4AIR` declaration, and native
`ResolveOffsetShell` references `InstaCamera::X4Air` through relocation
0x84e29f8.

| Bare supplier lens | Water target / native selector | Air target / native selector | Target full FOV water / air |
| ------------------ | ------------------------------ | ---------------------------- | --------------------------- |
| 131 (HJ)           | 147 /62                        | 149 /64                      | 190 /200 degrees            |
| 142 (LG)           | 148 /63                        | 150 /65                      | 190 /200 degrees            |

`ResolveOffsetShell` selects bare suppliers 131/142 at 0x5416d50/0x5416e84 and
0x5416eb8/0x5417014. `convertOffset` dispatch table 0x1e8ce44 selects the four
branches that load target 147 at 0x5b9a410,148 at 0x5b9a3f0,149 at 0x5b9b574 and
150 at 0x5b9cad0. Each tail-calls `converOffsetNormal`0x5b963cc.

V6 target lookup `getV6distort`0x5b98440 returns false for all four housing IDs,
so `converOffsetNormal` fits the target once with the first source lens's `xi`
at 0x5b96614–0x5b9662c. Its loop computes a separate source scale from each
lens's recorded `xi`, geometric-mean focal and radial coefficients at
0x5b96660–0x5b9669c. Both target lenses receive the shared `xi` and first five
radial coefficients; measured centers, extrinsics and distortion slots 5–12
remain unchanged. The portable implementation enables these four bare-V6 routes
and matching already encoded housing calibrations. Guard-to-housing and
conversion from an already converted housing to a different housing/environment
remain explicit errors for X4 Air.

The native seventh-coefficient getter returns false for these targets and bare
suppliers. `getV6DistortAndFocalFromLens`0x5b98790 therefore uses these exact
five-coefficient degree polynomials from `ins::Lens::getCoeff`:

| Lens | Polynomial coefficients in ascending powers                                                    | Native branch / data                                                                            |
| ---- | ---------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| 131  | `[0,0.023200178479423714,-2.6527512394617657e-5,1.2910990660481257e-6,-1.0421589669384779e-8]` | 0x3fcd958 /0x15191a0,0x15191b0 and literal 0xbe4661545ad02d9d                                   |
| 142  | `[0,0.024508114438217286,-2.3584121988814713e-5,7.382934532711228e-7,-6.482923649818883e-9]`   | 0x3fccf60 /0x151a2a0,0x1515b30 and literal 0xbe3bd80cc8899ba4; matches the registered iOS curve |
| 147  | `[0,0.02311,6.271e-5,-5.268e-7,-1.293e-9]`                                                     | 0x3fccc88 /0x1b989a8                                                                            |
| 148  | `[0,0.0245,5.707e-5,-9.534e-7,2.095e-9]`                                                       | 0x3fcdb3c /0x1b989f8                                                                            |
| 149  | `[0,0.0213,0.000204,-3.485e-6,1.458e-8]`                                                       | 0x3fcccec /0x1b989d0                                                                            |
| 150  | `[0,0.02271,0.00019,-3.754e-6,1.706e-8]`                                                       | 0x3fcdc20 /0x1b98a20                                                                            |

`ins::Lens::getFov`0x3fede38 reads table 0x1b9b7c8. Air targets use200 degrees;
water targets use190. `TemplateBlenderBase`0x4092974 stores the default10-degree
overlap, halved by `getLeftSphereAlpha`0x4099fa8: the portable fallback uses 190
degrees of full blend support. Recorded blend metadata remains authoritative;
this template default does not claim every editor's UI override is identical.

`calcFisheyeMaskB2AndProtector`0x40a10a8 selects 147–150 and assigns the same
verified eleven angular knots as X4 to both lenses. Method2 calls at 0x40a1768
and 0x40a192c interpolate angles before projecting. `distanceTransform`0x40a1aac
uses L2/mask5;0x40a1970/0x40a1994 construct scalar 0.24390244483947754. The
source mask shares the X4 implementation with its own Android provenance.

Public `ProfileProvenance.source` identifies the exact inspected distribution;
`software()`, `version()` and `binary_sha256()` use that identity. Android
values cannot inherit an iOS hash merely because both sources are SDK binaries.
Independent QR focal goldens with unequal source mirror parameters, supplier
selection, encoded idempotence, provenance assertions and CPU/GPU mask parity
cover this implementation. No physical X4 Air housing recording is qualified.

## Supplied sample

Both current and original offsets in the supplied X5 recording are lens type
113, while metadata tag 68 records state 10: Dive Case Pro underwater. Automatic
selection now converts this to type 119. Mapping it to type 117 was a bug:
standard Invisible Dive Case and Pro are different revisions. Optional embedded
profile names alone cannot establish the selected revision. A caller can
explicitly choose another housing/environment when recorded settings are
incorrect; the resolved report retains both detected and effective values.

The CPU stitcher implements the native V6 projection from the Metal source
embedded in the licensed Studio worker library. It uses the unified omni
normalization `x,y / (z + xi * norm)`, followed by five radial terms, two
radius-varying tangential pairs, and four thin-prism terms. It also uses the
vendor's equirectangular sphere convention, the parser's `+pi/2` second-Euler
basis conversion, the physical half-turn for the second X5 lens, and the X5
field-of-view values recovered from `Insta360Lens::GetFov`.

V1 and V2 share native degree-to-radian polynomial normalization. V1 obtains its
five degree coefficients from the lens-ID dispatcher; V2 retains the four
recorded coefficients. In both cases the native conversion ignores the constant
term and evaluates `Q(t)=t*(b1+t*(b2+t*(b3+t*b4)))`, with `t` in degrees. For
native reference full FOV `F`, radius `r`, and `S=180/pi`, the normalized focal
length is `r*b1*S/Q(F/2)` and the radian coefficients are
`[1, b2*S/b1, b3*S²/b1, b4*S³/b1]`. The reference FOV normalizes the encoded
radius; it is separate from renderer clipping and blend settings.

The recovered SDK 1.10.4 arm64 `INSCoreMedia` symbols are `GetDegreeCoeffs` at
`0x19f2164`, `GetFov` at `0x19f4bd4`, and `PolyPinholeModelToInstrinsicParams`
at `0x19f4da0`. V1 parsing calls that conversion at `0x19eff08`; V2 parsing
calls the same conversion at `0x19f1ea4`. The binary SHA-256 is
`3b905b46e46053d9c426c4af8bb28e449666d3ddf1c03419ab564ed2b0a01409`. The
242-entry degree dispatcher masks the lens ID to eight bits. Some IDs have
dedicated coefficients, including 13/17, 19 and 113; others, including 78,
86/87, 117–120 and 198/199, explicitly select the native equidistant default.
The public provenance distinguishes that default from dedicated table rows and
recorded V2 coefficients. Neither generic defaults nor table rows substitute for
per-unit radius, principal points or extrinsics, or establish physical housing
qualification. Invalid native normalization, including the non-positive
denominator in table row 37, remains inspectable but cannot render.

`ParsedLens::distortion_coefficients`, `radius`, `fx` and `fy` retain their
native values; `polynomial_projection` stores the derived radian coefficients,
dimensionless focal scale and provenance. CPU and GPU consume those coefficients
with effective focal lengths `fx*scale` and `fy*scale`. Decoded-coordinate
scaling continues to apply to `fx`/`fy` without changing the degree coefficients
or radius. Parsing prepares valid models automatically. After editing the model,
lens ID or raw coefficients, call `refresh_polynomial_projection`; validation
rejects stale prepared parameters. Older serialized values without the optional
field remain inspectable and need this refresh before rendering. A failed
refresh preserves the previous parameters and does not make stale data valid.

Independent high-precision degree-radius coordinates cover dedicated V1 rows and
recorded V2 coefficients, including nonzero V1 constant terms. A digest of all
242 native coefficient/FOV rows checks exact table transcription. CPU/GPU image
comparisons cover V1 dedicated/default and V2 recorded paths with repeated frame
reuse. Real-recording qualification remains limited to the supplied X5 V6
material.

Calibration validation also applies to caller-built and deserialized values:
polynomial models require a positive radius, and omnidirectional models require
`xi`. Missing model parameters fail before rendering so CPU and GPU cannot
interpret the same incomplete calibration differently. Regression tests remove
these fields from serialized valid calibrations and check render preflight.

For X5, the stitcher separates optical validity from seam selection. It uses the
lens-specific FOV and blend-angle tables, the Studio `calAlpha` exponent 5.2
curve, a calibrated radial source mask with a native distance-field feather, and
a two-band blend. High-frequency detail stays on the narrow calibrated seam;
low-frequency illumination uses all valid overlap. A two-pass,
longitude-smoothed per-channel gain estimator uses the recovered Studio
`ColorAdjustment::meanAdjustment` statistics, slope normalization, opposing
neutral regions, and zero clamp.

The gain estimator's sphere is aligned with the lens axes: its north pole is the
first lens's optical axis, and its south pole faces the second lens. Both
statistics and gain evaluation use this same basis. In the iOS 1.10.4
`INSCoreMedia` static trace, `ColorAdjustment::setSphereMap` builds a +90-degree
pitch map at `0x15b5ae4`–`0x15b5b18` and its -90-degree inverse at
`0x15b5b6c`–`0x15b5ba0`. `setEye2SphereMap` applies the first map at
`0x15c2ccc`–`0x15c2cdc`; `meanAdjustment` remaps the completed gain fields
through the inverse at `0x15b9350` and `0x15b936c`. The portable implementation
derives the corresponding basis from the first lens's recorded body-to-camera
orientation, preserving its own established lens order and supporting rotated
calibrations; this is an adaptation of the recovered coordinate operation, not a
claim of bit-identical vendor output.

Using body-up latitude for these ramps omitted that remap. With unequal lens
exposures it extrapolated a dimming correction deep into the visible lens and
could clamp an entire wedge to zero. CPU and GPU regressions use constant
22/220-DN images in both lens orders: every panorama ray remains visible and
both optical axes retain their source brightness. The correction changes gain
coordinates without changing projected image geometry or imposing a gain floor.

The asymmetric mask measures azimuth from down in each source fisheye. Standard
X5 housing uses lower-contour knots (0,90.5), (10,90.5), (20,91.8), (60,94)
degrees; Pro uses (0,91), (10,91), (32,92.5), (55,93.5). The shared prepared
mask rasterizes and erodes the lower contour, then computes the native L2 mask5
chamfer distance multiplied by 0.24390244483947754. CPU and GPU sample the same
field, exclude invalid bilinear taps and renormalize valid taps. Masked pixels
do not contribute to color statistics. These are source-space masks, not
rectangular panorama crops. See [housing correction](housings.md) for native
provenance, bounded cache ownership and sensor-window normalization.

Other registered cameras now pass camera/lens/setup validation and share the
same V1/V2/V3/V6 projection implementation when their recording carries those
formats. This is an implementation boundary, not a blanket qualification claim:
V1 sensor-crop conversion remains unsupported, and real-camera regression
corpora are still required for ONE X, X2, X3, X4, and X6 (including X6 10-bit
media).
