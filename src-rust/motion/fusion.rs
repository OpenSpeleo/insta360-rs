//! Gravity-referenced attitude, separate from the legacy relative gyro API.
//!
//! The quaternion maps calibrated body coordinates into a right-handed world
//! whose up direction is +Z. Acceleration is specific force in g: a stationary
//! upright sensor reports `[0, 0, 1]`. Gravity does not observe compass heading.

use std::f64::consts::{PI, TAU};
use std::time::Duration;

use super::{cross, vector_length, MotionSample, Orientation};
use crate::{Error, Result, Stabilization};

const UP: [f64; 3] = [0.0, 0.0, 1.0];
const MAX_POSES: usize = 5_000_000;
const MAX_STEP_SECONDS: f64 = 0.01;
const MAX_STEP_RADIANS: f64 = 0.1;
const INITIAL_DIRECTION_TOLERANCE: f64 = 0.08726646259971647; // Five degrees.
const STATIONARY_WINDOW: Duration = Duration::from_secs(1);
const STATIONARY_MAX_SPEED: f64 = 0.003490658503988659; // 0.2 degrees/second.
const STATIONARY_GYRO_STDDEV: f64 = 0.00015;
const STATIONARY_ACCEL_STDDEV: f64 = 0.005;

/// Conservative controls for calibrated gyro/accelerometer fusion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FusionOptions {
    /// Proportional gravity correction rate in inverse seconds, from 0 to 10.
    pub gravity_gain: f64,
    /// Maximum accepted departure of acceleration magnitude from 1 g.
    pub acceleration_tolerance_g: f64,
    /// Maximum angle between measured and gyro-predicted up, in radians.
    pub max_acceleration_innovation_radians: f64,
    /// Bounded initial interval used to qualify and average gravity.
    pub initialization_window: Duration,
    /// Minimum time spanned by qualified initial gravity observations.
    pub minimum_initialization_duration: Duration,
    /// A larger consecutive telemetry gap makes the track invalid.
    pub max_sample_gap: Duration,
    /// Conservative upper bound on calibrated angular-speed magnitude.
    ///
    /// Measurements at or above this limit are treated as possible saturation.
    /// Set this from the sensor's supported range, after accounting for axes.
    pub max_angular_speed_rad_s: f64,
    /// Enables a residual gyro bias estimate during strictly qualified stillness.
    ///
    /// Disabled by default: an arbitrarily slow constant yaw is indistinguishable
    /// from yaw bias using an accelerometer and gyro alone. Enable only when the
    /// recording contains known stationary periods. Factory bias belongs in the
    /// calibration applied before this API, independently of this option.
    pub estimate_stationary_bias: bool,
    /// Maximum stored poses, including bounded integration substeps.
    pub max_pose_count: usize,
}

impl Default for FusionOptions {
    fn default() -> Self {
        Self {
            gravity_gain: 0.5,
            acceleration_tolerance_g: 0.15,
            max_acceleration_innovation_radians: 20.0_f64.to_radians(),
            initialization_window: Duration::from_millis(250),
            minimum_initialization_duration: Duration::from_millis(20),
            max_sample_gap: Duration::from_millis(100),
            max_angular_speed_rad_s: 2_000.0_f64.to_radians(),
            estimate_stationary_bias: false,
            max_pose_count: MAX_POSES,
        }
    }
}

impl FusionOptions {
    fn validate(self) -> Result<Self> {
        if !self.gravity_gain.is_finite()
            || !(0.0..=10.0).contains(&self.gravity_gain)
            || !self.acceleration_tolerance_g.is_finite()
            || !(0.0..0.5).contains(&self.acceleration_tolerance_g)
            || !self.max_acceleration_innovation_radians.is_finite()
            || !(0.0..=PI / 3.0).contains(&self.max_acceleration_innovation_radians)
            || self.max_acceleration_innovation_radians == 0.0
            || self.initialization_window.is_zero()
            || self.initialization_window > Duration::from_secs(10)
            || self.minimum_initialization_duration.is_zero()
            || self.minimum_initialization_duration > self.initialization_window
            || self.max_sample_gap.is_zero()
            || self.max_sample_gap > Duration::from_secs(10)
            || !self.max_angular_speed_rad_s.is_finite()
            || self.max_angular_speed_rad_s <= 0.0
            || self.max_angular_speed_rad_s > 1_000.0
            || !(2..=MAX_POSES).contains(&self.max_pose_count)
        {
            return Err(invalid("fusion options are outside their supported bounds"));
        }
        Ok(self)
    }
}

/// Qualification and filter statistics for an entire camera-clock track.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FusionDiagnostics {
    /// Qualified observations used to establish initial gravity.
    pub initialization_samples: usize,
    /// Start of the coherent initialization interval, on the shared camera clock.
    pub initialization_start: Duration,
    /// End of the coherent initialization interval, on the shared camera clock.
    pub initialization_end: Duration,
    /// Integration substeps whose gravity measurement passed both gates.
    pub gravity_updates: usize,
    /// Integration substeps propagated with gyro alone after gravity rejection.
    pub gravity_rejections: usize,
    /// Sample intervals during which a qualified stationary bias was updated.
    pub stationary_bias_updates: usize,
    /// Final estimated residual bias in calibrated body radians/second.
    pub residual_gyro_bias_rad_s: [f64; 3],
    /// Number of stored poses, including angular-winding-preserving substeps.
    pub pose_count: usize,
}

#[derive(Clone, Copy, Debug)]
struct Pose {
    time: Duration,
    orientation: Orientation,
    heading: f64,
}

/// Deterministic, gravity-referenced camera attitude on a shared camera clock.
///
/// Input must include available pre-roll, with the same nonnegative clock origin
/// as frame capture times. Integration uses `q = q * dq` and actual intervals.
/// Substeps never exceed 0.1 radians, so interpolation retains rotations larger
/// than half a turn between input samples. Sample coverage is never extrapolated.
///
/// Initial heading is defined as zero. Heading subsequently follows the azimuth
/// of body +X, unwrapped across turns. Within roughly three degrees of a vertical
/// +X axis, heading continues from world-vertical gyro motion; azimuth itself is
/// unobservable at that singular orientation.
#[derive(Clone, Debug)]
pub struct AttitudeTrack {
    poses: Vec<Pose>,
    diagnostics: FusionDiagnostics,
}

impl AttitudeTrack {
    /// Builds a track from calibrated body-axis measurements in g and rad/s.
    ///
    /// Rejects nonfinite samples, nonmonotonic timestamps, telemetry gaps,
    /// possible gyro saturation, unqualified initial gravity, and size limits.
    pub fn new(samples: &[MotionSample], options: FusionOptions) -> Result<Self> {
        let options = options.validate()?;
        validate_samples(samples, options)?;
        let (initial, initialization) = initialize(samples, options)?;
        Self::integrate(samples, options, initial, 0.0, [0.0; 3], initialization)
    }

    /// Continues the same world frame at a verified chapter boundary. The media
    /// adapter supplies the preceding pose on a common camera clock; gravity is
    /// already initialized and must not be reset to a new heading.
    #[cfg(any(feature = "media", test))]
    pub(crate) fn from_state(
        samples: &[MotionSample],
        options: FusionOptions,
        orientation: Orientation,
        heading: f64,
        bias: [f64; 3],
    ) -> Result<Self> {
        let options = options.validate()?;
        validate_samples(samples, options)?;
        if options.estimate_stationary_bias {
            return Err(invalid(
                "chapter continuation does not support stationary bias estimation",
            ));
        }
        if !heading.is_finite() || bias.iter().any(|value| !value.is_finite()) {
            return Err(invalid("invalid fusion continuation state"));
        }
        Self::integrate(
            samples,
            options,
            orientation.validate()?,
            heading,
            bias,
            InitialGravityWindow {
                reference: UP,
                sum: UP,
                start: samples[0].timestamp,
                end: samples[0].timestamp,
                samples: 0,
            },
        )
    }

    fn integrate(
        samples: &[MotionSample],
        options: FusionOptions,
        initial: Orientation,
        initial_heading: f64,
        initial_bias: [f64; 3],
        initialization: InitialGravityWindow,
    ) -> Result<Self> {
        let pose_count = samples.windows(2).try_fold(1usize, |count, pair| {
            let next = count
                .checked_add(substep_count(pair, options))
                .ok_or_else(|| invalid("fusion pose count overflowed"))?;
            if next > options.max_pose_count {
                return Err(invalid(
                    "fusion exceeds the configured pose allocation limit",
                ));
            }
            Ok(next)
        })?;
        let mut poses = Vec::new();
        poses
            .try_reserve_exact(pose_count)
            .map_err(|_| invalid("cannot allocate the bounded fusion pose track"))?;
        poses.push(Pose {
            time: samples[0].timestamp,
            orientation: initial,
            heading: initial_heading,
        });
        let mut diagnostics = FusionDiagnostics {
            initialization_samples: initialization.samples,
            initialization_start: initialization.start,
            initialization_end: initialization.end,
            pose_count,
            ..FusionDiagnostics::default()
        };
        let mut current = initial;
        let mut heading = initial_heading;
        let mut bias = initial_bias;
        let mut stationary = StationaryWindow::default();
        for (index, pair) in samples.windows(2).enumerate() {
            let interval = pair[1].timestamp - pair[0].timestamp;
            if options.estimate_stationary_bias {
                stationary.advance(samples, index);
                if let Some(mean) = stationary.qualified_bias(samples, index).filter(|_| {
                    pair[0].timestamp >= initialization.start
                        && gravity_correction(current, pair[0].acceleration, options).is_some()
                }) {
                    let fraction = 1.0 - (-interval.as_secs_f64() / 5.0).exp();
                    bias = lerp(bias, mean, fraction);
                    diagnostics.stationary_bias_updates += 1;
                }
            }
            let steps = substep_count(pair, options);
            let mut previous_time = pair[0].timestamp;
            for step in 1..=steps {
                let time = substep_time(pair[0].timestamp, interval, step, steps)?;
                let seconds = (time - previous_time).as_secs_f64();
                let fraction = ((previous_time - pair[0].timestamp).as_secs_f64() + seconds * 0.5)
                    / interval.as_secs_f64();
                let omega = subtract(
                    lerp(pair[0].angular_velocity, pair[1].angular_velocity, fraction),
                    bias,
                );
                let predicted_mid = current * rotation(omega, seconds * 0.5)?;
                let acceleration = lerp(pair[0].acceleration, pair[1].acceleration, fraction);
                // Initialization may use a later coherent pre-roll interval.
                // Earlier transients must not perturb the gravity estimate that
                // was transported back into this initial body orientation.
                let correction = (previous_time >= initialization.start)
                    .then(|| gravity_correction(predicted_mid, acceleration, options))
                    .flatten();
                let corrected = if let Some(correction) = correction {
                    diagnostics.gravity_updates += 1;
                    add(omega, correction)
                } else {
                    diagnostics.gravity_rejections += 1;
                    omega
                };
                current = current * rotation(corrected, seconds)?;
                let predicted_heading = heading + predicted_mid.rotate_vector(omega)[2] * seconds;
                heading = azimuth(current)
                    .map(|wrapped| unwrap_near(wrapped, predicted_heading))
                    .unwrap_or(predicted_heading);
                poses.push(Pose {
                    time,
                    orientation: current,
                    heading,
                });
                previous_time = time;
            }
        }
        diagnostics.residual_gyro_bias_rad_s = bias;
        Ok(Self { poses, diagnostics })
    }

    /// Inclusive camera-clock coverage; requests outside this interval fail.
    pub fn coverage(&self) -> (Duration, Duration) {
        (self.poses[0].time, self.poses[self.poses.len() - 1].time)
    }

    /// Returns filter qualification statistics for reporting and validation.
    pub fn diagnostics(&self) -> &FusionDiagnostics {
        &self.diagnostics
    }

    /// Interpolates raw body-to-world attitude without extrapolating coverage.
    pub fn pose_at(&self, timestamp: Duration) -> Result<Orientation> {
        let (left, right, fraction) = self.bracket(timestamp)?;
        Ok(left.orientation.slerp(right.orientation, fraction))
    }

    /// Returns unwrapped body +X heading around world +Z in radians.
    pub fn heading_at(&self, timestamp: Duration) -> Result<f64> {
        let (left, right, fraction) = self.bracket(timestamp)?;
        Ok(left.heading + fraction * (right.heading - left.heading))
    }

    /// Returns the desired output-frame attitude in the same world as the pose.
    ///
    /// Direction lock is level with fixed zero initial heading. FlowState-style
    /// leveling retains the current unwrapped heading. Off returns the raw pose.
    /// For source sampling, a world target ray becomes a camera-body ray through
    /// `pose_at(capture_time)?.inverse() * target_at(frame_time, mode)?`.
    pub fn target_at(&self, timestamp: Duration, mode: Stabilization) -> Result<Orientation> {
        match mode {
            Stabilization::Off => self.pose_at(timestamp),
            Stabilization::DirectionLock => {
                self.bracket(timestamp)?;
                Ok(Orientation::IDENTITY)
            }
            Stabilization::FlowState => {
                Orientation::from_axis_angle(UP, self.heading_at(timestamp)?)
            }
        }
    }

    fn bracket(&self, timestamp: Duration) -> Result<(Pose, Pose, f64)> {
        let (start, end) = self.coverage();
        if timestamp < start || timestamp > end {
            return Err(invalid(format!(
                "requested attitude at {timestamp:?} is outside telemetry coverage {start:?}..={end:?}"
            )));
        }
        let insertion = self.poses.partition_point(|pose| pose.time <= timestamp);
        let left = self.poses[insertion - 1];
        if insertion == self.poses.len() || left.time == timestamp {
            return Ok((left, left, 0.0));
        }
        let right = self.poses[insertion];
        let fraction =
            (timestamp - left.time).as_secs_f64() / (right.time - left.time).as_secs_f64();
        Ok((left, right, fraction))
    }
}

fn validate_samples(samples: &[MotionSample], options: FusionOptions) -> Result<()> {
    if samples.len() < 2 || samples.len() > options.max_pose_count {
        return Err(invalid(
            "fusion requires two or more samples within the pose allocation limit",
        ));
    }
    for sample in samples {
        MotionSample::new(
            sample.timestamp,
            sample.acceleration,
            sample.angular_velocity,
        )?;
        if vector_length(sample.angular_velocity) >= options.max_angular_speed_rad_s {
            return Err(invalid(
                "gyro speed reaches the configured saturation limit",
            ));
        }
    }
    for pair in samples.windows(2) {
        let Some(interval) = pair[1].timestamp.checked_sub(pair[0].timestamp) else {
            return Err(invalid("fusion timestamps must be strictly increasing"));
        };
        if interval.is_zero() {
            return Err(invalid("fusion timestamps must be strictly increasing"));
        }
        if interval > options.max_sample_gap {
            return Err(invalid(format!(
                "gyro telemetry gap {interval:?} exceeds the supported limit"
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct InitialGravityWindow {
    reference: [f64; 3],
    sum: [f64; 3],
    start: Duration,
    end: Duration,
    samples: usize,
}

impl InitialGravityWindow {
    fn new(direction: [f64; 3], timestamp: Duration) -> Self {
        Self {
            reference: direction,
            sum: direction,
            start: timestamp,
            end: timestamp,
            samples: 1,
        }
    }

    fn retain_longer(self, best: &mut Option<Self>, minimum_duration: Duration) {
        let span = self.end - self.start;
        if self.samples >= 2
            && span >= minimum_duration
            && best.is_none_or(|previous| span > previous.end - previous.start)
        {
            *best = Some(self);
        }
    }
}

fn initialize(
    samples: &[MotionSample],
    options: FusionOptions,
) -> Result<(Orientation, InitialGravityWindow)> {
    let mut relative = Orientation::IDENTITY;
    let mut candidate: Option<InitialGravityWindow> = None;
    let mut best = None;
    for (index, sample) in samples.iter().enumerate() {
        if sample.timestamp - samples[0].timestamp > options.initialization_window {
            break;
        }
        if index > 0 {
            let pair = &samples[index - 1..=index];
            let interval = pair[1].timestamp - pair[0].timestamp;
            let steps = substep_count(pair, options);
            for step in 0..steps {
                let omega = lerp(
                    pair[0].angular_velocity,
                    pair[1].angular_velocity,
                    (step as f64 + 0.5) / steps as f64,
                );
                relative = relative * rotation(omega, interval.as_secs_f64() / steps as f64)?;
            }
        }
        let Some(direction) = qualified_acceleration(sample.acceleration, options) else {
            if let Some(window) = candidate.take() {
                window.retain_longer(&mut best, options.minimum_initialization_duration);
            }
            continue;
        };
        // Transport each measurement into the first body frame. A turning camera
        // can therefore use pre-roll without mistaking rotation for acceleration.
        let direction = relative.rotate_vector(direction);
        if let Some(window) = candidate.as_mut() {
            if dot(window.reference, direction) >= INITIAL_DIRECTION_TOLERANCE.cos() {
                window.sum = add(window.sum, direction);
                window.end = sample.timestamp;
                window.samples += 1;
            } else {
                window.retain_longer(&mut best, options.minimum_initialization_duration);
                candidate = Some(InitialGravityWindow::new(direction, sample.timestamp));
            }
        } else {
            candidate = Some(InitialGravityWindow::new(direction, sample.timestamp));
        }
    }
    if let Some(window) = candidate {
        window.retain_longer(&mut best, options.minimum_initialization_duration);
    }
    // Select the longest coherent contiguous interval, so a startup shock does
    // not pin initialization to the first reading. Keep the magnitude, angular,
    // and minimum-duration gates, and never bridge a rejected observation.
    let initialization = best.ok_or_else(|| {
        invalid("cannot initialize gravity from a sufficiently long, consistent initial interval")
    })?;
    let direction = scale(initialization.sum, 1.0 / vector_length(initialization.sum));
    let tilt = if direction[2] < -1.0 + 1.0e-12 {
        // Cross products vanish for antiparallel vectors; choose a deterministic
        // 180-degree rotation around body X instead of falling back to identity.
        Orientation::from_axis_angle([1.0, 0.0, 0.0], PI)?
    } else {
        let axis = cross(direction, UP);
        Orientation::new(1.0 + direction[2], axis[0], axis[1], axis[2])?
    };
    let heading = azimuth(tilt).unwrap_or_else(|| {
        let right = tilt.rotate_vector([0.0, 1.0, 0.0]);
        right[1].atan2(right[0]) - PI * 0.5
    });
    Ok((
        Orientation::from_axis_angle(UP, -heading)? * tilt,
        initialization,
    ))
}

fn qualified_acceleration(acceleration: [f64; 3], options: FusionOptions) -> Option<[f64; 3]> {
    let magnitude = vector_length(acceleration);
    if !magnitude.is_finite() || (magnitude - 1.0).abs() > options.acceleration_tolerance_g {
        return None;
    }
    Some(scale(acceleration, 1.0 / magnitude))
}

fn gravity_correction(
    pose: Orientation,
    acceleration: [f64; 3],
    options: FusionOptions,
) -> Option<[f64; 3]> {
    let measured = qualified_acceleration(acceleration, options)?;
    let predicted = pose.inverse().rotate_vector(UP);
    if dot(measured, predicted) < options.max_acceleration_innovation_radians.cos() {
        return None;
    }
    // A measured body +X component needs a negative Y correction. The order of
    // this cross product follows the body-to-world quaternion convention.
    Some(scale(cross(measured, predicted), options.gravity_gain))
}

fn substep_count(pair: &[MotionSample], options: FusionOptions) -> usize {
    let seconds = (pair[1].timestamp - pair[0].timestamp).as_secs_f64();
    let speed =
        vector_length(pair[0].angular_velocity).max(vector_length(pair[1].angular_velocity));
    // Bias cannot exceed the stationary speed threshold; correction magnitude
    // cannot exceed its gain. Include both to maintain the interpolation bound.
    let angle = seconds * (speed + options.gravity_gain + STATIONARY_MAX_SPEED);
    (seconds / MAX_STEP_SECONDS)
        .max(angle / MAX_STEP_RADIANS)
        .ceil()
        .max(1.0) as usize
}

fn substep_time(
    start: Duration,
    interval: Duration,
    step: usize,
    steps: usize,
) -> Result<Duration> {
    if interval.as_nanos() < steps as u128 {
        return Err(invalid(
            "gyro sample interval is too short for bounded integration",
        ));
    }
    let nanos = interval.as_nanos() * step as u128 / steps as u128;
    let nanos =
        u64::try_from(nanos).map_err(|_| invalid("fusion interval overflows nanoseconds"))?;
    start
        .checked_add(Duration::from_nanos(nanos))
        .ok_or_else(|| invalid("fusion timestamp overflows Duration"))
}

fn rotation(velocity: [f64; 3], seconds: f64) -> Result<Orientation> {
    Orientation::from_axis_angle(velocity, vector_length(velocity) * seconds)
}

fn azimuth(pose: Orientation) -> Option<f64> {
    let forward = pose.rotate_vector([1.0, 0.0, 0.0]);
    (forward[0].hypot(forward[1]) > 0.05).then(|| forward[1].atan2(forward[0]))
}

fn unwrap_near(angle: f64, reference: f64) -> f64 {
    angle + ((reference - angle) / TAU).round() * TAU
}

#[derive(Default)]
struct StationaryWindow {
    first: usize,
    count: usize,
    accel_sum: [f64; 3],
    accel_squared_sum: f64,
    gyro_sum: [f64; 3],
    gyro_squared_sum: f64,
}

impl StationaryWindow {
    fn advance(&mut self, samples: &[MotionSample], index: usize) {
        self.accumulate(samples[index], 1.0);
        self.count += 1;
        // Keep the observation immediately preceding the one-second boundary.
        while self.first < index
            && samples[index].timestamp - samples[self.first + 1].timestamp >= STATIONARY_WINDOW
        {
            self.accumulate(samples[self.first], -1.0);
            self.first += 1;
            self.count -= 1;
        }
    }

    fn accumulate(&mut self, sample: MotionSample, sign: f64) {
        self.accel_sum = add(self.accel_sum, scale(sample.acceleration, sign));
        self.accel_squared_sum += sign * dot(sample.acceleration, sample.acceleration);
        self.gyro_sum = add(self.gyro_sum, scale(sample.angular_velocity, sign));
        self.gyro_squared_sum += sign * dot(sample.angular_velocity, sample.angular_velocity);
    }

    fn qualified_bias(&self, samples: &[MotionSample], index: usize) -> Option<[f64; 3]> {
        if self.count < 16
            || samples[index].timestamp - samples[self.first].timestamp < STATIONARY_WINDOW
        {
            return None;
        }
        let accel = scale(self.accel_sum, 1.0 / self.count as f64);
        let gyro = scale(self.gyro_sum, 1.0 / self.count as f64);
        let accel_variance =
            (self.accel_squared_sum / self.count as f64 - dot(accel, accel)).max(0.0);
        let gyro_variance = (self.gyro_squared_sum / self.count as f64 - dot(gyro, gyro)).max(0.0);
        ((vector_length(accel) - 1.0).abs() <= 0.04
            && vector_length(gyro) <= STATIONARY_MAX_SPEED
            && accel_variance <= STATIONARY_ACCEL_STDDEV.powi(2)
            && gyro_variance <= STATIONARY_GYRO_STDDEV.powi(2))
        .then_some(gyro)
    }
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn add(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|axis| left[axis] + right[axis])
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|axis| left[axis] - right[axis])
}

fn scale(vector: [f64; 3], amount: f64) -> [f64; 3] {
    vector.map(|value| value * amount)
}

fn lerp(left: [f64; 3], right: [f64; 3], amount: f64) -> [f64; 3] {
    std::array::from_fn(|axis| left[axis] + amount * (right[axis] - left[axis]))
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidMedia(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(seconds: f64, acceleration: [f64; 3], angular_velocity: [f64; 3]) -> MotionSample {
        MotionSample::new(
            Duration::from_secs_f64(seconds),
            acceleration,
            angular_velocity,
        )
        .expect("finite sample")
    }

    fn assert_vector(actual: [f64; 3], expected: [f64; 3], tolerance: f64) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() < tolerance,
                "{actual} does not match {expected} within {tolerance}"
            );
        }
    }

    fn constant_samples(accel: [f64; 3], omega: [f64; 3], seconds: usize) -> Vec<MotionSample> {
        (0..=seconds * 100)
            .map(|i| sample(i as f64 * 0.01, accel, omega))
            .collect()
    }

    #[test]
    fn continuation_preserves_world_pose_and_unwrapped_heading() {
        let samples = constant_samples(UP, [0.0, 0.0, 2.0], 5);
        let options = FusionOptions::default();
        let full = AttitudeTrack::new(&samples, options).unwrap();
        let boundary = samples[350].timestamp;
        let tail: Vec<_> = samples[350..]
            .iter()
            .map(|sample| MotionSample {
                timestamp: sample.timestamp - boundary,
                ..*sample
            })
            .collect();
        let continued = AttitudeTrack::from_state(
            &tail,
            options,
            full.pose_at(boundary).unwrap(),
            full.heading_at(boundary).unwrap(),
            full.diagnostics().residual_gyro_bias_rad_s,
        )
        .unwrap();
        assert!(continued.heading_at(Duration::ZERO).unwrap() > std::f64::consts::TAU);
        for sample in &tail {
            let expected = full.pose_at(sample.timestamp + boundary).unwrap();
            let actual = continued.pose_at(sample.timestamp).unwrap();
            for axis in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], UP] {
                assert_vector(
                    actual.rotate_vector(axis),
                    expected.rotate_vector(axis),
                    1e-10,
                );
            }
            assert!(
                (continued.heading_at(sample.timestamp).unwrap()
                    - full.heading_at(sample.timestamp + boundary).unwrap())
                .abs()
                    < 1e-10
            );
        }
        assert_eq!(continued.diagnostics().initialization_samples, 0);
    }

    #[test]
    fn continuation_matches_uninterrupted_motion_across_many_overlapping_chapters() {
        let samples = constant_samples(UP, [0.0, 0.0, 2.0], 120);
        let options = FusionOptions::default();
        let uninterrupted = AttitudeTrack::new(&samples, options).unwrap();
        let mut previous = AttitudeTrack::new(&samples[..351], options).unwrap();
        let mut previous_origin = Duration::ZERO;
        for start in (300..samples.len() - 1).step_by(300) {
            let origin = samples[start].timestamp;
            let end = (start + 351).min(samples.len());
            let chapter: Vec<_> = samples[start..end]
                .iter()
                .map(|sample| MotionSample {
                    timestamp: sample.timestamp - origin,
                    ..*sample
                })
                .collect();
            let boundary = origin - previous_origin;
            let continued = AttitudeTrack::from_state(
                &chapter,
                options,
                previous.pose_at(boundary).unwrap(),
                previous.heading_at(boundary).unwrap(),
                previous.diagnostics().residual_gyro_bias_rad_s,
            )
            .unwrap();
            for sample in &chapter {
                let absolute = sample.timestamp + origin;
                let actual = continued.pose_at(sample.timestamp).unwrap();
                let expected = uninterrupted.pose_at(absolute).unwrap();
                for axis in [[1.0, 0.0, 0.0], UP] {
                    assert_vector(
                        actual.rotate_vector(axis),
                        expected.rotate_vector(axis),
                        1e-10,
                    );
                }
                assert!(
                    (continued.heading_at(sample.timestamp).unwrap()
                        - uninterrupted.heading_at(absolute).unwrap())
                    .abs()
                        < 1e-10
                );
            }
            previous = continued;
            previous_origin = origin;
        }
        assert!(previous.heading_at(previous.coverage().1).unwrap() > 30.0 * std::f64::consts::TAU);
    }

    #[test]
    fn levels_tilted_and_upside_down_initial_gravity() {
        for accel in [
            UP,
            [0.3, 0.4, 0.75_f64.sqrt()],
            [0.0, 0.0, -1.0],
            [1.0, 0.0, 0.0],
        ] {
            let samples = constant_samples(accel, [0.0; 3], 1);
            let track =
                AttitudeTrack::new(&samples, FusionOptions::default()).expect("gravity is stable");
            for time in [
                Duration::ZERO,
                Duration::from_millis(355),
                Duration::from_secs(1),
            ] {
                let pose = track.pose_at(time).expect("covered time");
                pose.validate().expect("unit quaternion");
                assert_vector(pose.rotate_vector(accel), UP, 1.0e-10);
                for mode in [Stabilization::DirectionLock, Stabilization::FlowState] {
                    assert_vector(
                        track
                            .target_at(time, mode)
                            .expect("target")
                            .rotate_vector(UP),
                        UP,
                        1.0e-12,
                    );
                }
            }
        }
    }

    // Independent closed-form Rz(yaw) Ry(pitch) Rx(roll) rotation matrix columns.
    fn analytic_axes(t: f64, rates: [f64; 3]) -> [[f64; 3]; 3] {
        let (sr, cr) = (rates[0] * t).sin_cos();
        let (sp, cp) = (rates[1] * t).sin_cos();
        let (sy, cy) = (rates[2] * t).sin_cos();
        [
            [cy * cp, sy * cp, -sp],
            [cy * sp * sr - sy * cr, sy * sp * sr + cy * cr, cp * sr],
            [cy * sp * cr + sy * sr, sy * sp * cr - cy * sr, cp * cr],
        ]
    }

    fn analytic_sample(t: f64, rates: [f64; 3]) -> MotionSample {
        let axes = analytic_axes(t, rates);
        let (sr, cr) = (rates[0] * t).sin_cos();
        let (sp, cp) = (rates[1] * t).sin_cos();
        let omega = [
            rates[0] - rates[2] * sp,
            rates[1] * cr + rates[2] * cp * sr,
            -rates[1] * sr + rates[2] * cp * cr,
        ];
        sample(t, [axes[0][2], axes[1][2], axes[2][2]], omega)
    }

    #[test]
    fn follows_independent_multi_axis_attitude_with_irregular_timestamps() {
        for rates in [
            [0.7, 0.0, 0.0],
            [0.0, -0.4, 0.0],
            [0.0, 0.0, 0.8],
            [0.7, -0.4, 0.8],
        ] {
            let mut time = 0.0;
            let samples: Vec<_> = (0..1201)
                .map(|i| {
                    let sample = analytic_sample(time, rates);
                    time += [0.001, 0.003, 0.002, 0.004][i % 4];
                    sample
                })
                .collect();
            let track = AttitudeTrack::new(&samples, FusionOptions::default())
                .expect("moving gravity initializes");
            for time in [0.0, 0.1475, 0.891, 1.555, 2.98] {
                let expected = analytic_axes(time, rates);
                let actual = track
                    .pose_at(Duration::from_secs_f64(time))
                    .expect("covered time");
                for (basis, expected) in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], UP]
                    .into_iter()
                    .zip(expected)
                {
                    assert_vector(actual.rotate_vector(basis), expected, 2.0e-5);
                }
            }
        }
    }

    #[test]
    fn startup_transients_do_not_pin_gravity_or_shift_the_motion_anchor() {
        let rates = [0.7, -0.4, 0.8];
        let samples: Vec<_> = (0..=500)
            .map(|index| {
                let time = index as f64 * 0.001;
                let mut sample = analytic_sample(time, rates);
                // A brief plausible 1 g startup disturbance, then a shock,
                // precede the longer clean gravity interval. Generate all
                // measurements from the independent analytical camera matrix.
                let specific_force = if index < 30 {
                    // Below the ordinary 20-degree innovation gate: only the
                    // initialization trust boundary keeps this false up from
                    // perturbing the later qualified gravity estimate.
                    [0.17_f64.sin(), 0.0, 0.17_f64.cos()]
                } else if index < 120 {
                    [0.0, -2.0, 0.0]
                } else {
                    UP
                };
                sample.acceleration =
                    analytic_axes(time, rates).map(|body_axis| dot(body_axis, specific_force));
                sample
            })
            .collect();
        let track = AttitudeTrack::new(&samples, FusionOptions::default())
            .expect("later coherent gravity survives a disturbed startup");
        assert!(track.diagnostics().initialization_start >= Duration::from_millis(120));
        assert!(track.diagnostics().initialization_end <= Duration::from_millis(250));
        assert_eq!(track.coverage().0, Duration::ZERO);
        for time in [0.0, 0.06, 0.1855, 0.4] {
            let pose = track
                .pose_at(Duration::from_secs_f64(time))
                .expect("original clock preserved");
            for (basis, expected) in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], UP]
                .into_iter()
                .zip(analytic_axes(time, rates))
            {
                assert_vector(pose.rotate_vector(basis), expected, 2.0e-6);
            }
        }
    }

    #[test]
    fn initialization_search_never_bridges_rejections_or_exceeds_its_window() {
        let interrupted: Vec<_> = (0..=100)
            .map(|index| {
                sample(
                    index as f64 * 0.005,
                    if index % 4 == 3 { [0.0; 3] } else { UP },
                    [0.0; 3],
                )
            })
            .collect();
        assert!(AttitudeTrack::new(&interrupted, FusionOptions::default()).is_err());
        let late: Vec<_> = (0..=100)
            .map(|index| {
                sample(
                    index as f64 * 0.005,
                    if index < 60 { [0.0; 3] } else { UP },
                    [0.0; 3],
                )
            })
            .collect();
        assert!(AttitudeTrack::new(&late, FusionOptions::default()).is_err());
        assert!(AttitudeTrack::new(
            &late,
            FusionOptions {
                initialization_window: Duration::from_millis(500),
                ..FusionOptions::default()
            }
        )
        .is_ok());
    }

    #[test]
    fn retains_more_than_half_a_turn_and_unwrapped_heading() {
        let omega = 25.0;
        let samples: Vec<_> = (0..=6)
            .map(|i| sample(i as f64 * 0.2, UP, [0.0, 0.0, omega]))
            .collect();
        let options = FusionOptions {
            max_sample_gap: Duration::from_millis(250),
            ..FusionOptions::default()
        };
        let track = AttitudeTrack::new(&samples, options).expect("bounded fast rotation");
        for time in [0.1, 0.125, 0.3, 0.77, 1.17] {
            let time_stamp = Duration::from_secs_f64(time);
            let pose = track.pose_at(time_stamp).expect("covered pose");
            assert_vector(
                pose.rotate_vector([1.0, 0.0, 0.0]),
                [(omega * time).cos(), (omega * time).sin(), 0.0],
                2.0e-7,
            );
            assert!(
                (track.heading_at(time_stamp).expect("heading") - omega * time).abs() < 1.0e-10
            );
            assert_vector(
                track
                    .target_at(time_stamp, Stabilization::FlowState)
                    .expect("target")
                    .rotate_vector([1.0, 0.0, 0.0]),
                pose.rotate_vector([1.0, 0.0, 0.0]),
                2.0e-7,
            );
        }
        assert!(track.diagnostics().pose_count > samples.len());
    }

    #[test]
    fn direction_lock_is_fixed_and_flow_state_preserves_intentional_yaw() {
        // Fixed 25-degree roll during a full intentional world-Z pan.
        let roll = 25.0_f64.to_radians();
        let omega = 0.8;
        let accel = [0.0, roll.sin(), roll.cos()];
        let samples = constant_samples(accel, scale(accel, omega), 10);
        let track = AttitudeTrack::new(&samples, FusionOptions::default()).expect("tilted pan");
        for seconds in [0.0, 1.5, 5.0, 9.5] {
            let time = Duration::from_secs_f64(seconds);
            assert_eq!(
                track
                    .target_at(time, Stabilization::DirectionLock)
                    .expect("lock"),
                Orientation::IDENTITY
            );
            let target = track
                .target_at(time, Stabilization::FlowState)
                .expect("flow-state");
            assert_vector(target.rotate_vector(UP), UP, 1.0e-12);
            assert_vector(
                target.rotate_vector([1.0, 0.0, 0.0]),
                [(omega * seconds).cos(), (omega * seconds).sin(), 0.0],
                1.0e-10,
            );
            assert_eq!(
                track.target_at(time, Stabilization::Off).expect("off"),
                track.pose_at(time).expect("pose")
            );
        }
    }

    #[test]
    fn acceleration_magnitude_and_innovation_rejection_preserve_gyro_motion() {
        for bad_accel in [[0.0, 0.0, 2.0], [1.0, 0.0, 0.0], [0.0; 3]] {
            let samples: Vec<_> = (0..=400)
                .map(|i| {
                    sample(
                        i as f64 * 0.01,
                        if (50..250).contains(&i) {
                            bad_accel
                        } else {
                            UP
                        },
                        [0.0, 0.0, 0.3],
                    )
                })
                .collect();
            let track = AttitudeTrack::new(&samples, FusionOptions::default())
                .expect("initial gravity is valid");
            let final_pose = track.pose_at(Duration::from_secs(4)).expect("end covered");
            assert_vector(final_pose.rotate_vector(UP), UP, 1.0e-10);
            assert_vector(
                final_pose.rotate_vector([1.0, 0.0, 0.0]),
                [1.2_f64.cos(), 1.2_f64.sin(), 0.0],
                1.0e-10,
            );
            assert!(track.diagnostics().gravity_rejections >= 199);
            assert!(track.diagnostics().gravity_updates >= 190);
        }
    }

    #[test]
    fn accepted_gravity_corrects_small_accumulated_tilt() {
        // A small uncalibrated roll gyro offset should produce a bounded error
        // under gravity feedback, versus linear drift with gyro-only propagation.
        let samples = constant_samples(UP, [0.01, 0.0, 0.0], 20);
        let track =
            AttitudeTrack::new(&samples, FusionOptions::default()).expect("stationary gravity");
        let no_feedback = AttitudeTrack::new(
            &samples,
            FusionOptions {
                gravity_gain: 0.0,
                ..FusionOptions::default()
            },
        )
        .expect("gyro only");
        let time = Duration::from_secs(20);
        let tilt = track.pose_at(time).expect("pose").rotate_vector(UP)[1].abs();
        let raw_tilt = no_feedback.pose_at(time).expect("pose").rotate_vector(UP)[1].abs();
        assert!(tilt < 0.021, "gravity must bound tilt, got {tilt}");
        assert!(
            raw_tilt > 0.19,
            "gyro-only tilt should drift, got {raw_tilt}"
        );
        assert_eq!(track.diagnostics().stationary_bias_updates, 0);
    }

    #[test]
    fn optional_stationary_bias_converges_and_defaults_preserve_very_slow_yaw() {
        let bias = [0.001, -0.0005, 0.0008];
        let samples = constant_samples(UP, bias, 35);
        let options = FusionOptions {
            estimate_stationary_bias: true,
            ..FusionOptions::default()
        };
        let track = AttitudeTrack::new(&samples, options).expect("qualified stillness");
        assert!(track.diagnostics().stationary_bias_updates > 3000);
        assert_vector(track.diagnostics().residual_gyro_bias_rad_s, bias, 2.0e-6);

        let yaw = 0.001;
        let samples = constant_samples(UP, [0.0, 0.0, yaw], 35);
        let track =
            AttitudeTrack::new(&samples, FusionOptions::default()).expect("slow intentional yaw");
        assert_eq!(track.diagnostics().stationary_bias_updates, 0);
        assert!(
            (track.heading_at(Duration::from_secs(35)).expect("heading") - yaw * 35.0).abs()
                < 1.0e-10
        );
    }

    #[test]
    fn stationary_detector_rejects_pans_and_unstable_sensors() {
        let options = FusionOptions {
            estimate_stationary_bias: true,
            ..FusionOptions::default()
        };
        for kind in 0..3 {
            let samples: Vec<_> = (0..=1000)
                .map(|i| {
                    let alternating = if i % 2 == 0 { 1.0 } else { -1.0 };
                    let omega = if kind == 0 {
                        [0.0, 0.0, 0.01]
                    } else {
                        [
                            0.0,
                            0.0,
                            0.001 + if kind == 1 { alternating * 0.001 } else { 0.0 },
                        ]
                    };
                    let accel = if kind == 2 {
                        [alternating * 0.015, 0.0, 1.0]
                    } else {
                        UP
                    };
                    sample(i as f64 * 0.01, accel, omega)
                })
                .collect();
            let track = AttitudeTrack::new(&samples, options).expect("valid track");
            assert_eq!(
                track.diagnostics().stationary_bias_updates,
                0,
                "kind {kind}"
            );
        }
    }

    #[test]
    fn errors_outside_camera_clock_coverage_including_for_direction_lock() {
        let samples: Vec<_> = (100..=200)
            .map(|i| sample(i as f64 * 0.01, UP, [0.0; 3]))
            .collect();
        let track = AttitudeTrack::new(&samples, FusionOptions::default())
            .expect("camera clock can start after zero");
        assert_eq!(
            track.coverage(),
            (Duration::from_secs(1), Duration::from_secs(2))
        );
        for time in [
            Duration::ZERO,
            Duration::from_millis(999),
            Duration::from_millis(2001),
        ] {
            assert!(track.pose_at(time).is_err());
            assert!(track.heading_at(time).is_err());
            for mode in [
                Stabilization::Off,
                Stabilization::DirectionLock,
                Stabilization::FlowState,
            ] {
                assert!(track.target_at(time, mode).is_err());
            }
        }
        assert!(track.pose_at(Duration::from_secs(1)).is_ok());
        assert!(track.pose_at(Duration::from_secs(2)).is_ok());
    }

    #[test]
    fn invalid_samples_gaps_saturation_and_allocations_fail_explicitly() {
        let good = constant_samples(UP, [0.0; 3], 1);
        let options = FusionOptions::default();
        assert!(AttitudeTrack::new(&[], options).is_err());
        assert!(AttitudeTrack::new(&good[..1], options).is_err());
        for mode in 0..4 {
            let mut bad = good.clone();
            match mode {
                0 => bad[10].angular_velocity[0] = f64::NAN,
                1 => bad[10].timestamp = bad[9].timestamp,
                2 => bad[10].timestamp = Duration::ZERO,
                _ => bad[10].angular_velocity[0] = options.max_angular_speed_rad_s,
            }
            assert!(AttitudeTrack::new(&bad, options).is_err());
        }
        let gap = [sample(0.0, UP, [0.0; 3]), sample(0.5, UP, [0.0; 3])];
        assert!(AttitudeTrack::new(&gap, options)
            .expect_err("gap")
            .to_string()
            .contains("gap"));
        let options = FusionOptions {
            max_pose_count: 110,
            ..options
        };
        assert!(
            AttitudeTrack::new(&constant_samples(UP, [0.0, 0.0, 20.0], 1), options)
                .expect_err("substeps exceed allocation")
                .to_string()
                .contains("allocation")
        );
    }

    #[test]
    fn invalid_options_and_unreliable_initial_gravity_fail_explicitly() {
        let good = constant_samples(UP, [0.0; 3], 1);
        let defaults = FusionOptions::default();
        for bad in [
            FusionOptions {
                gravity_gain: f64::NAN,
                ..defaults
            },
            FusionOptions {
                acceleration_tolerance_g: 1.0,
                ..defaults
            },
            FusionOptions {
                max_acceleration_innovation_radians: 0.0,
                ..defaults
            },
            FusionOptions {
                initialization_window: Duration::ZERO,
                ..defaults
            },
            FusionOptions {
                minimum_initialization_duration: Duration::from_secs(1),
                ..defaults
            },
            FusionOptions {
                max_sample_gap: Duration::ZERO,
                ..defaults
            },
            FusionOptions {
                max_angular_speed_rad_s: 0.0,
                ..defaults
            },
            FusionOptions {
                max_pose_count: MAX_POSES + 1,
                ..defaults
            },
        ] {
            assert!(AttitudeTrack::new(&good, bad).is_err());
        }
        for accel in [[0.0; 3], [0.0, 0.0, 9.81], [1.0, 1.0, 0.0]] {
            assert!(AttitudeTrack::new(&constant_samples(accel, [0.0; 3], 1), defaults).is_err());
        }
        assert!(AttitudeTrack::new(&good[..2], defaults).is_err());
        let inconsistent: Vec<_> = (0..=100)
            .map(|i| {
                sample(
                    i as f64 * 0.01,
                    if i % 3 == 0 { UP } else { [1.0, 0.0, 0.0] },
                    [0.0; 3],
                )
            })
            .collect();
        assert!(AttitudeTrack::new(&inconsistent, defaults).is_err());
    }
}
