//! Quaternion motion primitives and deterministic gyro integration.

mod fusion;
pub mod profile;
pub(crate) mod readout;

pub use fusion::{AttitudeTrack, FusionDiagnostics, FusionOptions};
pub use readout::{FrameMotion, ReadoutDirection, ReadoutPoseTable};

use std::f64::consts::PI;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::container::{ImuRange, InsvMetadata, VideoPtsMapType};
use crate::types::Stabilization;
use crate::{Error, Result};

const NORM_EPSILON: f64 = 1.0e-12;
const RAW_SAMPLE_SIZE: usize = 20;
const COMMON_SAMPLE_SIZE: usize = 56;
const MAX_MOTION_SAMPLES: usize = 5_000_000;
const DEFAULT_ACCELEROMETER_RANGE_G: f64 = 8.0;
const DEFAULT_GYROSCOPE_RANGE_DPS: f64 = 2_000.0;

/// A unit quaternion stored as `(w, x, y, z)`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Orientation {
    pub w: f64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Orientation {
    /// Identity rotation.
    pub const IDENTITY: Self = Self {
        w: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    /// Creates and normalizes a quaternion.
    pub fn new(w: f64, x: f64, y: f64, z: f64) -> Result<Self> {
        Self { w, x, y, z }.normalized()
    }

    /// Verifies that this quaternion is finite and normalized.
    pub fn validate(self) -> Result<Self> {
        let values = [self.w, self.x, self.y, self.z];
        if !values.iter().all(|value| value.is_finite()) {
            return Err(Error::InvalidMedia(
                "orientation contains a non-finite value".into(),
            ));
        }
        let norm = self.norm();
        if (norm - 1.0).abs() > 1.0e-6 {
            return Err(Error::InvalidMedia(format!(
                "orientation is not normalized (norm {norm})"
            )));
        }
        Ok(self)
    }

    /// Creates a rotation from a finite axis and angle in radians.
    pub fn from_axis_angle(axis: [f64; 3], angle_radians: f64) -> Result<Self> {
        if !angle_radians.is_finite() || !axis.iter().all(|value| value.is_finite()) {
            return Err(Error::InvalidMedia(
                "axis-angle rotation contains a non-finite value".into(),
            ));
        }
        let length = vector_length(axis);
        if length <= NORM_EPSILON {
            if angle_radians.abs() <= NORM_EPSILON {
                return Ok(Self::IDENTITY);
            }
            return Err(Error::InvalidMedia(
                "a non-zero rotation requires a non-zero axis".into(),
            ));
        }
        let (axis, length) = if length.is_finite() {
            (axis, length)
        } else {
            let scale = axis.into_iter().map(f64::abs).fold(0.0, f64::max);
            let scaled = axis.map(|value| value / scale);
            (scaled, vector_length(scaled))
        };
        let half = angle_radians * 0.5;
        let scale = half.sin() / length;
        Self::new(
            half.cos(),
            axis[0] * scale,
            axis[1] * scale,
            axis[2] * scale,
        )
    }

    /// Applies rotations around fixed X, then Y, then Z axes, in degrees.
    pub fn from_euler_degrees(x: f64, y: f64, z: f64) -> Result<Self> {
        if ![x, y, z].iter().all(|value| value.is_finite()) {
            return Err(Error::InvalidMedia(
                "Euler rotation contains a non-finite value".into(),
            ));
        }
        let qx = Self::from_axis_angle([1.0, 0.0, 0.0], x.to_radians())?;
        let qy = Self::from_axis_angle([0.0, 1.0, 0.0], y.to_radians())?;
        let qz = Self::from_axis_angle([0.0, 0.0, 1.0], z.to_radians())?;
        Ok(qz * qy * qx)
    }

    /// Returns the inverse unit rotation.
    pub fn inverse(self) -> Self {
        Self {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    /// Applies this orientation to a three-dimensional vector.
    pub fn rotate_vector(self, vector: [f64; 3]) -> [f64; 3] {
        let qv = [self.x, self.y, self.z];
        let first = cross(qv, vector);
        let second = cross(qv, first);
        [
            vector[0] + 2.0 * (self.w * first[0] + second[0]),
            vector[1] + 2.0 * (self.w * first[1] + second[1]),
            vector[2] + 2.0 * (self.w * first[2] + second[2]),
        ]
    }

    /// Spherically interpolates between two unit orientations.
    pub fn slerp(self, mut other: Self, amount: f64) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let mut dot = self.w * other.w + self.x * other.x + self.y * other.y + self.z * other.z;
        if dot < 0.0 {
            other = Self {
                w: -other.w,
                x: -other.x,
                y: -other.y,
                z: -other.z,
            };
            dot = -dot;
        }
        if dot > 0.9995 {
            return Self {
                w: self.w + amount * (other.w - self.w),
                x: self.x + amount * (other.x - self.x),
                y: self.y + amount * (other.y - self.y),
                z: self.z + amount * (other.z - self.z),
            }
            .normalized_or_identity();
        }

        let angle = dot.clamp(-1.0, 1.0).acos();
        let denominator = angle.sin();
        let left = ((1.0 - amount) * angle).sin() / denominator;
        let right = (amount * angle).sin() / denominator;
        Self {
            w: left * self.w + right * other.w,
            x: left * self.x + right * other.x,
            y: left * self.y + right * other.y,
            z: left * self.z + right * other.z,
        }
        .normalized_or_identity()
    }

    /// Returns heading around the positive Y axis in radians.
    pub fn yaw_radians(self) -> f64 {
        let forward = self.rotate_vector([0.0, 0.0, 1.0]);
        forward[0].atan2(forward[2])
    }

    fn norm(self) -> f64 {
        (self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    fn normalized(self) -> Result<Self> {
        if ![self.w, self.x, self.y, self.z]
            .iter()
            .all(|value| value.is_finite())
        {
            return Err(Error::InvalidMedia(
                "orientation contains a non-finite value".into(),
            ));
        }
        let norm = self.norm();
        if norm <= NORM_EPSILON {
            return Err(Error::InvalidMedia(
                "orientation quaternion has zero length".into(),
            ));
        }
        if norm.is_finite() {
            // Keep the frequent interpolation/integration path to one sqrt.
            return Ok(Self {
                w: self.w / norm,
                x: self.x / norm,
                y: self.y / norm,
                z: self.z / norm,
            });
        }
        // Finite components can have a norm larger than f64::MAX; dividing by
        // infinity would yield a zero pose. Rescale only this exceptional path.
        let scale = [self.w, self.x, self.y, self.z]
            .into_iter()
            .map(f64::abs)
            .fold(0.0, f64::max);
        let scaled = Self {
            w: self.w / scale,
            x: self.x / scale,
            y: self.y / scale,
            z: self.z / scale,
        };
        let norm = scaled.norm();
        Ok(Self {
            w: scaled.w / norm,
            x: scaled.x / norm,
            y: scaled.y / norm,
            z: scaled.z / norm,
        })
    }

    fn normalized_or_identity(self) -> Self {
        self.normalized().unwrap_or(Self::IDENTITY)
    }
}

impl std::ops::Mul for Orientation {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self::Output {
        Self {
            w: self.w * rhs.w - self.x * rhs.x - self.y * rhs.y - self.z * rhs.z,
            x: self.w * rhs.x + self.x * rhs.w + self.y * rhs.z - self.z * rhs.y,
            y: self.w * rhs.y - self.x * rhs.z + self.y * rhs.w + self.z * rhs.x,
            z: self.w * rhs.z + self.x * rhs.y - self.y * rhs.x + self.z * rhs.w,
        }
        .normalized_or_identity()
    }
}

/// One calibrated IMU observation. Angular velocity is radians per second.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MotionSample {
    pub timestamp: Duration,
    pub acceleration: [f64; 3],
    pub angular_velocity: [f64; 3],
}

/// Decodes INSV record `3` into media-relative motion samples.
///
/// Compact raw samples use the firmware's recorded IMU ranges and retain their
/// microsecond timestamps. Legacy samples contain an `i64` millisecond
/// timestamp followed by six little-endian `f64` values. Samples before the
/// first video frame are intentionally discarded.
pub fn decode_motion_record(data: &[u8], metadata: &InsvMetadata) -> Result<Vec<MotionSample>> {
    match metadata.video_pts_map_type {
        Some(VideoPtsMapType::ReadingInExposureFile) => {
            return Err(Error::MissingCapability(
                "gyro/video alignment requires exposure-file PTS mapping, which is not implemented"
                    .into(),
            ));
        }
        Some(VideoPtsMapType::Other(value)) => {
            return Err(Error::MissingCapability(format!(
                "gyro/video alignment uses unsupported PTS mapping type {value}"
            )));
        }
        None | Some(VideoPtsMapType::Unknown | VideoPtsMapType::DecoderWithFirstFrameTimestamp) => {
        }
    }
    let raw = metadata.is_raw_gyro.ok_or_else(|| {
        Error::InvalidMedia("metadata does not declare the gyro record format".into())
    })?;
    let sample_size = if raw {
        RAW_SAMPLE_SIZE
    } else {
        COMMON_SAMPLE_SIZE
    };
    if !data.len().is_multiple_of(sample_size) {
        return Err(Error::InvalidMedia(format!(
            "gyro record size {} is not divisible by its {sample_size}-byte sample size",
            data.len()
        )));
    }
    let count = data.len() / sample_size;
    if count > MAX_MOTION_SAMPLES {
        return Err(Error::InvalidMedia(format!(
            "gyro record contains {count} samples, exceeding the {MAX_MOTION_SAMPLES} sample limit"
        )));
    }
    if count == 0 {
        return Ok(Vec::new());
    }

    let first_timestamp = metadata.first_frame_timestamp.unwrap_or_else(|| {
        if raw {
            i64::from_le_bytes(data[..8].try_into().expect("raw timestamp is eight bytes"))
        } else {
            i64::from_le_bytes(data[..8].try_into().expect("gyro timestamp is eight bytes"))
        }
    });
    let timestamp_multiplier = if raw { 1_i128 } else { 1_000_i128 };
    let first_timestamp_us = i128::from(first_timestamp) * timestamp_multiplier;
    // A positive firmware adjustment means the IMU measurement lags the video,
    // so move the measurement earlier on the media timeline.
    let adjustment_us = metadata
        .gyro_timestamp_adjust_ms
        .unwrap_or(0.0)
        .mul_add(1_000.0, 0.0)
        .round();
    if !adjustment_us.is_finite()
        || adjustment_us < i64::MIN as f64
        || adjustment_us > i64::MAX as f64
    {
        return Err(Error::InvalidMedia(
            "gyro timestamp adjustment is outside the supported range".into(),
        ));
    }
    let adjustment_us = adjustment_us as i128;
    let range = metadata.imu_range.unwrap_or(ImuRange {
        accelerometer_g: DEFAULT_ACCELEROMETER_RANGE_G,
        gyroscope_degrees_per_second: DEFAULT_GYROSCOPE_RANGE_DPS,
    });
    validate_imu_range(range)?;

    let mut samples = Vec::with_capacity(count);
    for chunk in data.chunks_exact(sample_size) {
        let camera_timestamp = i64::from_le_bytes(
            chunk[..8]
                .try_into()
                .expect("gyro timestamp is eight bytes"),
        );
        let media_timestamp_us = i128::from(camera_timestamp) * timestamp_multiplier
            - first_timestamp_us
            - adjustment_us;
        if media_timestamp_us < 0 {
            continue;
        }
        let media_timestamp_us = u64::try_from(media_timestamp_us).map_err(|_| {
            Error::InvalidMedia("media-relative gyro timestamp overflows Duration".into())
        })?;
        let (acceleration, angular_velocity) = if raw {
            decode_raw_axes(chunk, range)
        } else {
            decode_common_axes(chunk)?
        };
        samples.push(MotionSample::new(
            Duration::from_micros(media_timestamp_us),
            acceleration,
            angular_velocity,
        )?);
    }
    for pair in samples.windows(2) {
        if pair[1].timestamp <= pair[0].timestamp {
            return Err(Error::InvalidMedia(
                "gyro record timestamps must be strictly increasing".into(),
            ));
        }
    }
    Ok(samples)
}

fn validate_imu_range(range: ImuRange) -> Result<()> {
    if !range.accelerometer_g.is_finite()
        || range.accelerometer_g <= 0.0
        || !range.gyroscope_degrees_per_second.is_finite()
        || range.gyroscope_degrees_per_second <= 0.0
    {
        return Err(Error::InvalidMedia(
            "IMU full-scale ranges must be finite and positive".into(),
        ));
    }
    Ok(())
}

fn decode_raw_axes(chunk: &[u8], range: ImuRange) -> ([f64; 3], [f64; 3]) {
    let decode = |offset: usize| {
        let encoded = u16::from_le_bytes([chunk[offset], chunk[offset + 1]]);
        i32::from(encoded) - 32_768
    };
    let acceleration_scale = range.accelerometer_g / 32_768.0;
    let gyroscope_scale = range.gyroscope_degrees_per_second / 32_768.0 * PI / 180.0;
    (
        [
            f64::from(decode(8)) * acceleration_scale,
            f64::from(decode(10)) * acceleration_scale,
            f64::from(decode(12)) * acceleration_scale,
        ],
        [
            f64::from(decode(14)) * gyroscope_scale,
            f64::from(decode(16)) * gyroscope_scale,
            f64::from(decode(18)) * gyroscope_scale,
        ],
    )
}

fn decode_common_axes(chunk: &[u8]) -> Result<([f64; 3], [f64; 3])> {
    let value = |index: usize| {
        let offset = 8 + index * 8;
        f64::from_le_bytes(
            chunk[offset..offset + 8]
                .try_into()
                .expect("common gyro value is eight bytes"),
        )
    };
    let values = [value(0), value(1), value(2), value(3), value(4), value(5)];
    if !values.iter().all(|value| value.is_finite()) {
        return Err(Error::InvalidMedia(
            "gyro record contains a non-finite value".into(),
        ));
    }
    Ok((
        [values[0], values[1], values[2]],
        [values[3], values[4], values[5]],
    ))
}

impl MotionSample {
    /// Creates a motion sample after checking acceleration and angular velocity.
    pub fn new(
        timestamp: Duration,
        acceleration: [f64; 3],
        angular_velocity: [f64; 3],
    ) -> Result<Self> {
        if !acceleration
            .iter()
            .chain(angular_velocity.iter())
            .all(|value| value.is_finite())
        {
            return Err(Error::InvalidMedia(
                "motion sample contains a non-finite value".into(),
            ));
        }
        Ok(Self {
            timestamp,
            acceleration,
            angular_velocity,
        })
    }
}

/// Integrated relative camera orientations with a selected stabilization mode.
#[derive(Clone, Debug)]
pub struct Stabilizer {
    mode: Stabilization,
    orientations: Vec<(Duration, Orientation)>,
    direction_anchor: Orientation,
}

impl Stabilizer {
    /// Integrates samples relative to the identity orientation.
    ///
    /// Bounded substeps preserve rotations exceeding half a turn between input
    /// samples. At most five million poses are retained; larger tracks fail.
    pub fn new(samples: &[MotionSample], mode: Stabilization) -> Result<Self> {
        Self::with_initial_orientation(samples, mode, Orientation::IDENTITY)
    }

    /// Integrates samples relative to a supplied initial camera orientation.
    pub fn with_initial_orientation(
        samples: &[MotionSample],
        mode: Stabilization,
        initial: Orientation,
    ) -> Result<Self> {
        let initial = initial.validate()?;
        if samples.is_empty() {
            return Err(Error::InvalidMedia(
                "stabilization requires at least one motion sample".into(),
            ));
        }
        for sample in samples {
            MotionSample::new(
                sample.timestamp,
                sample.acceleration,
                sample.angular_velocity,
            )?;
        }
        for pair in samples.windows(2) {
            if pair[1].timestamp <= pair[0].timestamp {
                return Err(Error::InvalidMedia(
                    "motion timestamps must be strictly increasing".into(),
                ));
            }
        }

        let pose_count = samples.windows(2).try_fold(1usize, |count, pair| {
            count
                .checked_add(relative_substep_count(pair)?)
                .filter(|count| *count <= MAX_MOTION_SAMPLES)
                .ok_or_else(|| {
                    Error::InvalidMedia("relative gyro integration exceeds the pose limit".into())
                })
        })?;
        let mut orientations = Vec::new();
        orientations.try_reserve_exact(pose_count).map_err(|_| {
            Error::InvalidMedia("cannot allocate the bounded relative gyro track".into())
        })?;
        let mut current = initial;
        orientations.push((samples[0].timestamp, current));
        for pair in samples.windows(2) {
            let interval = pair[1].timestamp - pair[0].timestamp;
            let steps = relative_substep_count(pair)?;
            let mut previous = pair[0].timestamp;
            for step in 1..=steps {
                let nanos = interval.as_nanos() * step as u128 / steps as u128;
                let time = pair[0].timestamp
                    + Duration::new(
                        (nanos / 1_000_000_000) as u64,
                        (nanos % 1_000_000_000) as u32,
                    );
                let seconds = (time - previous).as_secs_f64();
                let fraction = ((previous - pair[0].timestamp).as_secs_f64() + seconds * 0.5)
                    / interval.as_secs_f64();
                let angular_velocity = std::array::from_fn(|axis| {
                    pair[0].angular_velocity[axis] * (1.0 - fraction)
                        + pair[1].angular_velocity[axis] * fraction
                });
                let speed = vector_length(angular_velocity);
                if speed > NORM_EPSILON {
                    let delta = Orientation::from_axis_angle(angular_velocity, speed * seconds)?;
                    current = current * delta;
                }
                orientations.push((time, current));
                previous = time;
            }
        }

        Ok(Self {
            mode,
            orientations,
            direction_anchor: initial,
        })
    }

    /// Returns the selected stabilization mode.
    pub fn mode(&self) -> Stabilization {
        self.mode
    }

    /// Interpolates the integrated camera orientation at a media timestamp.
    pub fn orientation_at(&self, timestamp: Duration) -> Orientation {
        let insertion = self
            .orientations
            .partition_point(|(sample_timestamp, _)| *sample_timestamp <= timestamp);
        if insertion == 0 {
            return self.orientations[0].1;
        }
        if insertion == self.orientations.len() {
            return self.orientations[self.orientations.len() - 1].1;
        }
        let (left_time, left) = self.orientations[insertion - 1];
        let (right_time, right) = self.orientations[insertion];
        let interval = (right_time - left_time).as_secs_f64();
        let elapsed = (timestamp - left_time).as_secs_f64();
        left.slerp(right, elapsed / interval)
    }

    /// Rotation to apply to camera-space rays for the configured stabilization.
    pub fn correction_at(&self, timestamp: Duration) -> Orientation {
        let camera = self.orientation_at(timestamp);
        match self.mode {
            Stabilization::Off => Orientation::IDENTITY,
            Stabilization::DirectionLock => self.direction_anchor * camera.inverse(),
            Stabilization::FlowState => {
                let yaw = Orientation::from_axis_angle([0.0, 1.0, 0.0], camera.yaw_radians())
                    .unwrap_or(Orientation::IDENTITY);
                yaw * camera.inverse()
            }
        }
    }
}

fn relative_substep_count(pair: &[MotionSample]) -> Result<usize> {
    let interval = pair[1].timestamp - pair[0].timestamp;
    let maximum_speed =
        vector_length(pair[0].angular_velocity).max(vector_length(pair[1].angular_velocity));
    let steps = (maximum_speed * interval.as_secs_f64() / std::f64::consts::FRAC_PI_2)
        .ceil()
        .max(1.0);
    if !steps.is_finite()
        || steps > MAX_MOTION_SAMPLES as f64
        || steps as u128 > interval.as_nanos()
    {
        return Err(Error::InvalidMedia(
            "relative gyro interval exceeds bounded integration limits".into(),
        ));
    }
    Ok(steps as usize)
}

fn vector_length(vector: [f64; 3]) -> f64 {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length.is_finite() {
        length
    } else {
        vector[0].hypot(vector[1]).hypot(vector[2])
    }
}

fn cross(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}
