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

One reviewed registry identifies ONE X, ONE X2, X3, X4, X5/A3, and X6/C9 aliases
and maps their evidence-backed lens IDs to optical setups, full FOV, blend
angle, accepted projection generations, and optional source-mask recipes. Every
value retains header or static-binary provenance. Shared lens identifiers remain
camera-scoped; when a camera name is absent, a value is accepted only when all
matching registry records agree.

The registry is not a calibration database. The recording's current/original
offset remains the sole source of per-unit intrinsics, distortion, principal
points, and extrinsics. `ResolvedCalibration` stores the selected camera family
and `ResolvedLensGeometry` so CPU and GPU consume identical angles without a
per-pixel camera-ID switch. A valid positive `blendAngle` metadata value from
tag 128 overrides the registry fallback.

A recorded blend angle of exactly 180 degrees represents zero angular overlap.
CPU and GPU render it as the same finite hard seam, with equal ownership only on
the seam plane, instead of dividing by the zero-width overlap belt.

The parser also retains crop, rotation, file category, stream layout/order,
codec, capture-offset version, offset/accessory state, guard detection, timing,
and unknown protobuf fields. These values inform dispatch and diagnostics; they
do not silently alter recorded calibration.

## Current versus original

`CalibrationResolver::resolve_metadata` requires an explicit `OffsetSource`. It
never falls back between current and original offsets. The distinction is
semantic: an original offset is the factory copy, while a current offset may
already have been converted for a lens accessory. Falling back could therefore
silently use the wrong refraction model.

The resolver maps an already-converted X5 lens type directly and can convert a
V6 `OmniRadtanPro` offset when both the source and target six-coefficient
profiles are embedded in the recording:

| X5 lens type               | Accepted setup                |
| -------------------------- | ----------------------------- |
| 113 (`A3`)                 | `BareAir`                     |
| 114 (`A3_BARE_UNDERWATER`) | `BareUnderwater`              |
| 117 (`A3DivingWater`)      | `InvisibleDiveCaseUnderwater` |
| 118 (`A3DivingAir`)        | `InvisibleDiveCaseAir`        |

Unknown and protective-shell lens types are not guessed. The public X5 header
names 113, 115, 117, and 118; the `A3_BARE_UNDERWATER` 114 name comes from the
shipped `INSCoreMedia` debug information. Portable conversion currently covers
the evidence-backed V6 bare, dive-water, and dive-air targets. Other source or
target generations fail explicitly.

## Embedded profile descriptors

The supplied sample's named profile submessages have one of two protobuf shapes:

- field 1 string plus six repeated field 2 fixed64 doubles; or
- field 1 string plus one field 2 varint classifier value.

`ParsedEmbeddedProfile` validates and preserves both shapes. Static tracing of
`OffsetConvert::getPhysical2PixelScale`,
`OffsetConvert::getV6DistortAndFocalFromLens`, and
`OffsetConvert::converOffsetNormal` establishes that the six doubles are an
angle-to-physical-radius polynomial evaluated in degrees.

For V6 conversion, the resolver first fits the recorded Omni model to the source
physical curve to recover pixels per physical-radius unit. It then fits the
target curve against `[u, u^3, u^5, u^7, u^9]`, where
`u = sin(theta) / (cos(theta) + xi)`. It replaces `fx`, `fy`, and radial slots 0
through 4, while preserving `xi`, principal points, measured extrinsics, and
tangential/thin-prism slots 5 through 12. The regenerated offset carries the
target lens type and validates like a native V6 offset.

## Supplied sample

Both current and original offsets in the supplied X5 recording are lens type
113, but metadata tag 68 explicitly records state 10: A3/X5 Dive Case Pro
underwater. `StrictAuto` therefore uses the recording's `bare` and
`InvisibleDiveWater` physical curves to convert the V6 calibration to lens
type 117. Merely carrying an optional profile is not evidence that the accessory
was installed; the explicit recorded state is. A caller can still select
`BareAir` as a deliberate override.

The CPU stitcher implements the native V6 projection from the Metal source
embedded in the licensed Studio worker library. It uses the unified omni
normalization `x,y / (z + xi * norm)`, followed by five radial terms, two
radius-varying tangential pairs, and four thin-prism terms. It also uses the
vendor's equirectangular sphere convention, the parser's `+pi/2` second-Euler
basis conversion, the physical half-turn for the second X5 lens, and the X5
field-of-view values recovered from `Insta360Lens::GetFov`.

V1 remains parse-only because it does not carry enough polynomial data to
recreate the normalized renderer parameters by inspection. CPU and GPU preflight
reject it with a typed calibration error before allocating output. V2 and V3
have model-specific projection paths, but only the supplied X5 V6 path has been
checked against real recording metadata in this repository.

Calibration validation also applies to caller-built and deserialized values:
polynomial models require a positive radius, and omnidirectional models require
`xi`. Missing model parameters fail before rendering so CPU and GPU cannot
interpret the same incomplete calibration differently. Regression tests remove
these fields from serialized valid calibrations and check render preflight.

For X5, the stitcher separates optical validity from seam selection. It uses the
lens-specific FOV and blend-angle tables, the Studio `calAlpha` exponent 5.2
curve, a source-pixel dive-case mask with a four-pixel feather, and a two-band
blend. High-frequency detail stays on the narrow calibrated seam; low-frequency
illumination uses all valid overlap. A two-pass, longitude-smoothed per-channel
gain estimator uses the recovered Studio `ColorAdjustment::meanAdjustment`
statistics, slope normalization, opposing neutral regions, and zero clamp.

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

The asymmetric dive-case mask measures azimuth from **down in the decoded source
image**: its 90.5-degree boundary faces down, widening to 94 degrees toward the
sides. The statically inspected vendor routine rotates each source mask
counterclockwise before eroding its right half and rotates it back afterward.
Undoing that temporary rotation gives `atan2(abs(dx), abs(dy))`; exchanging
those arguments admits housing below the lens while discarding valid side
overlap. This direction is confirmed by `calcFisheyeMaskONEX5AndProtector` at
`0x16d64cc`/`0x16d65c4` and `calcFisheyeMaskErodeCircleMethord3` at
`0x16e97b4..0x16e97d4` in the registry's iOS 1.10.4 binary. CPU and GPU
regressions place differing pixels exclusively in that lower housing region and
require an unchanged, fully covered panorama.

Other registered cameras now pass camera/lens/setup validation and share the
same V2/V3/V6 projection implementation when their recording carries those
formats. This is an implementation boundary, not a blanket qualification claim:
V1 remains parse-only, accessory conversion is currently X5 V6-specific, and
real-camera regression corpora are still required for X1, X2, X3, X4, and X6
(including X6 10-bit media).
