use std::time::Duration;

use insta360_rs::container::InsvMetadata;
use insta360_rs::telemetry::{decode_camera_exposure_record, decode_exposure_record};
use insta360_rs::timing::ExposureTimeline;

#[test]
fn decodes_x5_exposure_timestamps_relative_to_video() {
    let mut record = Vec::new();
    exposure(&mut record, 900_000, 1.0 / 100.0);
    exposure(&mut record, 1_033_333, 1.0 / 150.0);
    let metadata = InsvMetadata {
        is_raw_gyro: Some(true),
        first_frame_timestamp: Some(1_000_000),
        ..InsvMetadata::default()
    };

    let samples = decode_exposure_record(&record, &metadata).expect("exposure");

    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].timestamp, Duration::from_micros(33_333));
    assert_eq!(
        samples[0].shutter_speed,
        Duration::from_secs_f64(1.0 / 150.0)
    );
}

#[test]
fn rejects_invalid_exposure_layouts() {
    let metadata = InsvMetadata {
        is_raw_gyro: Some(false),
        ..InsvMetadata::default()
    };
    assert!(decode_exposure_record(&[0; 15], &metadata).is_err());
}

#[test]
fn camera_exposure_decoder_retains_preroll_and_converts_legacy_units() {
    for (raw, multiplier) in [(true, 1), (false, 1_000)] {
        let mut record = Vec::new();
        exposure(&mut record, -10, 0.002);
        exposure(&mut record, 40, 0.004);
        let metadata = InsvMetadata {
            is_raw_gyro: Some(raw),
            first_frame_timestamp: Some(40),
            ..InsvMetadata::default()
        };
        let samples = decode_camera_exposure_record(&record, &metadata).expect("camera clock");
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].timestamp_micros, -10 * multiplier);
        assert_eq!(samples[1].timestamp_micros, 40 * multiplier);
        assert_eq!(samples[0].shutter_speed, Duration::from_millis(2));
    }
}

#[test]
fn camera_exposure_decoder_validates_preroll_and_clock_units() {
    let metadata = InsvMetadata {
        is_raw_gyro: Some(true),
        first_frame_timestamp: Some(100),
        ..InsvMetadata::default()
    };
    for shutter in [f64::NAN, f64::INFINITY, -0.001, f64::MAX] {
        let mut record = Vec::new();
        exposure(&mut record, 10, shutter);
        assert!(decode_camera_exposure_record(&record, &metadata).is_err());
    }
    for timestamps in [[10, 10], [10, 9]] {
        let mut record = Vec::new();
        for timestamp in timestamps {
            exposure(&mut record, timestamp, 0.001);
        }
        assert!(decode_camera_exposure_record(&record, &metadata).is_err());
    }
    let mut record = Vec::new();
    exposure(&mut record, i64::MAX, 0.001);
    assert!(decode_camera_exposure_record(&record, &InsvMetadata::default()).is_err());
    assert!(decode_camera_exposure_record(
        &record,
        &InsvMetadata {
            is_raw_gyro: Some(false),
            ..InsvMetadata::default()
        }
    )
    .is_err());
    assert!(decode_camera_exposure_record(&record[..15], &metadata).is_err());
}

#[test]
fn exposure_timeline_uses_actual_vfr_pts_and_camera_clock_drift() {
    let samples = camera_exposures(&[
        (900_000, 1),
        (1_000_000, 2),
        (1_033_330, 3),
        (1_099_990, 4),
        (1_133_320, 5),
        (1_166_650, 6),
    ]);
    let pts = [5_000_000, 5_033_333, 5_100_000, 5_133_333];
    let timeline = ExposureTimeline::new(&samples, 1_000_000, &pts).expect("timeline");
    // A seek recovers the same absolute frame without counting from the seek point.
    for (index, expected) in [(3, 1_133_320.0), (0, 1_000_000.0), (2, 1_099_990.0)] {
        assert_eq!(
            timeline.timestamp_micros_at_pts(pts[index]).unwrap(),
            expected
        );
    }
    assert_eq!(
        timeline.timestamp_micros_at_pts(5_060_000).unwrap(),
        1_033_330.0 + 26_667.0 / 66_667.0 * 66_660.0
    );
    assert_eq!(
        timeline.shutter_speed_at_pts(5_060_000).unwrap(),
        Duration::from_millis(3)
    );
    assert_eq!(
        timeline.shutter_speed_at_pts(5_100_000).unwrap(),
        Duration::from_millis(4)
    );
    assert!(timeline.timestamp_micros_at_pts(pts[0] - 1).is_err());
    assert!(timeline.timestamp_micros_at_pts(pts[3] + 1).is_err());
}

#[test]
fn exposure_timeline_anchors_to_nearest_fft_with_earlier_ties() {
    let samples = camera_exposures(&[(100, 1), (200, 2), (300, 3), (400, 4)]);
    for (fft, expected) in [(50, 100.0), (150, 100.0), (151, 200.0), (200, 200.0)] {
        let timeline = ExposureTimeline::new(&samples, fft, &[0, 10]).unwrap();
        assert_eq!(timeline.timestamp_micros_at_pts(0).unwrap(), expected);
        assert_eq!(
            timeline.timestamp_micros_at_pts(10).unwrap(),
            expected + 100.0
        );
    }
    let timeline = ExposureTimeline::new(&samples, 500, &[0]).unwrap();
    assert_eq!(timeline.timestamp_micros_at_pts(0).unwrap(), 400.0);
}

#[test]
fn exposure_timeline_rejects_incomplete_or_ambiguous_correspondence() {
    let samples = camera_exposures(&[(100, 1), (200, 2), (300, 3)]);
    for pts in [vec![], vec![0, 0], vec![10, 0], vec![0, 10, 20, 30]] {
        assert!(ExposureTimeline::new(&samples, 100, &pts).is_err());
    }
    assert!(ExposureTimeline::new(&samples, 200, &[0, 10, 20]).is_err());
    assert!(ExposureTimeline::new(&[], 100, &[0]).is_err());
    assert!(ExposureTimeline::new(&[samples[1], samples[0]], 100, &[0]).is_err());
}

#[test]
fn exposure_timeline_preserves_integer_microseconds_at_both_float_boundaries() {
    let limit = 1_i64 << 53;
    for start in [-limit, limit - 2] {
        let samples = camera_exposures(&[(start, 1), (start + 1, 1), (start + 2, 1)]);
        let timeline = ExposureTimeline::new(&samples, start, &[0, 1, 2]).unwrap();
        for index in 0..3 {
            assert_eq!(
                timeline.timestamp_micros_at_pts(index).unwrap(),
                (start + index) as f64
            );
        }
    }
    for timestamp in [-limit - 1, limit + 1, i64::MIN, i64::MAX] {
        let samples = camera_exposures(&[(timestamp, 1)]);
        assert!(ExposureTimeline::new(&samples, 0, &[0]).is_err());
        assert!(ExposureTimeline::new(&camera_exposures(&[(0, 1)]), timestamp, &[0]).is_err());
    }
}

fn camera_exposures(entries: &[(i64, u64)]) -> Vec<insta360_rs::telemetry::CameraExposureSample> {
    entries
        .iter()
        .map(
            |&(timestamp_micros, shutter_millis)| insta360_rs::telemetry::CameraExposureSample {
                timestamp_micros,
                shutter_speed: Duration::from_millis(shutter_millis),
            },
        )
        .collect()
}

fn exposure(output: &mut Vec<u8>, timestamp: i64, shutter_seconds: f64) {
    output.extend_from_slice(&timestamp.to_le_bytes());
    output.extend_from_slice(&shutter_seconds.to_le_bytes());
}
