mod common;
#[path = "common/rolling_shutter.rs"]
mod rolling_shutter;

use insta360_rs::calibration::synthetic_dual_fisheye_calibration;
use insta360_rs::{
    CpuStitcher, EquirectangularProjection, Error, LensFrame, Orientation, PanoramaFrame,
    StitchEngine,
};

fn solid_lens(width: u32, height: u32, color: [u8; 3]) -> LensFrame {
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for _ in 0..width * height {
        rgb.extend_from_slice(&color);
    }
    LensFrame::new(width, height, rgb).expect("solid frame has the right size")
}

#[test]
fn validates_lens_and_panorama_storage() {
    let lens_error = LensFrame::new(4, 4, vec![0; 47]).expect_err("one byte is missing");
    assert!(matches!(lens_error, Error::InvalidMedia(_)));

    let panorama_error =
        PanoramaFrame::new(8, 8, vec![0; 8 * 8 * 3]).expect_err("panorama is not 2:1");
    assert!(matches!(panorama_error, Error::InvalidMedia(_)));
}

#[test]
fn oversized_panorama_returns_an_error_without_panicking() {
    let lenses = [solid_lens(2, 2, [10; 3]), solid_lens(2, 2, [20; 3])];
    let calibration = synthetic_dual_fisheye_calibration(2, 2).unwrap();
    // RGB byte count fits 64-bit usize but exceeds Rust's isize allocation
    // limit. This deterministically rejects without requesting physical RAM.
    let error = CpuStitcher::new()
        .stitch(
            &lenses,
            &calibration,
            EquirectangularProjection {
                width: 3_000_000_000,
                height: 1_500_000_000,
            },
        )
        .expect_err("oversized panorama");
    assert!(matches!(error, Error::InvalidMedia(_)));
}

#[test]
fn stitches_opposing_solid_lenses_into_a_complete_panorama() {
    let lenses = [
        solid_lens(64, 64, [255, 0, 0]),
        solid_lens(64, 64, [0, 0, 255]),
    ];
    let calibration =
        synthetic_dual_fisheye_calibration(64, 64).expect("synthetic calibration is valid");

    let panorama = CpuStitcher::new()
        .stitch(
            &lenses,
            &calibration,
            EquirectangularProjection {
                width: 128,
                height: 64,
            },
        )
        .expect("synthetic pair should stitch");

    assert_eq!(panorama.width(), 128);
    assert_eq!(panorama.height(), 64);
    assert_eq!(panorama.as_rgb8().len(), 128 * 64 * 3);
    assert!(panorama
        .as_rgb8()
        .chunks_exact(3)
        .all(|pixel| pixel[0] > 0 || pixel[2] > 0));
    assert!(panorama
        .as_rgb8()
        .chunks_exact(3)
        .any(|pixel| pixel[0] > 0 && pixel[2] > 0));
}

#[test]
fn parallel_stitching_is_byte_deterministic() {
    let lenses = [
        solid_lens(48, 48, [20, 100, 220]),
        solid_lens(48, 48, [240, 80, 10]),
    ];
    let calibration =
        synthetic_dual_fisheye_calibration(48, 48).expect("synthetic calibration is valid");
    let projection = EquirectangularProjection {
        width: 96,
        height: 48,
    };
    let stitcher = CpuStitcher::new();

    let first = stitcher
        .stitch(&lenses, &calibration, projection)
        .expect("first stitch should succeed");
    let second = stitcher
        .stitch(&lenses, &calibration, projection)
        .expect("second stitch should succeed");

    assert_eq!(first, second);
}

#[test]
fn bilinear_sampling_produces_intermediate_values() {
    let width = 32;
    let height = 32;
    let mut gradient = Vec::with_capacity(width * height * 3);
    for _row in 0..height {
        for column in 0..width {
            let value = (column * 255 / (width - 1)) as u8;
            gradient.extend_from_slice(&[value, value, value]);
        }
    }
    let lenses = [
        LensFrame::new(width as u32, height as u32, gradient.clone()).expect("valid gradient"),
        LensFrame::new(width as u32, height as u32, gradient).expect("valid gradient"),
    ];
    let calibration = synthetic_dual_fisheye_calibration(width as u32, height as u32)
        .expect("synthetic calibration is valid");

    let panorama = CpuStitcher::new()
        .stitch(
            &lenses,
            &calibration,
            EquirectangularProjection {
                width: 64,
                height: 32,
            },
        )
        .expect("gradient should stitch");

    assert!(panorama
        .as_rgb8()
        .chunks_exact(3)
        .any(|pixel| pixel[0] != 0 && pixel[0] != 255));
}

#[test]
fn rejects_invalid_feather_fraction() {
    assert!(CpuStitcher::with_feather_fraction(f64::NAN).is_err());
    assert!(CpuStitcher::with_feather_fraction(0.51).is_err());
    assert!(CpuStitcher::with_feather_fraction(0.0).is_ok());
}

#[test]
fn masked_source_pixels_do_not_change_cpu_blending() {
    let calibration = common::x5_v6_underwater_calibration(128, 128);
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let stitcher = CpuStitcher::new();
    let render = |exterior| {
        stitcher
            .stitch(
                &common::x5_underwater_lenses_with_exterior(exterior),
                &calibration,
                projection,
            )
            .expect("masked X5 stitch")
    };
    let black_exterior = render(0);
    let white_exterior = render(255);
    assert!(black_exterior
        .as_rgb8()
        .iter()
        .all(|channel| *channel == 100));
    assert_eq!(black_exterior, white_exterior);
}

#[test]
fn underwater_lower_housing_does_not_enter_cpu_panorama() {
    let calibration = common::x5_v6_underwater_calibration(512, 512);
    let projection = EquirectangularProjection {
        width: 512,
        height: 256,
    };
    let stitcher = CpuStitcher::new();
    let render = |housing| {
        stitcher
            .stitch(
                &common::x5_underwater_lenses_with_lower_housing(housing),
                &calibration,
                projection,
            )
            .expect("underwater housing stitch")
    };
    let black = render(0);
    let white = render(255);
    assert!(black.as_rgb8().iter().all(|channel| *channel == 100));
    assert_eq!(black, white);
}

#[test]
fn gyro_orientation_rotates_the_panorama_without_changing_geometry() {
    let lenses = [
        solid_lens(64, 64, [255, 0, 0]),
        solid_lens(64, 64, [0, 0, 255]),
    ];
    let calibration =
        synthetic_dual_fisheye_calibration(64, 64).expect("synthetic calibration is valid");
    let projection = EquirectangularProjection {
        width: 128,
        height: 64,
    };
    let stitcher = CpuStitcher::new();
    let identity = stitcher
        .stitch(&lenses, &calibration, projection)
        .expect("identity stitch");
    let identity_oriented = stitcher
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("oriented identity stitch");
    assert_eq!(identity, identity_oriented);

    let turned = stitcher
        .stitch_with_orientation(
            &lenses,
            &calibration,
            projection,
            Orientation::from_axis_angle([0.0, 1.0, 0.0], std::f64::consts::PI).expect("half turn"),
        )
        .expect("turned stitch");
    let center = ((projection.height / 2 * projection.width + projection.width / 2) * 3) as usize;
    assert!(identity.as_rgb8()[center] > identity.as_rgb8()[center + 2]);
    assert!(turned.as_rgb8()[center + 2] > turned.as_rgb8()[center]);
}

#[test]
fn color_compensation_keeps_bright_lens_hemispheres_visible() {
    let calibration = common::x5_v6_underwater_calibration(128, 128);
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    for levels in [[22, 220], [220, 22]] {
        let lenses = levels.map(|level| solid_lens(128, 128, [level; 3]));
        let panorama = CpuStitcher::new()
            .stitch(&lenses, &calibration, projection)
            .expect("unequal exposures stitch");
        // Both source images have a strictly positive constant signal. A
        // correction ramp must approach one at its own optical axis; extending
        // a body-latitude ramp into the opposite hemisphere made black wedges.
        let minimum = *panorama.as_rgb8().iter().min().expect("pixels");
        assert!(
            minimum >= 20,
            "color compensation erased a valid ray: {minimum}"
        );
        for (column, level) in [(127, levels[0]), (255, levels[1])] {
            let pixel = ((64 * projection.width + column) * 3) as usize;
            assert_eq!(
                panorama.as_rgb8()[pixel],
                level,
                "optical axis must stay neutral"
            );
        }
    }
}

#[test]
fn color_compensation_rotates_with_the_camera() {
    let lenses = [
        solid_lens(128, 128, [60; 3]),
        solid_lens(128, 128, [180; 3]),
    ];
    let calibration = common::x5_v6_underwater_calibration(128, 128);
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let stitcher = CpuStitcher::new();
    let baseline = stitcher
        .stitch(&lenses, &calibration, projection)
        .expect("baseline");
    let rotated = stitcher
        .stitch_with_orientation(
            &lenses,
            &calibration,
            projection,
            Orientation::from_axis_angle([1.0, 0.0, 0.0], std::f64::consts::PI).expect("half turn"),
        )
        .expect("rotated panorama");
    // A half turn about X reverses both panorama axes exactly at pixel centers.
    // Exposure correction must follow the same scene points as the geometry.
    let maximum = baseline
        .as_rgb8()
        .chunks_exact(3)
        .rev()
        .zip(rotated.as_rgb8().chunks_exact(3))
        .flat_map(|(a, b)| a.iter().zip(b).map(|(a, b)| a.abs_diff(*b)))
        .max()
        .expect("pixels");
    assert!(
        maximum <= 1,
        "rotation changed scene colors by {maximum} DN"
    );
}

#[test]
fn rolling_shutter_recovers_forward_generated_sensor_geometry() {
    let stitcher = CpuStitcher::new();
    for columns in [false, true] {
        for (reference, target) in [
            ([0.0; 3], [0.0; 3]),
            ([17.0, -23.0, 41.0], [0.0, 0.0, 29.0]),
        ] {
            let chart = rolling_shutter::chart(rolling_shutter::scans(columns), reference, target);
            let corrected = stitcher
                .stitch_with_motion(
                    &chart.lenses,
                    &chart.calibration,
                    chart.projection,
                    &chart.motion,
                )
                .expect("corrected sensor geometry");
            let uncorrected = stitcher
                .stitch_with_orientation(
                    &chart.lenses,
                    &chart.calibration,
                    chart.projection,
                    chart.motion.correction(),
                )
                .expect("global correction only");
            let (corrected_mean, corrected_max) =
                rolling_shutter::error(&corrected, &chart.expected);
            let (uncorrected_mean, _) = rolling_shutter::error(&uncorrected, &chart.expected);
            assert!(corrected_mean < 0.3 && corrected_max <= 2, "forward world-ray error: mean {corrected_mean}, max {corrected_max}, columns {columns}");
            assert!(uncorrected_mean > 1.0 && corrected_mean * 8.0 < uncorrected_mean, "correction must remove measured skew: corrected {corrected_mean}, uncorrected {uncorrected_mean}");
        }
    }
}

#[test]
fn zero_readout_motion_is_byte_identical_to_global_stabilization() {
    for columns in [false, true] {
        let scans = rolling_shutter::scans(columns).map(|scan| rolling_shutter::Scan {
            angle: 0.0,
            phase: 0.0,
            ..scan
        });
        let chart = rolling_shutter::chart(scans, [17.0, -23.0, 41.0], [0.0, 0.0, 29.0]);
        let stitcher = CpuStitcher::new();
        let corrected = stitcher
            .stitch_with_motion(
                &chart.lenses,
                &chart.calibration,
                chart.projection,
                &chart.motion,
            )
            .expect("identity readout");
        let global = stitcher
            .stitch_with_orientation(
                &chart.lenses,
                &chart.calibration,
                chart.projection,
                chart.motion.correction(),
            )
            .expect("global pose");
        assert_eq!(corrected, global);
    }
}

#[test]
fn rolling_shutter_and_stabilization_preserve_masked_source_isolation() {
    use insta360_rs::motion::{FrameMotion, ReadoutDirection, ReadoutPoseTable};
    let table = |angle: f64| {
        ReadoutPoseTable::new(
            ReadoutDirection::BottomToTop,
            [0.12, 0.91],
            vec![
                Orientation::from_axis_angle([0.3, -0.6, 0.7], -angle).expect("start"),
                Orientation::from_axis_angle([0.3, -0.6, 0.7], angle).expect("end"),
            ],
        )
        .expect("readout")
    };
    let motion = FrameMotion::new(
        Orientation::from_euler_degrees(13.0, -21.0, 37.0).expect("reference"),
        // The native bottom cutout leaves one degree of total overlap. Keep
        // opposing scan poses within that coverage while testing isolation.
        [Some(table(0.002)), Some(table(-0.003))],
    )
    .expect("frame motion");
    let calibration = common::x5_v6_underwater_calibration(128, 128);
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let stitcher = CpuStitcher::new();
    let render = |exterior| {
        stitcher
            .stitch_with_motion(
                &common::x5_underwater_lenses_with_exterior(exterior),
                &calibration,
                projection,
                &motion,
            )
            .expect("masked motion rendering")
    };
    let black = render(0);
    let white = render(255);
    assert_eq!(black, white);
    assert!(black.as_rgb8().iter().all(|channel| *channel == 100));
}

#[test]
fn native_readout_clips_lens_coverage_at_capture_time_after_rotation() {
    use insta360_rs::motion::{FrameMotion, ReadoutDirection, ReadoutPoseTable};
    let calibration = common::x5_v6_underwater_calibration(128, 128);
    let lenses = [
        solid_lens(128, 128, [100; 3]),
        solid_lens(128, 128, [100; 3]),
    ];
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let stitcher = CpuStitcher::new();
    let correction = Orientation::from_euler_degrees(13.0, -21.0, 37.0).expect("reference pose");
    for axis in [[0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
        let capture =
            Orientation::from_axis_angle(axis, 0.3).expect("capture relative to reference");
        let table = ReadoutPoseTable::new(
            ReadoutDirection::BottomToTop,
            [0.12, 0.91],
            vec![capture; 2],
        )
        .expect("constant readout");
        let motion = FrameMotion::new(correction, [Some(table.clone()), Some(table)])
            .expect("constant sensor offset");
        let actual = stitcher
            .stitch_with_motion(&lenses, &calibration, projection, &motion)
            .expect("native capture projection");
        let equivalent_global = stitcher
            .stitch_with_orientation(
                &lenses,
                &calibration,
                projection,
                correction * capture.inverse(),
            )
            .expect("equivalent global projection");
        // Applying native FOV clipping before the sensor-time rotation used to
        // discard newly visible rays, leaving thousands of black panorama pixels.
        assert_eq!(actual, equivalent_global);
        assert!(actual.as_rgb8().iter().all(|channel| *channel == 100));

        let varying = ReadoutPoseTable::new(
            ReadoutDirection::TopToBottom,
            [0.0, 1.0],
            (0..=64)
                .map(|index| {
                    Orientation::from_axis_angle(axis, 0.3 + (index as f64 / 64.0 - 0.5) * 0.02)
                        .expect("varying capture pose")
                })
                .collect(),
        )
        .expect("varying readout");
        let motion = FrameMotion::new(correction, [Some(varying.clone()), Some(varying)])
            .expect("varying sensor offset");
        let actual = stitcher
            .stitch_with_motion(&lenses, &calibration, projection, &motion)
            .expect("native varying capture projection");
        // This small scan variation is narrower than the established native
        // lens overlap, so every world ray retains a visible captured sample.
        assert!(actual.as_rgb8().iter().all(|channel| *channel == 100));
    }
}
