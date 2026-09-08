use std::f64::consts::{FRAC_PI_2, PI};
use std::time::Duration;

use insta360_rs::container::{ImuRange, InsvMetadata, VideoPtsMapType};
use insta360_rs::motion::decode_motion_record;
use insta360_rs::{Error, MotionSample, Orientation, Stabilization, Stabilizer};

fn assert_vector_close(actual: [f64; 3], expected: [f64; 3], tolerance: f64) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, received {actual}"
        );
    }
}

fn yaw_samples() -> Vec<MotionSample> {
    [0, 1, 2]
        .into_iter()
        .map(|seconds| {
            MotionSample::new(
                Duration::from_secs(seconds),
                [0.0, -9.81, 0.0],
                [0.0, FRAC_PI_2, 0.0],
            )
            .expect("sample is finite")
        })
        .collect()
}

#[test]
fn axis_angle_rotates_and_inverts_vectors() {
    let orientation =
        Orientation::from_axis_angle([0.0, 1.0, 0.0], FRAC_PI_2).expect("valid axis angle");
    let rotated = orientation.rotate_vector([0.0, 0.0, 1.0]);
    assert_vector_close(rotated, [1.0, 0.0, 0.0], 1.0e-12);
    assert_vector_close(
        orientation.inverse().rotate_vector(rotated),
        [0.0, 0.0, 1.0],
        1.0e-12,
    );
}

#[test]
fn finite_large_quaternions_and_axes_keep_their_rotation() {
    let rotation = Orientation::new(f64::MAX, 0.0, f64::MAX, 0.0).expect("finite quaternion");
    rotation
        .validate()
        .expect("normalized even when the unscaled norm overflows");
    assert_vector_close(
        rotation.rotate_vector([0.0, 0.0, 1.0]),
        [1.0, 0.0, 0.0],
        1e-12,
    );
    let rotation =
        Orientation::from_axis_angle([f64::MAX, f64::MAX, 0.0], PI).expect("finite axis");
    assert_vector_close(
        rotation.rotate_vector([1.0, 0.0, 0.0]),
        [0.0, 1.0, 0.0],
        1e-12,
    );
    Orientation::from_euler_degrees(f64::MAX, 0.0, 0.0)
        .expect("finite degrees")
        .validate()
        .unwrap();
}

#[test]
fn relative_track_retains_full_turns_between_samples() {
    for turns in [-2.0, -1.0, 0.75, 1.0, 2.0] {
        let rate = turns * 2.0 * PI;
        let samples = [Duration::ZERO, Duration::from_secs(1)]
            .map(|timestamp| MotionSample::new(timestamp, [0.0; 3], [0.0, rate, 0.0]).unwrap());
        let track = Stabilizer::new(&samples, Stabilization::DirectionLock).unwrap();
        for millis in [125, 250, 500, 750, 875, 1000] {
            let angle = rate * millis as f64 / 1000.0;
            let time = Duration::from_millis(millis);
            assert_vector_close(
                track.orientation_at(time).rotate_vector([0.0, 0.0, 1.0]),
                [angle.sin(), 0.0, angle.cos()],
                2e-8,
            );
            assert_vector_close(
                (track.correction_at(time) * track.orientation_at(time))
                    .rotate_vector([0.0, 0.0, 1.0]),
                [0.0, 0.0, 1.0],
                1e-12,
            );
        }
    }
}

#[test]
fn relative_track_rejects_unbounded_winding_without_allocating() {
    for rate in [1e20, f64::MAX] {
        let samples = [Duration::ZERO, Duration::from_secs(1)]
            .map(|timestamp| MotionSample::new(timestamp, [0.0; 3], [0.0, rate, 0.0]).unwrap());
        assert!(Stabilizer::new(&samples, Stabilization::DirectionLock).is_err());
    }
}

#[test]
fn integrates_angular_velocity_and_interpolates_orientation() {
    let stabilizer =
        Stabilizer::new(&yaw_samples(), Stabilization::Off).expect("ordered gyro should integrate");

    let halfway = stabilizer.orientation_at(Duration::from_millis(500));
    let forward = halfway.rotate_vector([0.0, 0.0, 1.0]);
    let half_sqrt = 0.5_f64.sqrt();
    assert_vector_close(forward, [half_sqrt, 0.0, half_sqrt], 1.0e-10);

    let final_forward = stabilizer
        .orientation_at(Duration::from_secs(2))
        .rotate_vector([0.0, 0.0, 1.0]);
    assert_vector_close(final_forward, [0.0, 0.0, -1.0], 1.0e-10);
}

#[test]
fn direction_lock_cancels_integrated_camera_rotation() {
    let stabilizer = Stabilizer::new(&yaw_samples(), Stabilization::DirectionLock)
        .expect("ordered gyro should integrate");
    let camera = stabilizer.orientation_at(Duration::from_secs(1));
    let correction = stabilizer.correction_at(Duration::from_secs(1));
    let stabilized = correction.rotate_vector(camera.rotate_vector([0.0, 0.0, 1.0]));

    assert_vector_close(stabilized, [0.0, 0.0, 1.0], 1.0e-10);
}

#[test]
fn flowstate_preserves_yaw_while_removing_tilt() {
    let initial =
        Orientation::from_axis_angle([1.0, 0.0, 0.0], PI / 6.0).expect("valid initial tilt");
    let still_samples = [
        MotionSample::new(Duration::ZERO, [0.0; 3], [0.0; 3]).expect("finite sample"),
        MotionSample::new(Duration::from_secs(1), [0.0; 3], [0.0; 3]).expect("finite sample"),
    ];
    let stabilizer =
        Stabilizer::with_initial_orientation(&still_samples, Stabilization::FlowState, initial)
            .expect("valid motion");
    let camera = stabilizer.orientation_at(Duration::ZERO);
    let corrected = stabilizer.correction_at(Duration::ZERO) * camera;

    assert_vector_close(
        corrected.rotate_vector([0.0, 0.0, 1.0]),
        [0.0, 0.0, 1.0],
        1.0e-10,
    );
}

#[test]
fn rejects_invalid_samples_and_timestamp_order() {
    assert!(MotionSample::new(Duration::ZERO, [f64::NAN, 0.0, 0.0], [0.0; 3]).is_err());

    let duplicate = [
        MotionSample::new(Duration::ZERO, [0.0; 3], [0.0; 3]).expect("finite sample"),
        MotionSample::new(Duration::ZERO, [0.0; 3], [0.0; 3]).expect("finite sample"),
    ];
    let error = Stabilizer::new(&duplicate, Stabilization::DirectionLock)
        .expect_err("duplicate timestamps are ambiguous");
    assert!(matches!(error, Error::InvalidMedia(_)));
}

#[test]
fn decodes_x5_raw_imu_ranges_and_aligns_to_the_first_frame() {
    let mut record = Vec::new();
    raw_sample(&mut record, 999_000, [32_768; 3], [32_768; 3]);
    raw_sample(
        &mut record,
        1_002_000,
        [49_152, 16_384, 32_768],
        [49_152, 16_384, 32_768],
    );
    let metadata = InsvMetadata {
        is_raw_gyro: Some(true),
        first_frame_timestamp: Some(1_000_000),
        gyro_timestamp_adjust_ms: Some(1.0),
        imu_range: Some(ImuRange {
            accelerometer_g: 32.0,
            gyroscope_degrees_per_second: 2_000.0,
        }),
        ..InsvMetadata::default()
    };

    let samples = decode_motion_record(&record, &metadata).expect("raw gyro");

    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].timestamp, Duration::from_millis(1));
    assert_eq!(samples[0].acceleration, [16.0, -16.0, 0.0]);
    let expected = 1_000.0_f64.to_radians();
    assert!((samples[0].angular_velocity[0] - expected).abs() < 1.0e-12);
    assert!((samples[0].angular_velocity[1] + expected).abs() < 1.0e-12);
    assert_eq!(samples[0].angular_velocity[2], 0.0);
}

#[test]
fn decodes_legacy_common_gyro_records() {
    let mut record = Vec::new();
    common_sample(&mut record, 10, [0.0, 1.0, 0.0], [0.1, 0.2, 0.3]);
    common_sample(&mut record, 12, [0.0, 1.0, 0.0], [0.4, 0.5, 0.6]);
    let metadata = InsvMetadata {
        is_raw_gyro: Some(false),
        first_frame_timestamp: Some(10),
        ..InsvMetadata::default()
    };

    let samples = decode_motion_record(&record, &metadata).expect("common gyro");

    assert_eq!(samples.len(), 2);
    assert_eq!(samples[1].timestamp, Duration::from_millis(2));
    assert_eq!(samples[1].acceleration, [0.0, 1.0, 0.0]);
    assert_eq!(samples[1].angular_velocity, [0.4, 0.5, 0.6]);
}

#[test]
fn legacy_decoder_requires_the_separate_exposure_timeline_for_mode_two() {
    let mut record = Vec::new();
    raw_sample(&mut record, 1_000_000, [32_768; 3], [32_768; 3]);
    let metadata = InsvMetadata {
        is_raw_gyro: Some(true),
        video_pts_map_type: Some(VideoPtsMapType::ReadingInExposureFile),
        ..InsvMetadata::default()
    };

    let error = decode_motion_record(&record, &metadata)
        .expect_err("exposure-file mapping must not use first-frame alignment");
    assert!(matches!(error, Error::MissingCapability(_)));
    assert!(error.to_string().contains("exposure-file PTS mapping"));
}

#[test]
fn accepts_decoder_first_frame_pts_mapping() {
    let mut record = Vec::new();
    raw_sample(&mut record, 1_000_000, [32_768; 3], [32_768; 3]);
    let metadata = InsvMetadata {
        is_raw_gyro: Some(true),
        first_frame_timestamp: Some(1_000_000),
        video_pts_map_type: Some(VideoPtsMapType::DecoderWithFirstFrameTimestamp),
        ..InsvMetadata::default()
    };

    let samples = decode_motion_record(&record, &metadata).expect("supported PTS mapping");
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].timestamp, Duration::ZERO);
}

fn raw_sample(
    output: &mut Vec<u8>,
    timestamp_us: i64,
    acceleration: [u16; 3],
    gyroscope: [u16; 3],
) {
    output.extend_from_slice(&timestamp_us.to_le_bytes());
    for value in acceleration.into_iter().chain(gyroscope) {
        output.extend_from_slice(&value.to_le_bytes());
    }
}

fn common_sample(
    output: &mut Vec<u8>,
    timestamp_ms: i64,
    acceleration: [f64; 3],
    gyroscope: [f64; 3],
) {
    output.extend_from_slice(&timestamp_ms.to_le_bytes());
    for value in acceleration.into_iter().chain(gyroscope) {
        output.extend_from_slice(&value.to_le_bytes());
    }
}
