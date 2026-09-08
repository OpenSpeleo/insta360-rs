//! Camera-specific interpretation of decoded IMU samples.
//!
//! This is independent of lens extrinsics: those already rotate the body ray
//! into each calibrated lens and must not be applied to IMU measurements again.

use super::readout::ReadoutDirection;
use super::MotionSample;
use crate::container::{FileRotation, InsvMetadata, UnknownMetadataValue};
use crate::profile::camera_profile_for_name;
use crate::types::CameraModel;
use crate::{Error, Result};

/// The observed 56-byte tag-31 calibration payload, retained without assuming
/// an undocumented bias order, unit, or subtraction convention.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordedImuCalibration {
    /// Six little-endian floating-point values in their recorded order.
    pub values: [f64; 6],
    /// Final little-endian integer timestamp, whose unit is not established.
    pub timestamp: u64,
}

/// Verified X5 compact-raw IMU conventions.
///
/// Inputs use the units produced by [`super::decode_motion_record`]: specific
/// force in g and angular velocity in radians per second. Outputs use the
/// stitcher's right-handed body coordinates: forward X, left Y, up Z.
#[derive(Clone, Debug)]
pub struct X5MotionProfile {
    factory_calibration: Option<RecordedImuCalibration>,
    rolling_readout_ms: Option<f64>,
}

impl X5MotionProfile {
    /// Resolves the supported camera and raw-sample layout without substituting
    /// a different camera's axis mapping or an absent IMU full-scale range.
    pub fn from_metadata(metadata: &InsvMetadata) -> Result<Self> {
        let name = metadata
            .camera_name
            .as_deref()
            .unwrap_or("missing camera name");
        if camera_profile_for_name(name).map(|profile| &profile.camera) != Some(&CameraModel::X5) {
            return Err(Error::UnsupportedCamera(format!(
                "automatic IMU normalization requires X5 metadata, found {name}"
            )));
        }
        if metadata.is_raw_gyro != Some(true) {
            return Err(Error::MissingCapability(
                "X5 IMU normalization requires the compact raw gyro layout".into(),
            ));
        }
        let range = metadata.imu_range.ok_or_else(|| {
            Error::MissingCalibration("X5 raw IMU full-scale ranges are missing".into())
        })?;
        super::validate_imu_range(range)?;
        Ok(Self {
            factory_calibration: parse_factory_calibration(metadata)?,
            rolling_readout_ms: metadata.rolling_shutter_time,
        })
    }

    /// Rotates one observation into camera-body coordinates while preserving
    /// its timestamp and units. Specific force points up for a stationary IMU.
    pub fn normalize_sample(&self, sample: MotionSample) -> Result<MotionSample> {
        MotionSample::new(
            sample.timestamp,
            raw_to_body(sample.acceleration),
            raw_to_body(sample.angular_velocity),
        )
    }

    /// Normalizes a sequence in place after validating every observation.
    pub fn normalize_samples(&self, samples: &mut [MotionSample]) -> Result<()> {
        // Validate before mutation so malformed input cannot leave a partially
        // transformed sequence that a caller might transform a second time.
        for sample in samples.iter() {
            MotionSample::new(
                sample.timestamp,
                sample.acceleration,
                sample.angular_velocity,
            )?;
        }
        for sample in samples {
            sample.acceleration = raw_to_body(sample.acceleration);
            sample.angular_velocity = raw_to_body(sample.angular_velocity);
        }
        Ok(())
    }

    /// Recorded calibration values. They are intentionally not applied as a
    /// guessed factory bias; stationary bias estimation is a separate step.
    pub fn factory_calibration(&self) -> Option<&RecordedImuCalibration> {
        self.factory_calibration.as_ref()
    }

    /// Recorded-resolution rolling-shutter duration in seconds.
    ///
    /// X5 tag 25 is milliseconds. This value alone does not specify scan
    /// direction or the anchor of the frame's exposure timestamp.
    pub fn rolling_readout_seconds(&self) -> Result<f64> {
        let value = self.rolling_readout_ms.ok_or_else(|| {
            Error::MissingCalibration("X5 rolling-shutter duration is missing".into())
        })?;
        if !value.is_finite() || value < 0.0 {
            return Err(Error::InvalidMedia(
                "X5 rolling-shutter duration must be finite and nonnegative".into(),
            ));
        }
        Ok(value / 1_000.0)
    }

    /// Resolves native decoded-image scan direction, readout interval,
    /// and duration in seconds for both X5 lenses.
    ///
    /// This path supports an explicitly unrotated image and a valid recorded
    /// crop. Crop offsets move the crop relative to the sensor center; they
    /// are not absolute top-left coordinates. Negative offsets are valid when
    /// the complete crop remains inside the sensor. Unestablished rotations,
    /// missing crops, and partially decoded declarations are rejected.
    ///
    /// Tag 25 spans the recorded active image, so the interval is `[0, 1]`.
    /// Applying the firmware destination/source ratio again would shorten the
    /// recorded sweep. This interpretation is corroborated by the vendor implementation's
    /// resolution-specific sweep and the primary X5 telemetry-parser path;
    /// physical readout timing has not been independently measured.
    pub fn readout_profile(
        &self,
        metadata: &InsvMetadata,
    ) -> Result<(ReadoutDirection, [f64; 2], f64)> {
        if metadata.file_rotation != Some(FileRotation::Degrees0)
            || metadata
                .unknown_fields
                .iter()
                .any(|field| matches!(field.number, 27 | 130))
        {
            return Err(Error::MissingCapability(
                "X5 rolling shutter requires unambiguous unrotated source and crop metadata".into(),
            ));
        }
        let crop = metadata.crop_window.as_ref().ok_or_else(|| {
            Error::MissingCalibration("X5 sensor crop metadata is missing".into())
        })?;
        if !crop.unknown_fields.is_empty() {
            return Err(Error::MissingCapability(
                "X5 sensor crop contains unsupported fields".into(),
            ));
        }
        let validate_crop = |source: u32, destination: u32, offset: i32| -> Result<()> {
            if source == 0 || destination == 0 || destination > source {
                return Err(Error::InvalidMedia(
                    "X5 sensor crop dimensions are invalid".into(),
                ));
            }
            // INSOffsetCalculator::cropOffset (0x3cce1c) negates the recorded
            // offsets; OffsetConvert::convertOffset (0x1e38f54..70) computes
            // (src-dst)/2 - supplied_offset before adjusting lens centers.
            let source = f64::from(source);
            let destination = f64::from(destination);
            let start = (source - destination) * 0.5 + f64::from(offset);
            let end = start + destination;
            if start < 0.0 || end > source {
                return Err(Error::InvalidMedia(
                    "X5 sensor crop lies outside the sensor".into(),
                ));
            }
            Ok(())
        };
        validate_crop(crop.source_width, crop.destination_width, crop.x_offset)?;
        validate_crop(crop.source_height, crop.destination_height, crop.y_offset)?;
        // INSSphericalPanoObject's constructor sets verticalSweep=YES at
        // 0x6dee8..f0. INSSphereModel selects normalized projected v for both
        // lenses (0x64b35c,0x64b5ac); applyGyroStabilization samples poses at
        // center - sweep/2 + sweep*v (0x6f27c..284,0x6f2d0..2e8).
        // telemetry-parser's insta360::insert_lens_profile forwards tag 25
        // unchanged for the encoded image and explicitly ignores `_src`.
        Ok((
            ReadoutDirection::TopToBottom,
            [0.0, 1.0],
            self.rolling_readout_seconds()?,
        ))
    }
}

fn raw_to_body([x, y, z]: [f64; 3]) -> [f64; 3] {
    // INSCoreMedia iOS 1.10.4 static evidence:
    // dataTypeToStabilizerGyroType(A3=33) -> 145 (table at 0x50720ec).
    // AlignAxes, type 145, 0x19fc288: omega=(-y,z,-x), gravity=(y,-z,x).
    // The latter is gravity, the negative of accelerometer specific force.
    // AlignAxes produces the stabilizer's frame, not the OffsetParser/projector
    // body frame. Independent optical camera rotations (x5_optical_motion.json)
    // establish that the latter differs by a proper half-turn about Y. Omitting
    // that bridge reverses two axes of physical motion and amplifies camera
    // shake. Keep the two stages explicit; this is a discrete basis conversion,
    // not a fitted camera-specific correction.
    let gyrostab = [-y, z, -x];
    [-gyrostab[0], gyrostab[1], -gyrostab[2]]
}

fn parse_factory_calibration(metadata: &InsvMetadata) -> Result<Option<RecordedImuCalibration>> {
    let mut parsed = None;
    for field in metadata
        .unknown_fields
        .iter()
        .filter(|field| field.number == 31)
    {
        let UnknownMetadataValue::LengthDelimited(bytes) = &field.value else {
            return Err(Error::InvalidMedia(
                "X5 gyro calibration has the wrong wire type".into(),
            ));
        };
        if bytes.len() != 56 {
            return Err(Error::MissingCapability(format!(
                "X5 gyro calibration layout has unsupported length {}",
                bytes.len()
            )));
        }
        let calibration = RecordedImuCalibration {
            values: std::array::from_fn(|index| {
                f64::from_le_bytes(bytes[index * 8..index * 8 + 8].try_into().unwrap())
            }),
            timestamp: u64::from_le_bytes(bytes[48..56].try_into().unwrap()),
        };
        if !calibration.values.iter().all(|value| value.is_finite()) {
            return Err(Error::InvalidMedia(
                "X5 gyro calibration contains a non-finite value".into(),
            ));
        }
        if parsed
            .as_ref()
            .is_some_and(|previous| previous != &calibration)
        {
            return Err(Error::InvalidMedia(
                "X5 gyro calibration declarations conflict".into(),
            ));
        }
        parsed = Some(calibration);
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{CropWindow, ImuRange, UnknownMetadataField};
    use crate::motion::Orientation;
    use std::time::Duration;

    fn metadata() -> InsvMetadata {
        InsvMetadata {
            camera_name: Some("Insta360 X5".into()),
            is_raw_gyro: Some(true),
            imu_range: Some(ImuRange {
                accelerometer_g: 32.0,
                gyroscope_degrees_per_second: 2_000.0,
            }),
            rolling_shutter_time: Some(11.836874961853027),
            ..InsvMetadata::default()
        }
    }

    fn calibration_field(values: [f64; 6], timestamp: u64) -> UnknownMetadataField {
        let mut bytes: Vec<_> = values.into_iter().flat_map(f64::to_le_bytes).collect();
        bytes.extend(timestamp.to_le_bytes());
        UnknownMetadataField {
            number: 31,
            value: UnknownMetadataValue::LengthDelimited(bytes),
        }
    }

    #[test]
    fn x5_axes_preserve_units_and_match_three_independent_basis_rotations() {
        let profile = X5MotionProfile::from_metadata(&metadata()).unwrap();
        let timestamp = Duration::from_millis(17);
        for (raw, body) in [
            ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
            ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
            ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ] {
            let sample = profile
                .normalize_sample(MotionSample::new(timestamp, raw, raw).unwrap())
                .unwrap();
            assert_eq!(sample.timestamp, timestamp);
            assert_eq!(sample.acceleration, body);
            assert_eq!(sample.angular_velocity, body);
        }
        // The vendor implementation uses downward gravity; our fusion consumes the opposing
        // stationary specific force. Raw +X is therefore projector body +Z/up.
        assert_eq!(raw_to_body([1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]);
    }

    #[test]
    fn axis_transform_is_a_proper_rotation_and_commutes_with_integration() {
        // Compose the vendor intermediate basis with the independently established
        // projector mounting; verify its handedness and integration covariance.
        let raw_to_gyrostab = Orientation::new(0.5, -0.5, 0.5, 0.5).unwrap();
        let gyro_to_projector =
            Orientation::from_axis_angle([0.0, 1.0, 0.0], std::f64::consts::PI).unwrap();
        let conversion = gyro_to_projector * raw_to_gyrostab;
        for axis in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
            let raw_rotation = Orientation::from_axis_angle(axis, 0.4).unwrap();
            let expected = conversion * raw_rotation * conversion.inverse();
            let body_rotation = Orientation::from_axis_angle(raw_to_body(axis), 0.4).unwrap();
            for vector in [[0.2, -0.3, 0.7], axis] {
                let expected = expected.rotate_vector(vector);
                let actual = body_rotation.rotate_vector(vector);
                assert!(actual
                    .into_iter()
                    .zip(expected)
                    .all(|(a, b)| (a - b).abs() < 1.0e-12));
            }
        }
    }

    #[test]
    fn compact_raw_scaling_reaches_body_g_and_radians_per_second_once() {
        let mut metadata = metadata();
        metadata.first_frame_timestamp = Some(1_000_000);
        let mut bytes = 1_002_000_i64.to_le_bytes().to_vec();
        for encoded in [33_792_u16, 32_768, 32_768, 32_768, 33_792, 32_768] {
            bytes.extend(encoded.to_le_bytes());
        }
        let mut samples = super::super::decode_motion_record(&bytes, &metadata).unwrap();
        X5MotionProfile::from_metadata(&metadata)
            .unwrap()
            .normalize_samples(&mut samples)
            .unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].timestamp, Duration::from_millis(2));
        assert_eq!(samples[0].acceleration, [0.0, 0.0, 1.0]);
        assert!((samples[0].angular_velocity[0] - 62.5_f64.to_radians()).abs() < 1.0e-12);
        assert_eq!(samples[0].angular_velocity[1..], [0.0, 0.0]);
    }

    #[test]
    fn mounting_matches_independent_optical_camera_rotation_about_every_axis() {
        #[derive(serde::Deserialize)]
        struct Observation {
            raw_integrated_rotation_radians: [f64; 3],
            optical_projector_rotation_radians: [f64; 3],
        }
        #[derive(serde::Deserialize)]
        struct Fixture {
            interval_seconds: f64,
            observations: Vec<Observation>,
        }
        // The expected motion comes from tracked image features in an
        // unstabilized panorama, independently of the IMU basis conversion.
        // Scene-bearing rotation is inverted to obtain physical camera motion.
        // Selection uses optical quality and magnitude, not agreement with the
        // gyro. These observations reject the former two-axis sign reversal;
        // synthetic integration in a self-consistent wrong basis cannot.
        let fixture: Fixture =
            serde_json::from_str(include_str!("../../tests/fixtures/x5_optical_motion.json"))
                .unwrap();
        let profile = X5MotionProfile::from_metadata(&metadata()).unwrap();
        assert_eq!(fixture.observations.len(), 12);
        for observation in fixture.observations {
            let raw = observation
                .raw_integrated_rotation_radians
                .map(|value| value / fixture.interval_seconds);
            let normalized = profile
                .normalize_sample(MotionSample::new(Duration::ZERO, [1.0, 0.0, 0.0], raw).unwrap())
                .unwrap();
            let rotation = normalized
                .angular_velocity
                .map(|value| value * fixture.interval_seconds);
            let error = rotation
                .into_iter()
                .zip(observation.optical_projector_rotation_radians)
                .map(|(actual, optical)| (actual - optical).powi(2))
                .sum::<f64>()
                .sqrt();
            assert!(
                error < 0.25_f64.to_radians(),
                "IMU/optical rotation disagrees by {} degrees",
                error.to_degrees()
            );
        }
    }

    #[test]
    fn recorded_calibration_is_preserved_without_guessing_bias_semantics() {
        let mut metadata = metadata();
        let values = [0.001, -0.002, 0.003, -0.004, 0.005, -0.006];
        metadata.unknown_fields.push(calibration_field(values, 123));
        let profile = X5MotionProfile::from_metadata(&metadata).unwrap();
        assert_eq!(
            profile.factory_calibration(),
            Some(&RecordedImuCalibration {
                values,
                timestamp: 123
            })
        );
        let zero = MotionSample::new(Duration::ZERO, [0.0; 3], [0.0; 3]).unwrap();
        assert_eq!(profile.normalize_sample(zero).unwrap(), zero);
    }

    #[test]
    fn unsupported_or_ambiguous_profile_data_fails_explicitly() {
        let mut value = metadata();
        value.camera_name = Some("Insta360 X4".into());
        assert!(X5MotionProfile::from_metadata(&value).is_err());
        value = metadata();
        value.is_raw_gyro = Some(false);
        assert!(X5MotionProfile::from_metadata(&value).is_err());
        value = metadata();
        value.imu_range = None;
        assert!(X5MotionProfile::from_metadata(&value).is_err());
        value = metadata();
        value.unknown_fields.push(calibration_field([0.0; 6], 0));
        value.unknown_fields.push(calibration_field([0.1; 6], 0));
        assert!(X5MotionProfile::from_metadata(&value).is_err());
        value.unknown_fields = vec![calibration_field([f64::NAN; 6], 0)];
        assert!(X5MotionProfile::from_metadata(&value).is_err());
        value.unknown_fields = vec![UnknownMetadataField {
            number: 31,
            value: UnknownMetadataValue::LengthDelimited(vec![0; 48]),
        }];
        assert!(X5MotionProfile::from_metadata(&value).is_err());
    }

    #[test]
    fn readout_uses_milliseconds_without_an_invented_default() {
        let mut metadata = metadata();
        let profile = X5MotionProfile::from_metadata(&metadata).unwrap();
        assert!(
            (profile.rolling_readout_seconds().unwrap() - 0.011836874961853027).abs() < 1.0e-15
        );
        for invalid in [None, Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
            metadata.rolling_shutter_time = invalid;
            assert!(X5MotionProfile::from_metadata(&metadata)
                .unwrap()
                .rolling_readout_seconds()
                .is_err());
        }
    }

    #[test]
    fn failed_sequence_validation_does_not_partially_rotate_samples() {
        let profile = X5MotionProfile::from_metadata(&metadata()).unwrap();
        let valid = MotionSample::new(Duration::ZERO, [1.0, 2.0, 3.0], [4.0, 5.0, 6.0]).unwrap();
        let mut invalid = valid;
        invalid.angular_velocity[0] = f64::NAN;
        let mut samples = [valid, invalid];
        assert!(profile.normalize_samples(&mut samples).is_err());
        assert_eq!(samples[0], valid);
    }

    #[test]
    fn x5_readout_spans_recorded_active_image_without_a_second_firmware_crop() {
        let mut metadata = metadata();
        metadata.file_rotation = Some(FileRotation::Degrees0);
        metadata.crop_window = Some(CropWindow {
            source_width: 5376,
            source_height: 5376,
            destination_width: 5312,
            destination_height: 5312,
            x_offset: 0,
            y_offset: 0,
            unknown_fields: Vec::new(),
        });
        let profile = X5MotionProfile::from_metadata(&metadata).unwrap();
        let (direction, fraction, readout) = profile.readout_profile(&metadata).unwrap();
        assert_eq!(direction, ReadoutDirection::TopToBottom);
        assert_eq!(fraction, [0.0, 1.0]);
        assert_eq!(readout, profile.rolling_readout_seconds().unwrap());
        for offset in [-32, -7, 0, 15, 32] {
            metadata.crop_window.as_mut().unwrap().y_offset = offset;
            let (_, fraction, duration) = profile.readout_profile(&metadata).unwrap();
            assert_eq!(fraction, [0.0, 1.0]);
            assert_eq!(duration, readout);
        }
        metadata.crop_window.as_mut().unwrap().y_offset = -33;
        assert!(profile.readout_profile(&metadata).is_err());
        metadata.crop_window.as_mut().unwrap().y_offset = 33;
        assert!(profile.readout_profile(&metadata).is_err());
    }

    #[test]
    fn readout_rejects_unknown_rotation_missing_crop_and_unsupported_crop_fields() {
        let mut metadata = metadata();
        let profile = X5MotionProfile::from_metadata(&metadata).unwrap();
        metadata.file_rotation = Some(FileRotation::Degrees0);
        assert!(profile.readout_profile(&metadata).is_err());
        metadata.crop_window = Some(CropWindow {
            source_width: 16,
            source_height: 16,
            destination_width: 16,
            destination_height: 16,
            x_offset: 0,
            y_offset: 0,
            unknown_fields: Vec::new(),
        });
        assert_eq!(profile.readout_profile(&metadata).unwrap().1, [0.0, 1.0]);
        for rotation in [
            None,
            Some(FileRotation::Unknown),
            Some(FileRotation::Degrees90),
        ] {
            metadata.file_rotation = rotation;
            assert!(profile.readout_profile(&metadata).is_err());
        }
        metadata.file_rotation = Some(FileRotation::Degrees0);
        metadata
            .crop_window
            .as_mut()
            .unwrap()
            .unknown_fields
            .push(UnknownMetadataField {
                number: 7,
                value: UnknownMetadataValue::Varint(1),
            });
        assert!(profile.readout_profile(&metadata).is_err());
    }
}
