//! File-level camera-clock alignment and gravity-referenced stabilization.

use std::io::{Read, Seek};
use std::time::Duration;

use crate::container::{InsvInspection, InsvMetadata, RecordInfo, VideoPtsMapType};
use crate::motion::profile::X5MotionProfile;
use crate::motion::{
    decode_motion_record, AttitudeTrack, FrameMotion, FusionOptions, ReadoutDirection,
    ReadoutPoseTable,
};
use crate::telemetry::{decode_camera_exposure_record, CameraExposureSample};
use crate::timing::{validate_camera_timestamp, ExposureTimeline};
use crate::{Error, InsvReader, Result, RollingShutterCorrection, Stabilization, StitchConfig};

const MAX_GYRO_BYTES: u64 = 5_000_000 * 20;
const MAX_EXPOSURE_BYTES: u64 = 10_000_000 * 16;
const READOUT_POSES: usize = 129;

#[derive(Clone)]
enum FrameClock {
    Exposure(ExposureTimeline),
    Decoder {
        first_frame_micros: i64,
        first_pts_micros: i64,
        exposures: Vec<CameraExposureSample>,
    },
}

impl FrameClock {
    fn exposure_at(&self, pts_micros: i64, origin_micros: i64) -> Result<(f64, Duration)> {
        match self {
            Self::Exposure(timeline) => Ok((
                timeline.timestamp_micros_from_origin_at_pts(pts_micros, origin_micros)?,
                timeline.shutter_speed_at_pts(pts_micros)?,
            )),
            Self::Decoder {
                first_frame_micros,
                first_pts_micros,
                exposures,
            } => {
                let camera_micros = pts_micros
                    .checked_sub(*first_pts_micros)
                    .and_then(|relative| first_frame_micros.checked_add(relative))
                    .ok_or_else(|| {
                        Error::InvalidMedia("decoded video time overflows the camera clock".into())
                    })?;
                validate_camera_timestamp(camera_micros)?;
                let index =
                    exposures.partition_point(|sample| sample.timestamp_micros <= camera_micros);
                if index == 0 || camera_micros > exposures[exposures.len() - 1].timestamp_micros {
                    return Err(Error::InvalidMedia(
                        "decoded frame is outside exposure coverage".into(),
                    ));
                }
                Ok((
                    (i128::from(camera_micros) - i128::from(origin_micros)) as f64,
                    exposures[index - 1].shutter_speed,
                ))
            }
        }
    }
}

#[derive(Clone, Copy)]
struct SensorReadout {
    seconds: f64,
    direction: ReadoutDirection,
    sensor_fraction: [f64; 2],
}

pub(super) struct FileStabilizer {
    attitude: AttitudeTrack,
    clocks: [FrameClock; 2],
    camera_origin_micros: i64,
    gyro_adjust_micros: f64,
    mode: Stabilization,
    readout: Option<SensorReadout>,
    report: String,
    warnings: Vec<String>,
}

impl FileStabilizer {
    pub(super) fn from_reader<R: Read + Seek>(
        reader: &mut InsvReader<R>,
        inspection: &InsvInspection,
        config: &StitchConfig,
    ) -> Result<Option<Self>> {
        if config.stabilization == Stabilization::Off {
            if config.rolling_shutter == RollingShutterCorrection::Required {
                return Err(Error::InvalidMedia(
                    "required rolling-shutter correction conflicts with disabled stabilization"
                        .into(),
                ));
            }
            return Ok(None);
        }
        // Validate the camera convention before reading large telemetry payloads.
        X5MotionProfile::from_metadata(&inspection.metadata)?;
        validate_timing_metadata(&inspection.metadata)?;
        if inspection
            .records
            .iter()
            .any(|record| matches!(record.id, 6 | 128))
        {
            return Err(Error::MissingCapability(
                "stabilization of explicit timelapse frame-PTS or edited time-map records is unsupported".into(),
            ));
        }
        let gyro = unique_record(&inspection.records, 3)?.ok_or_else(|| {
            Error::MissingCapability("stabilization requires an embedded gyro record".into())
        })?;
        let primary = unique_record(&inspection.records, 4)?.ok_or_else(|| {
            Error::MissingCapability("stabilization requires an embedded exposure record".into())
        })?;
        let secondary = unique_record(&inspection.records, 12)?;
        for record in [Some(gyro), Some(primary), secondary].into_iter().flatten() {
            if record.format != 0 {
                return Err(Error::MissingCapability(format!(
                    "telemetry record {} has unsupported format {}",
                    record.id, record.format
                )));
            }
        }
        let presentation = reader.video_presentation_timestamps()?;
        let gyro_payload = reader.read_record_payload(gyro, MAX_GYRO_BYTES)?;
        let primary_payload = reader.read_record_payload(primary, MAX_EXPOSURE_BYTES)?;
        let primary = decode_camera_exposure_record(&primary_payload, &inspection.metadata)?;
        let secondary = secondary
            .map(|record| reader.read_record_payload(record, MAX_EXPOSURE_BYTES))
            .transpose()?
            .map(|payload| decode_camera_exposure_record(&payload, &inspection.metadata))
            .transpose()?;
        Self::from_payloads(
            &inspection.metadata,
            config,
            &gyro_payload,
            primary,
            secondary,
            presentation,
        )
        .map(Some)
    }

    fn from_payloads(
        metadata: &InsvMetadata,
        config: &StitchConfig,
        gyro_payload: &[u8],
        primary: Vec<CameraExposureSample>,
        secondary: Option<Vec<CameraExposureSample>>,
        mut presentation: Vec<Vec<i64>>,
    ) -> Result<Self> {
        let profile = X5MotionProfile::from_metadata(metadata)?;
        validate_timing_metadata(metadata)?;
        if presentation.is_empty()
            || presentation.len() > 2
            || presentation
                .iter()
                .any(|track| track.is_empty() || track.windows(2).any(|pair| pair[0] >= pair[1]))
        {
            return Err(Error::InvalidMedia(
                "stabilization requires one or two unambiguous video PTS sequences".into(),
            ));
        }
        if primary.is_empty() || secondary.as_ref().is_some_and(Vec::is_empty) {
            return Err(Error::InvalidMedia(
                "stabilization exposure records are empty".into(),
            ));
        }
        for sample in primary.iter().chain(secondary.iter().flatten()) {
            validate_camera_timestamp(sample.timestamp_micros)?;
        }
        if gyro_payload.len() < 40 || !gyro_payload.len().is_multiple_of(20) {
            return Err(Error::InvalidMedia(
                "stabilization requires at least two complete raw gyro samples".into(),
            ));
        }
        // Check each physical sensor axis before rotation; a vector norm cannot
        // distinguish valid simultaneous axis motion from a clipped component.
        let mut previous_timestamp = None;
        for (index, sample) in gyro_payload.chunks_exact(20).enumerate() {
            let timestamp = i64::from_le_bytes(sample[..8].try_into().unwrap());
            validate_camera_timestamp(timestamp)?;
            if previous_timestamp.is_some_and(|previous| previous >= timestamp) {
                return Err(Error::InvalidMedia(
                    "raw gyro timestamps must be strictly increasing, including pre-roll".into(),
                ));
            }
            previous_timestamp = Some(timestamp);
            for offset in [14, 16, 18] {
                let encoded = u16::from_le_bytes([sample[offset], sample[offset + 1]]);
                if encoded <= 8 || encoded >= u16::MAX - 8 {
                    return Err(Error::InvalidMedia(format!(
                        "raw gyro sample {index} reaches a sensor saturation rail"
                    )));
                }
            }
        }
        let camera_origin_micros = i64::from_le_bytes(gyro_payload[..8].try_into().unwrap());
        let first_frame_micros = metadata.first_frame_timestamp.ok_or_else(|| {
            Error::MissingCalibration(
                "stabilization requires the first-frame camera timestamp".into(),
            )
        })?;
        validate_camera_timestamp(first_frame_micros)?;
        let exposure_mapping = match metadata.video_pts_map_type {
            Some(VideoPtsMapType::ReadingInExposureFile) => true,
            None
            | Some(VideoPtsMapType::Unknown | VideoPtsMapType::DecoderWithFirstFrameTimestamp) => {
                false
            }
            Some(VideoPtsMapType::Other(value)) => {
                return Err(Error::MissingCapability(format!(
                    "stabilization does not support video PTS mapping type {value}"
                )))
            }
        };
        let gyro_adjust_micros = gyro_adjustment_micros(metadata)?;
        if presentation.len() == 1 {
            presentation.push(presentation[0].clone());
        }
        let shared_exposures = secondary.is_none();
        // The vendor converter uses the primary exposure clock when the
        // requested lens has no secondary exposure record.
        let secondary = secondary.unwrap_or_else(|| primary.clone());
        let build_clock = |exposures: Vec<CameraExposureSample>, pts: &[i64]| {
            if exposure_mapping {
                ExposureTimeline::new(&exposures, first_frame_micros, pts).map(FrameClock::Exposure)
            } else {
                Ok(FrameClock::Decoder {
                    first_frame_micros,
                    first_pts_micros: pts[0],
                    exposures,
                })
            }
        };
        let mut clocks = [
            build_clock(primary, &presentation[0])?,
            build_clock(secondary, &presentation[1])?,
        ];
        let minimum_camera_interval = clocks
            .iter()
            .zip(&presentation)
            .filter_map(|(clock, pts)| match clock {
                FrameClock::Exposure(timeline) => timeline.minimum_frame_interval_micros(),
                FrameClock::Decoder { .. } => {
                    pts.windows(2).map(|pair| pair[1].abs_diff(pair[0])).min()
                }
            })
            .min()
            .map(|micros| micros as f64 / 1_000_000.0);
        if metadata.reverse_video_track_order == Some(true) {
            clocks.swap(0, 1);
            presentation.swap(0, 1);
        }

        // Decode onto one nonnegative camera clock, retaining pre-roll. The
        // public media-relative decoder keeps its mode-2 guard; only this path
        // supplies the verified exposure mapping and applies the offset once.
        let mut normalized_metadata = metadata.clone();
        normalized_metadata.first_frame_timestamp = Some(camera_origin_micros);
        normalized_metadata.gyro_timestamp_adjust_ms = Some(0.0);
        normalized_metadata.video_pts_map_type =
            Some(VideoPtsMapType::DecoderWithFirstFrameTimestamp);
        let mut samples = decode_motion_record(gyro_payload, &normalized_metadata)?;
        profile.normalize_samples(&mut samples)?;
        let first_capture = clocks
            .iter()
            .zip(&presentation)
            .map(|(clock, pts)| {
                let (timestamp, shutter) = clock.exposure_at(pts[0], camera_origin_micros)?;
                Ok(timestamp + gyro_adjust_micros - shutter.as_secs_f64() * 500_000.0)
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .reduce(f64::min)
            .unwrap();
        let initialization_window =
            Duration::try_from_secs_f64((first_capture / 1_000_000.0).min(2.0)).map_err(|_| {
                Error::InvalidMedia("no gyro pre-roll precedes the first capture".into())
            })?;
        let options = FusionOptions {
            initialization_window,
            max_angular_speed_rad_s: metadata
                .imu_range
                .unwrap()
                .gyroscope_degrees_per_second
                .to_radians()
                * 3.0_f64.sqrt(),
            ..FusionOptions::default()
        };
        let attitude = AttitudeTrack::new(&samples, options)?;
        let mut warnings = Vec::new();
        let readout = if config.rolling_shutter == RollingShutterCorrection::Off {
            None
        } else {
            let resolved = (|| {
                let seconds = profile.rolling_readout_seconds()?;
                if minimum_camera_interval.is_some_and(|interval| seconds > interval + 0.000002) {
                    return Err(Error::InvalidMedia(
                        "sensor readout duration exceeds the recorded exposure frame interval"
                            .into(),
                    ));
                }
                if seconds * (options.max_angular_speed_rad_s + options.gravity_gain)
                    / (READOUT_POSES - 1) as f64
                    >= std::f64::consts::FRAC_PI_2
                {
                    return Err(Error::MissingCapability("sensor readout cannot preserve angular winding within the bounded pose table".into()));
                }
                let (direction, sensor_fraction, _) = profile.readout_profile(metadata)?;
                Ok(SensorReadout {
                    seconds,
                    direction,
                    sensor_fraction,
                })
            })();
            match resolved {
                Ok(readout) => Some(readout),
                Err(error @ (Error::MissingCalibration(_) | Error::MissingCapability(_)))
                    if config.rolling_shutter == RollingShutterCorrection::Auto =>
                {
                    warnings.push(format!("rolling-shutter correction omitted: {error}"));
                    None
                }
                Err(error) => return Err(error),
            }
        };
        if profile.factory_calibration().is_some() {
            warnings.push("recorded IMU calibration is retained but its undocumented bias layout is not applied".into());
        }
        let diagnostics = attitude.diagnostics();
        let report = format!(
            "X5 gravity fusion: {} IMU samples, {} gravity observations initialized from {:.3}..{:.3}s of retained pre-roll, {} gravity updates, {} rejected updates; {}; {}; rolling shutter {}",
            samples.len(), diagnostics.initialization_samples,
            diagnostics.initialization_start.as_secs_f64(), diagnostics.initialization_end.as_secs_f64(), diagnostics.gravity_updates,
            diagnostics.gravity_rejections,
            if exposure_mapping { "actual video PTS mapped through exposure timestamps" } else { "decoder PTS plus first-frame camera timestamp" },
            if shared_exposures { "primary exposure clock shared by both lenses" } else { "independent primary and secondary exposure clocks" },
            if readout.is_some() { "enabled from recorded timing and source-sensor geometry" } else { "disabled" },
        );
        let stabilizer = Self {
            attitude,
            clocks,
            camera_origin_micros,
            gyro_adjust_micros,
            mode: config.stabilization,
            readout,
            report,
            warnings,
        };
        // Fail during preparation if any frame/readout lies outside telemetry,
        // rather than extrapolating or discovering a missing tail after export.
        let (start, end) = stabilizer.attitude.coverage();
        let half_readout = readout.map_or(0.0, |readout| readout.seconds * 500_000.0);
        for (lens, pts) in presentation.iter().enumerate() {
            for &timestamp in pts {
                let center = stabilizer.frame_center_micros(lens, timestamp)?;
                for camera_time in [center - half_readout, center + half_readout] {
                    let time = stabilizer.relative_time(camera_time)?;
                    if time < start || time > end {
                        return Err(Error::InvalidMedia(format!(
                            "lens {lens} frame at {timestamp} us exceeds gyro coverage during exposure/readout"
                        )));
                    }
                }
            }
        }
        Ok(stabilizer)
    }

    pub(super) fn frame_motion(&self, pts_micros: i64) -> Result<FrameMotion> {
        let centers = [
            self.frame_center_micros(0, pts_micros)?,
            self.frame_center_micros(1, pts_micros)?,
        ];
        let reference_time = self.relative_time(centers[0])?;
        let reference = self.attitude.pose_at(reference_time)?;
        let target = self.attitude.target_at(reference_time, self.mode)?;
        let mut tables = [None, None];
        for lens in 0..2 {
            if let Some(readout) = self.readout.filter(|readout| readout.seconds > 0.0) {
                let mut poses = Vec::with_capacity(READOUT_POSES);
                for index in 0..READOUT_POSES {
                    let fraction = index as f64 / (READOUT_POSES - 1) as f64;
                    let camera_time =
                        centers[lens] + (fraction - 0.5) * readout.seconds * 1_000_000.0;
                    poses.push(
                        self.attitude
                            .pose_at(self.relative_time(camera_time)?)?
                            .inverse()
                            * reference,
                    );
                }
                tables[lens] = Some(ReadoutPoseTable::new(
                    readout.direction,
                    readout.sensor_fraction,
                    poses,
                )?);
            } else if centers[lens] != centers[0] {
                // Preserve different lens exposure centers even when sensor
                // readout correction is disabled: this is a constant rotation.
                let relative = self
                    .attitude
                    .pose_at(self.relative_time(centers[lens])?)?
                    .inverse()
                    * reference;
                tables[lens] = Some(ReadoutPoseTable::new(
                    ReadoutDirection::TopToBottom,
                    [0.0, 1.0],
                    vec![relative; 2],
                )?);
            }
        }
        FrameMotion::new(target.inverse() * reference, tables)
    }

    pub(super) fn diagnostics(&self) -> String {
        self.report.clone()
    }
    pub(super) fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn frame_center_micros(&self, lens: usize, pts_micros: i64) -> Result<f64> {
        // Exposure clocks subtract the shared integer origin before any
        // fractional shutter/gyro timing is applied, preserving fine timing
        // even at the accepted absolute-camera timestamp boundaries.
        let (camera_micros, shutter) =
            self.clocks[lens].exposure_at(pts_micros, self.camera_origin_micros)?;
        Ok(camera_micros + self.gyro_adjust_micros - shutter.as_secs_f64() * 500_000.0)
    }

    fn relative_time(&self, camera_micros: f64) -> Result<Duration> {
        Duration::try_from_secs_f64(camera_micros / 1_000_000.0).map_err(|_| {
            Error::InvalidMedia(
                "frame capture time precedes the retained gyro clock or overflows".into(),
            )
        })
    }
}

fn unique_record(records: &[RecordInfo], id: u8) -> Result<Option<&RecordInfo>> {
    let mut records = records.iter().filter(|record| record.id == id);
    let first = records.next();
    if records.next().is_some() {
        return Err(Error::InvalidMedia(format!(
            "duplicate telemetry record {id} is ambiguous"
        )));
    }
    Ok(first)
}

fn gyro_adjustment_micros(metadata: &InsvMetadata) -> Result<f64> {
    let value = match metadata.has_gyro_timestamp_adjust {
        Some(true) => metadata.gyro_timestamp_adjust_ms.ok_or_else(|| {
            Error::MissingCalibration("recorded gyro timestamp adjustment is missing".into())
        })?,
        Some(false) => 0.0,
        None if metadata
            .gyro_timestamp_adjust_ms
            .is_none_or(|value| value == 0.0) =>
        {
            0.0
        }
        None => {
            return Err(Error::MissingCalibration(
                "gyro timestamp adjustment lacks its validity flag".into(),
            ))
        }
    };
    let micros = value * 1_000.0;
    if !micros.is_finite() || micros.abs() > i64::MAX as f64 {
        return Err(Error::InvalidMedia(
            "gyro timestamp adjustment is outside the supported range".into(),
        ));
    }
    Ok(micros)
}

fn validate_timing_metadata(metadata: &InsvMetadata) -> Result<()> {
    if let Some(field) = metadata.unknown_fields.iter().find(|field| {
        matches!(
            field.number,
            24 | 25 | 28 | 29 | 30 | 59 | 62 | 64 | 65 | 133
        )
    }) {
        return Err(Error::InvalidMedia(format!(
            "stabilization timing metadata tag {} is malformed or unsupported",
            field.number
        )));
    }
    if metadata
        .timelapse_interval
        .is_some_and(|interval| interval != 0.0)
        || metadata
            .timelapse_interval_ms
            .is_some_and(|interval| interval != 0)
    {
        return Err(Error::MissingCapability(
            "stabilization of timelapse capture requires its explicit camera-time mapper".into(),
        ));
    }
    if metadata
        .rolling_shutter_time
        .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        return Err(Error::InvalidMedia(
            "rolling-shutter timing metadata is invalid".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{CropWindow, FileRotation, ImuRange};
    use crate::motion::Orientation;
    use std::fs::File;
    use std::io::Cursor;

    const ORIGIN: i64 = 1_000_000;
    const FFT: i64 = 1_100_000;
    const YAW_COUNTS: i32 = 1_000;

    fn metadata() -> InsvMetadata {
        InsvMetadata {
            camera_name: Some("Insta360 X5".into()),
            is_raw_gyro: Some(true),
            first_frame_timestamp: Some(FFT),
            video_pts_map_type: Some(VideoPtsMapType::ReadingInExposureFile),
            has_gyro_timestamp_adjust: Some(true),
            gyro_timestamp_adjust_ms: Some(2.0),
            imu_range: Some(ImuRange {
                accelerometer_g: 32.0,
                gyroscope_degrees_per_second: 2_000.0,
            }),
            rolling_shutter_time: Some(10.0),
            file_rotation: Some(FileRotation::Degrees0),
            crop_window: Some(CropWindow {
                source_width: 100,
                source_height: 100,
                destination_width: 80,
                destination_height: 80,
                x_offset: 0,
                y_offset: 0,
                unknown_fields: Vec::new(),
            }),
            ..InsvMetadata::default()
        }
    }

    fn gyro() -> Vec<u8> {
        let mut data = Vec::new();
        for index in 0..=400 {
            data.extend_from_slice(&(ORIGIN + index * 1_000).to_le_bytes());
            // Raw +X specific force becomes projector body +Z; raw +X gyro
            // becomes +Z yaw after the complete gyrostab-to-projector bridge.
            for value in [1_024, 0, 0, YAW_COUNTS, 0, 0] {
                data.extend_from_slice(&((32_768 + value) as u16).to_le_bytes());
            }
        }
        data
    }

    fn exposures(delay: i64) -> Vec<CameraExposureSample> {
        [
            (1_090_000, 2),
            (FFT, 10),
            (1_130_000, 4),
            (1_170_000, 20),
            (1_220_000, 2),
        ]
        .into_iter()
        .map(|(timestamp, shutter)| CameraExposureSample {
            timestamp_micros: timestamp + delay,
            shutter_speed: Duration::from_millis(shutter),
        })
        .collect()
    }

    fn prepare(
        metadata: &InsvMetadata,
        config: &StitchConfig,
        secondary: Option<Vec<CameraExposureSample>>,
    ) -> Result<FileStabilizer> {
        FileStabilizer::from_payloads(
            metadata,
            config,
            &gyro(),
            exposures(0),
            secondary,
            vec![vec![0, 33_333, 100_000]; 2],
        )
    }

    fn yaw_rate() -> f64 {
        f64::from(YAW_COUNTS) * 2_000.0_f64.to_radians() / 32_768.0
    }

    fn assert_yaw(orientation: Orientation, radians: f64) {
        let ray = orientation.rotate_vector([1.0, 0.0, 0.0]);
        assert!(
            (ray[0] - radians.cos()).abs() < 1e-9,
            "{ray:?} vs {radians}"
        );
        assert!(
            (ray[1] - radians.sin()).abs() < 1e-9,
            "{ray:?} vs {radians}"
        );
        assert!(ray[2].abs() < 1e-9);
    }

    #[test]
    fn exposure_mapping_applies_offset_and_midpoint_once_and_retains_seek_anchor() {
        let config = StitchConfig {
            rolling_shutter: RollingShutterCorrection::Off,
            ..StitchConfig::default()
        };
        let stabilizer = prepare(&metadata(), &config, None).unwrap();
        // First center = FFT + 2 ms gyro delta - 10 ms shutter / 2.
        // Third VFR frame uses the recorded 1.17 s exposure, despite PTS=0.1 s.
        for (pts, expected_seconds) in [
            (100_000, 0.162),
            (0, 0.097),
            (33_333, 0.130),
            (100_000, 0.162),
        ] {
            let motion = stabilizer.frame_motion(pts).unwrap();
            assert_yaw(motion.correction(), yaw_rate() * expected_seconds);
            assert!(motion.readout().iter().all(Option::is_none));
        }
        assert!(stabilizer.frame_motion(-1).is_err());
        assert!(stabilizer.frame_motion(100_001).is_err());
    }

    #[test]
    fn readout_uses_active_resolution_and_centered_per_lens_camera_times() {
        let config = StitchConfig {
            rolling_shutter: RollingShutterCorrection::Required,
            ..StitchConfig::default()
        };
        let stabilizer = prepare(&metadata(), &config, Some(exposures(2_000))).unwrap();
        let motion = stabilizer.frame_motion(0).unwrap();
        for lens in 0..2 {
            let table = motion.readout()[lens].as_ref().unwrap();
            assert_eq!(table.direction(), ReadoutDirection::TopToBottom);
            assert_eq!(table.sensor_fraction(), [0.0, 1.0]);
            assert_eq!(table.poses().len(), READOUT_POSES);
            for fraction in [0.0, 0.1, 0.5, 0.9, 1.0] {
                assert_yaw(
                    table.rotation_at_fraction(fraction),
                    yaw_rate() * ((0.5 - fraction) * 0.010 - lens as f64 * 0.002),
                );
            }
        }
        assert_yaw(motion.correction(), yaw_rate() * 0.097);
    }

    #[test]
    fn track_order_and_secondary_global_capture_time_are_preserved_with_readout_off() {
        let config = StitchConfig {
            rolling_shutter: RollingShutterCorrection::Off,
            ..StitchConfig::default()
        };
        let mut metadata = metadata();
        for reverse in [false, true] {
            metadata.reverse_video_track_order = Some(reverse);
            let stabilizer = prepare(&metadata, &config, Some(exposures(2_000))).unwrap();
            let motion = stabilizer.frame_motion(0).unwrap();
            assert_yaw(
                motion.correction(),
                yaw_rate() * if reverse { 0.099 } else { 0.097 },
            );
            assert!(motion.readout()[0].is_none());
            let secondary = motion.readout()[1].as_ref().unwrap();
            assert_eq!(secondary.poses().len(), 2);
            assert_yaw(
                secondary.poses()[0],
                yaw_rate() * if reverse { 0.002 } else { -0.002 },
            );
        }
    }

    #[test]
    fn flowstate_preserves_yaw_while_direction_lock_fixes_world_heading() {
        let flowstate = StitchConfig {
            stabilization: Stabilization::FlowState,
            rolling_shutter: RollingShutterCorrection::Off,
            ..StitchConfig::default()
        };
        let stabilizer = prepare(&metadata(), &flowstate, None).unwrap();
        for pts in [0, 33_333, 100_000] {
            assert_yaw(stabilizer.frame_motion(pts).unwrap().correction(), 0.0);
        }
    }

    #[test]
    fn automatic_readout_reports_missing_geometry_and_required_readout_fails() {
        let mut metadata = metadata();
        metadata.crop_window = None;
        let automatic = prepare(&metadata, &StitchConfig::default(), None).unwrap();
        assert!(automatic.readout.is_none());
        assert!(automatic
            .warnings()
            .iter()
            .any(|warning| warning.contains("rolling-shutter correction omitted")));
        let required = StitchConfig {
            rolling_shutter: RollingShutterCorrection::Required,
            ..StitchConfig::default()
        };
        assert!(prepare(&metadata, &required, None).is_err());
    }

    #[test]
    fn decoder_pts_modes_use_camera_exposure_shutter_and_valid_adjustment_flag() {
        let config = StitchConfig {
            rolling_shutter: RollingShutterCorrection::Off,
            ..StitchConfig::default()
        };
        for mode in [
            VideoPtsMapType::Unknown,
            VideoPtsMapType::DecoderWithFirstFrameTimestamp,
        ] {
            let mut metadata = metadata();
            metadata.video_pts_map_type = Some(mode);
            metadata.has_gyro_timestamp_adjust = Some(false);
            let stabilizer = prepare(&metadata, &config, None).unwrap();
            // Decode time .1 s is camera1.2 s; last preceding exposure has20ms shutter.
            assert_yaw(
                stabilizer.frame_motion(100_000).unwrap().correction(),
                yaw_rate() * 0.190,
            );
        }
        let mut metadata = metadata();
        metadata.has_gyro_timestamp_adjust = None;
        assert!(prepare(&metadata, &config, None).is_err());
        metadata.video_pts_map_type = Some(VideoPtsMapType::Other(3));
        assert!(prepare(&metadata, &config, None).is_err());
    }

    #[test]
    fn leading_presentation_delay_does_not_change_capture_time_in_any_supported_mode() {
        let config = StitchConfig {
            rolling_shutter: RollingShutterCorrection::Off,
            ..StitchConfig::default()
        };
        for mode in [
            VideoPtsMapType::Unknown,
            VideoPtsMapType::DecoderWithFirstFrameTimestamp,
            VideoPtsMapType::ReadingInExposureFile,
        ] {
            let mut metadata = metadata();
            metadata.video_pts_map_type = Some(mode);
            let original = prepare(&metadata, &config, None).unwrap();
            let delayed = FileStabilizer::from_payloads(
                &metadata,
                &config,
                &gyro(),
                exposures(0),
                None,
                vec![vec![40_000, 73_333, 140_000]; 2],
            )
            .unwrap();
            for pts in [0, 33_333, 100_000] {
                assert_eq!(
                    original.frame_motion(pts).unwrap().correction(),
                    delayed.frame_motion(pts + 40_000).unwrap().correction()
                );
            }
        }
    }

    #[test]
    fn automatic_readout_does_not_hide_malformed_or_impossible_timing() {
        use crate::container::{UnknownMetadataField, UnknownMetadataValue};
        let mut invalid = metadata();
        invalid.crop_window = None;
        invalid.rolling_shutter_time = Some(100_000.0);
        assert!(matches!(
            prepare(&invalid, &StitchConfig::default(), None),
            Err(Error::InvalidMedia(_))
        ));
        for value in [f64::NAN, f64::INFINITY, -1.0] {
            invalid.rolling_shutter_time = Some(value);
            assert!(matches!(
                prepare(&invalid, &StitchConfig::default(), None),
                Err(Error::InvalidMedia(_))
            ));
        }
        for tag in [24, 25, 28, 29, 62, 64, 65] {
            let mut invalid = metadata();
            invalid.unknown_fields.push(UnknownMetadataField {
                number: tag,
                value: UnknownMetadataValue::LengthDelimited(vec![0]),
            });
            assert!(prepare(&invalid, &StitchConfig::default(), None).is_err());
        }
        let mut timelapse = metadata();
        timelapse.timelapse_interval_ms = Some(1_000);
        assert!(matches!(
            prepare(&timelapse, &StitchConfig::default(), None),
            Err(Error::MissingCapability(_))
        ));
    }

    #[test]
    fn zero_readout_keeps_global_motion_without_redundant_sensor_tables() {
        let mut metadata = metadata();
        metadata.rolling_shutter_time = Some(0.0);
        let config = StitchConfig {
            rolling_shutter: RollingShutterCorrection::Required,
            ..StitchConfig::default()
        };
        let stabilizer = prepare(&metadata, &config, None).unwrap();
        let motion = stabilizer.frame_motion(0).unwrap();
        assert_yaw(motion.correction(), yaw_rate() * 0.097);
        assert!(motion.readout().iter().all(Option::is_none));
    }

    #[test]
    fn large_camera_origins_preserve_fractional_capture_and_readout_times() {
        let limit = 1_i64 << 53;
        let config = StitchConfig {
            rolling_shutter: RollingShutterCorrection::Required,
            ..StitchConfig::default()
        };
        for mode in [
            VideoPtsMapType::DecoderWithFirstFrameTimestamp,
            VideoPtsMapType::ReadingInExposureFile,
        ] {
            let mut metadata = metadata();
            metadata.video_pts_map_type = Some(mode);
            let mut primary = exposures(0);
            primary[1].shutter_speed = Duration::from_nanos(10_000_501);
            let original = FileStabilizer::from_payloads(
                &metadata,
                &config,
                &gyro(),
                primary.clone(),
                None,
                vec![vec![0, 33_333, 100_000]; 2],
            )
            .unwrap();
            for origin in [-limit, limit - 500_000] {
                let shift = origin - ORIGIN;
                let mut shifted_metadata = metadata.clone();
                shifted_metadata.first_frame_timestamp = Some(FFT + shift);
                let mut shifted_gyro = gyro();
                for sample in shifted_gyro.chunks_exact_mut(20) {
                    let timestamp = i64::from_le_bytes(sample[..8].try_into().unwrap());
                    sample[..8].copy_from_slice(&(timestamp + shift).to_le_bytes());
                }
                let mut shifted_exposures = primary.clone();
                for sample in &mut shifted_exposures {
                    sample.timestamp_micros += shift;
                }
                let shifted = FileStabilizer::from_payloads(
                    &shifted_metadata,
                    &config,
                    &shifted_gyro,
                    shifted_exposures,
                    None,
                    vec![vec![0, 33_333, 100_000]; 2],
                )
                .unwrap();
                for pts in [0, 33_333, 60_000, 100_000] {
                    let expected = original.frame_motion(pts).unwrap();
                    let actual = shifted.frame_motion(pts).unwrap();
                    assert_eq!(actual.correction(), expected.correction());
                    for lens in 0..2 {
                        assert_eq!(
                            actual.readout()[lens].as_ref().unwrap().poses(),
                            expected.readout()[lens].as_ref().unwrap().poses()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn rejects_camera_clocks_outside_exact_integer_microseconds() {
        let limit = 1_i64 << 53;
        for timestamp in [-limit - 1, limit + 1, i64::MIN, i64::MAX] {
            let mut gyro_payload = gyro();
            gyro_payload[..8].copy_from_slice(&timestamp.to_le_bytes());
            let error = FileStabilizer::from_payloads(
                &metadata(),
                &StitchConfig::default(),
                &gyro_payload,
                exposures(0),
                None,
                vec![vec![0, 33_333, 100_000]; 2],
            )
            .err()
            .unwrap();
            assert!(error.to_string().contains("exact microsecond range"));
            let mut metadata = metadata();
            metadata.first_frame_timestamp = Some(timestamp);
            assert!(prepare(&metadata, &StitchConfig::default(), None).is_err());
        }
    }

    #[test]
    fn raw_saturation_nonmonotonic_preroll_and_large_gaps_fail_preparation() {
        let mut clipped = gyro();
        clipped[14..16].copy_from_slice(&0_u16.to_le_bytes());
        let mut nonmonotonic = gyro();
        nonmonotonic[20..28].copy_from_slice(&(ORIGIN - 1).to_le_bytes());
        let mut gap = gyro();
        gap.drain(100 * 20..250 * 20);
        for (payload, expected) in [
            (clipped, "saturation"),
            (nonmonotonic, "pre-roll"),
            (gap, "gap"),
        ] {
            let error = FileStabilizer::from_payloads(
                &metadata(),
                &StitchConfig::default(),
                &payload,
                exposures(0),
                None,
                vec![vec![0, 33_333, 100_000]; 2],
            )
            .err()
            .unwrap();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn disabled_stabilization_does_not_read_telemetry() {
        let mut reader = InsvReader::new(Cursor::new(Vec::<u8>::new())).unwrap();
        let inspection = InsvInspection {
            boxes: Vec::new(),
            records: Vec::new(),
            metadata: InsvMetadata::default(),
            video_tracks: Vec::new(),
            duration: None,
            fps: None,
            trailer: crate::TrailerInfo {
                offset: 0,
                size: 0,
                version: 0,
                record_count: 0,
            },
        };
        let mut config = StitchConfig {
            stabilization: Stabilization::Off,
            ..StitchConfig::default()
        };
        assert!(
            FileStabilizer::from_reader(&mut reader, &inspection, &config)
                .unwrap()
                .is_none()
        );
        config.rolling_shutter = RollingShutterCorrection::Required;
        assert!(FileStabilizer::from_reader(&mut reader, &inspection, &config).is_err());
    }

    #[test]
    fn prepares_external_x5_stabilization_when_configured() {
        let Ok(path) = std::env::var("INSTA360_RS_X5_SAMPLE") else {
            return;
        };
        let mut reader = InsvReader::new(File::open(path).unwrap()).unwrap();
        let inspection = reader.inspect().unwrap();
        let config = StitchConfig {
            rolling_shutter: RollingShutterCorrection::Required,
            ..StitchConfig::default()
        };
        let stabilizer = FileStabilizer::from_reader(&mut reader, &inspection, &config)
            .unwrap()
            .unwrap();
        assert!(stabilizer.diagnostics().contains("actual video PTS"));
        let late = stabilizer.frame_motion(1_330_562_567).unwrap();
        let first = stabilizer.frame_motion(0).unwrap();
        let late_again = stabilizer.frame_motion(1_330_562_567).unwrap();
        assert_eq!(late.correction(), late_again.correction());
        for motion in [first, late] {
            motion.correction().validate().unwrap();
            assert!(motion.readout().iter().all(Option::is_some));
        }
    }
}
