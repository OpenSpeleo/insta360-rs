# X5 attitude and sensor readout

The file-backed stabilization path combines camera-clock exposure mapping,
camera-specific IMU axes, gravity-referenced attitude, and independent readout
poses for the two source lenses. It does not claim to reproduce the proprietary
Insta360 FlowState filter. The existing relative gyro primitives remain useful
for callers that already have calibrated, correctly timed motion data.

## Coordinates and units

`MotionSample` angular velocity is radians/second. The supported X5 compact raw
record stores unsigned 16-bit axes centered at 32768. Acceleration is specific
force in multiples of standard gravity, and the recorded full-scale ranges must
be present and positive:

```text
a_raw[g]     = (encoded_accel - 32768) * accelerometer_range_g / 32768
w_raw[rad/s] = (encoded_gyro - 32768) * gyro_range_deg_s / 32768 * pi/180

a_gyrostab = (-a_raw.y, a_raw.z, -a_raw.x)
w_gyrostab = (-w_raw.y, w_raw.z, -w_raw.x)

gyrostab_to_projector = diag(-1, +1, -1)  # proper 180-degree rotation about Y
a_body = (a_raw.y, a_raw.z, a_raw.x)
w_body = (w_raw.y, w_raw.z, w_raw.x)
```

`X5MotionProfile` applies the complete raw-to-projector conversion and preserves
timestamps. The vendor implementation's `AlignAxes` output is an intermediate
stabilizer frame; it cannot be inserted directly into the calibrated projector.
Its missing half-turn reverses the signs of physical rotation about two camera
axes, so stabilization amplifies their motion. Both acceleration and angular
velocity must cross the same proper frame conversion. The body basis is
right-handed: forward X, left Y, up Z. The equirectangular renderer uses
`(cos(lat)*cos(lon), -cos(lat)*sin(lon), sin(lat))` in this basis. A stationary
upright sensor therefore supplies body specific force `(0,0,1)` from raw +X. Its
gravity vector points in the opposite direction.

The intermediate conversion comes from static analysis of the vendor
implementation. The additional projector mounting is independently established
by image motion in the supplied X5 recording. Neither a free fitted mounting
matrix nor a timing adjustment is used. The measured vectors and their source
timestamps are retained in `tests/fixtures/x5_optical_motion.json` so future
axis changes must agree with physical image observations as well as synthetic
quaternion tests.

The per-lens calibrated `r_c_b_` rotation is applied when projecting a body ray
into a fisheye. Applying its Euler fields to the IMU again would mix sensor
normalization with optical projection. No unit-specific transform from the
comparison script is embedded here. X5 optical setup conversion preserves these
extrinsics, and track reversal assigns decoded streams to calibrated lens slots;
neither operation changes the projector body basis.

## Attitude and targets

The lower-level `Stabilizer` is a relative gyro integrator with Y-up heading; it
does not consume gravity or infer X5 mounting. Its linearly interpolated angular
rates are integrated with bounded substeps, retaining rotations over 180 degrees
between observations, and it caps stored poses at five million. Unlike
`AttitudeTrack`, lookup outside its sample interval clamps to the nearest
endpoint. Pass body-axis, camera-clock samples through `AttitudeTrack` for the
Z-up file renderer. `Orientation::from_euler_degrees` applies fixed-axis X, then
Y, then Z rotations (`qz * qy * qx`).

`AttitudeTrack` consumes body-axis specific force and rates on a shared camera
clock, including available pre-roll. Its quaternion `Q(t)` maps body coordinates
into a world whose up direction is Z. Initial heading is defined as zero because
an accelerometer cannot determine a compass heading.

Initialization qualifies and averages a bounded interval of gravity
observations. Integration uses actual intervals, midpoint rates, and bounded
substeps of at most 10 milliseconds and 0.1 radians. Keeping intermediate poses
preserves angular winding even if consecutive source samples span a large
rotation. Gravity correction accepts measurements only when acceleration
magnitude and the difference from predicted up pass their gates. The default
magnitude gate is within 0.15 g of 1 g; the default angle gate is 20 degrees.
Rejected gravity observations leave gyro propagation active. Invalid timestamps,
excessive gaps, possible saturation, unqualified initial gravity, and allocation
limits fail explicitly. Pose lookup does not extrapolate outside telemetry
coverage.

Residual stationary gyro-bias estimation is optional and disabled by default. It
requires a strictly qualified still interval. Slow constant yaw and yaw bias are
indistinguishable using only gyro and accelerometer observations; a quiet signal
alone does not justify cancelling its yaw rate.

The target world orientation `T(t)` controls the output behavior:

| Mode            | Target                                                      |
| --------------- | ----------------------------------------------------------- |
| Off             | Raw attitude `Q(t)`                                         |
| Direction lock  | Level world orientation with fixed initial heading          |
| FlowState-style | Level world orientation retaining current unwrapped heading |

The last mode provides gravity leveling. It does not implement the vendor
implementation's proprietary heading planning, smoothing, motion classification,
or subject-aware behavior. Near a vertical body X axis, heading azimuth is
singular; the filter continues heading using the world-vertical gyro component.

## Camera timing and rolling shutter

The exposure clock, gyro clock, and decoded video PTS are distinct. In the
supported exposure-indexed recording, each decoded frame is matched with its
recorded exposure before timing corrections are applied. Mapping is validated
before reusing any decoder that ordinarily handles media-relative timestamps.
Pre-roll must be retained and all times rebased to the same nonnegative origin.

For a recorded camera exposure timestamp `E`, recorded gyro/video adjustment
`delta`, and shutter duration `S`, the vendor timing conversion establishes the
exposure center:

```text
t_center = E + delta - S/2
```

All terms above must first use the same unit. X5 raw timestamps use
microseconds; the recorded adjustment is milliseconds and the exposure duration
is seconds. Corrections must be applied once. The two lenses can have distinct
exposure observations and therefore distinct capture times.

X5 metadata tag 25 is a recorded-resolution readout duration in milliseconds,
converted to seconds by `X5MotionProfile::rolling_readout_seconds`. It is not a
guessed fraction of the nominal frame interval. The verified unrotated profile
scans top to bottom in each decoded fisheye. A source fraction `f` has time:

```text
t_capture(f) = t_center + readout_seconds * (f - 0.5)
f = (source_y + 0.5)/decoded_height
```

The fraction comes from the projected source pixel, not the output panorama row.
The automatic profile uses `[0,1]` over the recorded active image. This duration
scope is an implementation inference supported by two independent paths: the
vendor implementation calls the value resolution-specific and samples it across
normalized source texture v; the primary telemetry-parser X5 path forwards tag
25 unchanged and explicitly ignores source crop dimensions. Applying an
additional firmware `destination/source` ratio would shorten the sweep without
support from either path. Physical scan timing has not been independently
measured, and this is not a claim of exact vendor parity.

The profile still validates the spatial crop metadata. Crop offsets are relative
to the center of the source sensor:

```text
crop_start = (source_height - destination_height)/2 + y_offset
crop_end = crop_start + destination_height
```

The profile validates both crop axes and permits signed offsets only when the
complete crop remains inside the sensor. It requires explicit zero image
rotation and known crop metadata. Missing, malformed, unsupported, and out-of-
bounds transformations fail; the profile does not guess rotation or clamp a bad
crop into the sensor. Pure image scaling preserves normalized readout fractions.
Additional image rotations or crops applied after capture require an explicit
readout transform.

`ReadoutPoseTable` describes a general source sensor, with four scan directions,
an increasing crop interval within the supplied readout domain, and bounded
uniformly spaced poses. For example, a caller removing the top and bottom 10
percent of an already captured image supplies `[0.1,0.9]`; this is separate from
the firmware crop already represented by the recorded-resolution duration.
Entries are rotations `Q(t_capture).inverse() * Q(t_reference)`. The global
sampling rotation is `Q(t_reference).inverse() * T(t_frame)`, so the combined
source lookup is `Q(t_capture).inverse() * T(t_frame)`. A lens must choose its
capture time from its own projected source position. The table resolves angular
winding before shortest-arc interpolation and supports the same representation
on CPU and GPU.

## Recorded factory calibration

The observed tag-31 `gyro_calib` payload has six little-endian `f64` values
followed by a little-endian `u64` timestamp, totaling 56 bytes.
`RecordedImuCalibration` retains that shape and checks finite values and
conflicting declarations. It does not assign an undocumented bias order, unit,
or subtraction sign. Unsupported layouts fail explicitly.

For the supplied X5 recording the six values are approximately
`[-0.003438465, 0.002705575, 0.013432439, -0.002162954, -0.013254938, 0.001173755]`,
and the final timestamp is zero. Neither the public vendor header nor the
inspected primary telemetry parser establishes enough semantics to subtract
these values. Optional stationary residual estimation remains separate.

## Evidence and verification

The static vendor evidence below refers to the arm64 iOS 1.10.4 `INSCoreMedia`
framework. Its SHA-256 is
`3b905b46e46053d9c426c4af8bb28e449666d3ddf1c03419ab564ed2b0a01409`. These
addresses were read with `nm` and `llvm-objdump`; no vendor binary was run.

| Fact                                                                                    | Static source                                                                                                                                                                                     |
| --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| X5 internal family A3 has Objective-C gyro type 33                                      | `Headers/INSExtraGyroData.h`; camera registry identifies X5/A3                                                                                                                                    |
| A3 maps to stabilizer gyro type 145                                                     | `+[INSGyroPlayer(stabilizer) dataTypeToStabilizerGyroType:]` at `0x1f3220`, table at `0x50720ec`                                                                                                  |
| X5 lens IDs 113, 115, 117, 118 select that gyro family for dual-lens use                | `bac::BaseUtil::GetLensConfig` at `0x172c410`, X5 branch `0x172c5e8`                                                                                                                              |
| Raw rate becomes intermediate gyrostab `(-y,z,-x)`; vendor gravity becomes `(y,-z,x)`   | `ins::AlignAxes` gyro type 145 branch at `0x19fc288`                                                                                                                                              |
| Gyrostab coordinates are Z-up; the vendor implementation's GL conversion is `(-y,z,-x)` | `ins::FromHorizonalRotation` uses down `(0,0,-1)`; `bac::BaseUtil::QuatFromGyrostablib` at `0x172dcb8` conjugates by `(w,x,y,z)=(.5,-.5,.5,.5)`; inverse constants at `0x5158640`/`0x5158650`     |
| Raw acceleration uses g and has no implicit sign flip during scaling                    | `StardardWithRawAcc` at `0x657dd4`                                                                                                                                                                |
| Vertical sweep is the default                                                           | `Headers/INSRender.h:232`; `INSSphericalPanoObject` constructor writes true at `0x6dee8..0x6def0`, matching the setter ivar at `0x70b94`                                                          |
| Vertical sweep uses projected source v for each lens                                    | `INSSphereModel` at `0x64b35c` and `0x64b5ac`; `calLensTexture:point:u:v:` normalizes native y by source height at `0x64d2c0..0x64d308`                                                           |
| Readout is centered on frame time and increases with source v                           | `INSSphericalPanoObject::applyGyroStabilization:` at `0x6f27c..0x6f284` and `0x6f2d0..0x6f2e8`                                                                                                    |
| Crop origin is centered plus recorded offset                                            | `INSOffsetCalculator::cropOffset` negates metadata offsets at `0x3cce1c`; `OffsetConvert::convertOffset` forms half the dimension difference minus that supplied offset at `0x1e38f54..0x1e38f70` |
| Exposure center subtracts half shutter duration after the clock adjustment              | `arvrender::GyroTimeConverter::StaticAddDeltaTime` at `0x43fa9b0`                                                                                                                                 |
| Tag-25 sweep duration is milliseconds                                                   | Vendor sample `CameraConfigByJsonController.swift` carries `rollingShutterTime` through `rollShuuterTimeMs` to sweep time; `Headers/INSGyroPlayer.h:32` also specifies milliseconds               |

Independent primary sources corroborate the data units and format:
[telemetry-parser's INSV reader](https://github.com/AdrianEddy/telemetry-parser/blob/master/src/insta360/record.rs)
labels acceleration as g and exposes tag 25 as frame readout time;
[its metadata parser](https://github.com/AdrianEddy/telemetry-parser/blob/master/src/insta360/extra_info.rs)
decodes the six unnamed calibration values and final timestamp.
[Gyroflow's frame transform](https://github.com/gyroflow/gyroflow/blob/master/src/core/stabilization/frame_transform.rs)
uses source coordinates and a centered time interval. Its generic sensor crop
scaling depends on additional per-frame lens parameters that the Insta360 parser
does not emit; it does not apply the X5 firmware crop ratio again. Its
normalized IMU API changes units and coordinate conventions; its output must not
be mixed directly with this crate's raw decoded samples.

The supplied recording reports 32 g and 2000 degrees/second IMU ranges,
11.836874961853027 milliseconds of readout, zero file rotation, and a centered
5376-square to 5312-square crop. That crop has physical origin 32 pixels, while
the recorded readout duration spans the decoded active image with interval
`[0,1]`. Its earliest gyro sample precedes the first video timestamp by about
0.779 seconds, demonstrating why pre-roll matters.

Focused tests cover all three axis basis vectors, angular integration
covariance, specific-force sign, unit preservation, malformed and opaque factory
calibration, centered and shifted crop coordinates, source pixel centers,
readout units, and explicit rejection of unestablished profiles. Fusion and
renderer tests additionally exercise tilted starts, rotating trajectories,
acceleration disturbances, heading wrap, telemetry gaps, and CPU/GPU source
readout agreement.

This implementation corrects orientation. It does not reconstruct translation,
depth, six-degree-of-freedom camera motion, exposure blur, or lens parallax.
Gravity cannot prevent heading drift or reliably distinguish every sustained
linear acceleration from tilt. Validation on a single real X5 recording does not
establish behavior for every firmware version, capture mode, housing, or
transformed image pipeline.
