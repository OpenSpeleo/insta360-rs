#![cfg(feature = "gpu")]

use insta360_rs::calibration::synthetic_dual_fisheye_calibration;
use insta360_rs::{CpuStitcher, EquirectangularProjection, LensFrame, Orientation};

mod common;
#[path = "common/rolling_shutter.rs"]
mod rolling_shutter;

use common::x5_v6_underwater_calibration;

fn gpu_available_or_skip() -> bool {
    let available = !insta360_rs::gpu::available_adapters().is_empty();
    if !available && std::env::var_os("INSTA360_RS_REQUIRE_GPU").is_some() {
        panic!("INSTA360_RS_REQUIRE_GPU is set but wgpu found no compatible adapter");
    }
    available
}

fn software_vulkan(stitcher: &insta360_rs::gpu::GpuStitcher) -> bool {
    let adapter = stitcher.adapter_info();
    adapter.backend == "Vulkan" && adapter.device_type == "Cpu"
}

#[test]
fn gpu_rejects_calibration_that_cannot_be_represented_by_shader_parameters() {
    if !gpu_available_or_skip() {
        return;
    }
    let gpu = insta360_rs::gpu::GpuStitcher::new().unwrap();
    let lenses = [
        LensFrame::new(2, 2, vec![100; 12]).unwrap(),
        LensFrame::new(2, 2, vec![100; 12]).unwrap(),
    ];
    let mut calibration = synthetic_dual_fisheye_calibration(2, 2).unwrap();
    for focal in [f64::MAX, f64::MIN_POSITIVE] {
        calibration.lenses[0].fx = focal;
        calibration
            .validate_for_stitching()
            .expect("usable f64 calibration");
        let error = gpu
            .stitch_with_orientation(
                &lenses,
                &calibration,
                EquirectangularProjection {
                    width: 4,
                    height: 2,
                },
                Orientation::IDENTITY,
            )
            .expect_err("overflow/underflow must not silently distort a GPU panorama");
        assert!(matches!(error, insta360_rs::Error::GpuUnavailable(_)));
    }
}

#[test]
fn gpu_rolling_shutter_recovers_independent_world_rays_and_matches_cpu() {
    if !gpu_available_or_skip() {
        return;
    }
    let cpu = CpuStitcher::new();
    let gpu = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    let software = software_vulkan(&gpu);
    // Mesa's quantized filtering measures 0.48–0.63 DN mean world-ray error
    // and up to 2 DN against CPU interpolation. Hardware keeps its tighter bounds.
    let maximum_mean = if software { 1.0 } else { 0.3 };
    let maximum_cpu_difference = if software { 2 } else { 1 };
    for columns in [false, true] {
        for (reference, target) in [
            ([0.0; 3], [0.0; 3]),
            ([17.0, -23.0, 41.0], [0.0, 0.0, 29.0]),
        ] {
            let chart = rolling_shutter::chart(rolling_shutter::scans(columns), reference, target);
            let corrected = gpu
                .stitch_with_motion(
                    &chart.lenses,
                    &chart.calibration,
                    chart.projection,
                    &chart.motion,
                )
                .expect("GPU corrected sensor geometry");
            let expected = cpu
                .stitch_with_motion(
                    &chart.lenses,
                    &chart.calibration,
                    chart.projection,
                    &chart.motion,
                )
                .expect("CPU corrected sensor geometry");
            let uncorrected = gpu
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
            assert!(
                corrected_mean < maximum_mean && corrected_max <= 2,
                "independent GPU world-ray error: mean {corrected_mean}, max {corrected_max}"
            );
            // An 8x ratio below the software filter's 1 DN floor measures
            // quantization. Require improvement exceeding that whole floor;
            // the independent corrected-image mean and maximum remain bounded.
            let removes_skew = if software {
                corrected_mean + 1.0 < uncorrected_mean
            } else {
                corrected_mean * 8.0 < uncorrected_mean
            };
            assert!(uncorrected_mean > 1.0 && removes_skew, "GPU correction must remove skew: corrected {corrected_mean}, uncorrected {uncorrected_mean}");
            let difference = sorted_channel_differences(corrected.as_rgb8(), expected.as_rgb8());
            assert!(*difference.last().expect("pixels") <= maximum_cpu_difference);
        }
        let scans = rolling_shutter::scans(columns).map(|scan| rolling_shutter::Scan {
            angle: 0.0,
            phase: 0.0,
            ..scan
        });
        let chart = rolling_shutter::chart(scans, [17.0, -23.0, 41.0], [0.0, 0.0, 29.0]);
        let corrected = gpu
            .stitch_with_motion(
                &chart.lenses,
                &chart.calibration,
                chart.projection,
                &chart.motion,
            )
            .expect("zero motion GPU table");
        let global = gpu
            .stitch_with_orientation(
                &chart.lenses,
                &chart.calibration,
                chart.projection,
                chart.motion.correction(),
            )
            .expect("global GPU correction");
        assert_eq!(corrected, global);
    }
}

#[test]
fn gpu_rolling_shutter_preserves_masks_and_color_with_stabilization() {
    use insta360_rs::motion::{FrameMotion, ReadoutDirection, ReadoutPoseTable};
    if !gpu_available_or_skip() {
        return;
    }
    let table = |angle: f64| {
        ReadoutPoseTable::new(
            ReadoutDirection::RightToLeft,
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
        [Some(table(0.02)), Some(table(-0.03))],
    )
    .expect("frame motion");
    let calibration = common::x5_v6_underwater_calibration(128, 128);
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let cpu = CpuStitcher::new();
    let gpu = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    let render = |exterior| {
        gpu.stitch_with_motion(
            &common::x5_underwater_lenses_with_exterior(exterior),
            &calibration,
            projection,
            &motion,
        )
        .expect("masked GPU motion rendering")
    };
    let black = render(0);
    let white = render(255);
    assert_eq!(black, white);
    assert!(black.as_rgb8().iter().all(|channel| *channel == 100));

    let lenses = [textured_lens(128, 128, 0), textured_lens(128, 128, 1)];
    let actual = gpu
        .stitch_with_motion(&lenses, &calibration, projection, &motion)
        .expect("GPU readout and radiometry");
    let expected = cpu
        .stitch_with_motion(&lenses, &calibration, projection, &motion)
        .expect("CPU readout and radiometry");
    let differences = sorted_channel_differences(actual.as_rgb8(), expected.as_rgb8());
    // Mesa software filtering measures p99 3 DN; its maximum remains below 5.
    let maximum_percentile_99 = if software_vulkan(&gpu) { 3 } else { 2 };
    assert!(
        differences[differences.len() * 99 / 100] <= maximum_percentile_99,
        "99th percentile must preserve color parity"
    );
    assert!(
        *differences.last().expect("pixels") <= 5,
        "maximum GPU/CPU radiometry deviation"
    );
}

#[test]
fn gpu_rolling_shutter_yuv_input_and_encoder_output_use_the_same_sensor_motion() {
    use insta360_rs::gpu::{
        GpuChromaLocation, GpuPlane, GpuYuv420Frame, GpuYuvMatrix, GpuYuvRange,
    };
    if !gpu_available_or_skip() {
        return;
    }
    let chart = rolling_shutter::chart(
        rolling_shutter::scans(true),
        [17.0, -23.0, 41.0],
        [0.0, 0.0, 29.0],
    );
    let size = chart.lenses[0].width() as usize;
    let luma: [Vec<u8>; 2] = std::array::from_fn(|lens| {
        chart.lenses[lens]
            .as_rgb8()
            .chunks_exact(3)
            .map(|pixel| pixel[0])
            .collect()
    });
    let chroma = vec![128; size * size / 4];
    let yuv_lenses = std::array::from_fn(|lens| GpuYuv420Frame {
        width: size as u32,
        height: size as u32,
        y: GpuPlane {
            data: &luma[lens],
            stride: size,
        },
        u: GpuPlane {
            data: &chroma,
            stride: size / 2,
        },
        v: GpuPlane {
            data: &chroma,
            stride: size / 2,
        },
        range: GpuYuvRange::Full,
        matrix: GpuYuvMatrix::Bt709,
        chroma_location: GpuChromaLocation::Center,
    });
    let rgb_lenses = std::array::from_fn(|lens| {
        LensFrame::new(
            size as u32,
            size as u32,
            luma[lens].iter().flat_map(|value| [*value; 3]).collect(),
        )
        .expect("matching gray RGB image")
    });
    let gpu = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    let rgb = gpu
        .stitch_with_motion(
            &rgb_lenses,
            &chart.calibration,
            chart.projection,
            &chart.motion,
        )
        .expect("RGB motion");
    let yuv = gpu
        .stitch_yuv420_with_motion(
            &yuv_lenses,
            &chart.calibration,
            chart.projection,
            &chart.motion,
        )
        .expect("YUV input motion");
    let differences = sorted_channel_differences(rgb.as_rgb8(), yuv.as_rgb8());
    assert!(*differences.last().expect("pixels") <= 1);
    let independent_gray: Vec<_> = chart
        .expected
        .chunks_exact(3)
        .flat_map(|pixel| [pixel[0]; 3])
        .collect();
    let (mean, maximum) = rolling_shutter::error(&yuv, &independent_gray);
    // Quantized software filtering measures 0.68 DN mean; keep the 2 DN maximum.
    let maximum_mean = if software_vulkan(&gpu) { 1.0 } else { 0.3 };
    assert!(
        mean < maximum_mean && maximum <= 2,
        "YUV world-ray error mean {mean}, max {maximum}"
    );

    let encoded = gpu
        .stitch_yuv420_to_yuv420_with_motion(
            &yuv_lenses,
            &chart.calibration,
            chart.projection,
            &chart.motion,
        )
        .expect("encoder YUV motion");
    let planes = encoded.planes();
    let width = chart.projection.width as usize;
    for (row, expected_row) in yuv.as_rgb8().chunks_exact(width * 3).enumerate() {
        for (column, pixel) in expected_row.chunks_exact(3).enumerate() {
            let luma = 0.2126 * f64::from(pixel[0])
                + 0.7152 * f64::from(pixel[1])
                + 0.0722 * f64::from(pixel[2]);
            let expected = (16.0 + 219.0 * luma / 255.0).round() as u8;
            assert!(planes[0].data[row * planes[0].stride + column].abs_diff(expected) <= 1);
        }
    }
}

#[test]
fn gpu_native_readout_clips_lens_coverage_at_capture_time() {
    use insta360_rs::motion::{FrameMotion, ReadoutDirection, ReadoutPoseTable};
    if !gpu_available_or_skip() {
        return;
    }
    let calibration = common::x5_v6_underwater_calibration(128, 128);
    let lenses = std::array::from_fn(|_| {
        LensFrame::new(128, 128, vec![100; 128 * 128 * 3]).expect("neutral lens")
    });
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let gpu = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    let cpu = CpuStitcher::new();
    let correction = Orientation::from_euler_degrees(13.0, -21.0, 37.0).expect("reference pose");
    for axis in [[0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
        let capture =
            Orientation::from_axis_angle(axis, 0.3).expect("capture relative to reference");
        let table = ReadoutPoseTable::new(
            ReadoutDirection::RightToLeft,
            [0.12, 0.91],
            vec![capture; 2],
        )
        .expect("constant readout");
        let motion = FrameMotion::new(correction, [Some(table.clone()), Some(table)])
            .expect("constant sensor offset");
        let actual = gpu
            .stitch_with_motion(&lenses, &calibration, projection, &motion)
            .expect("GPU native capture projection");
        let equivalent_global = gpu
            .stitch_with_orientation(
                &lenses,
                &calibration,
                projection,
                correction * capture.inverse(),
            )
            .expect("equivalent global projection");
        assert_eq!(actual, equivalent_global);
        assert!(actual.as_rgb8().iter().all(|channel| *channel == 100));
        let varying = ReadoutPoseTable::new(
            ReadoutDirection::BottomToTop,
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
        let actual = gpu
            .stitch_with_motion(&lenses, &calibration, projection, &motion)
            .expect("GPU varying native projection");
        let expected = cpu
            .stitch_with_motion(&lenses, &calibration, projection, &motion)
            .expect("CPU varying native projection");
        assert_eq!(actual, expected);
        assert!(actual.as_rgb8().iter().all(|channel| *channel == 100));
    }
}

#[test]
fn bundled_lut_changes_gpu_pixels_and_matches_cpu_color_conversion() {
    if !gpu_available_or_skip() {
        return;
    }
    let lenses = [textured_lens(64, 64, 0), textured_lens(64, 64, 1)];
    let calibration = synthetic_dual_fisheye_calibration(64, 64).expect("calibration");
    let projection = EquirectangularProjection {
        width: 128,
        height: 64,
    };
    let mut gpu = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    let baseline = gpu
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("unconverted panorama");
    let lut = std::sync::Arc::new(
        insta360_rs::color::CubeLut::load_bundled("studio-i-log-x5-rec709")
            .expect("verified Studio LUT"),
    );
    let mut expected = baseline.as_rgb8().to_vec();
    lut.apply_rgb8(&mut expected).expect("CPU color transform");
    assert_ne!(expected, baseline.as_rgb8());

    gpu.set_color_lut(Some(lut));
    let converted = gpu
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("GPU color transform");
    let maximum = expected
        .iter()
        .zip(converted.as_rgb8())
        .map(|(cpu, gpu)| cpu.abs_diff(*gpu))
        .max()
        .unwrap();
    assert!(maximum <= 1, "GPU LUT error reached {maximum} code values");

    gpu.set_color_lut(None);
    let restored = gpu
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("disabled transform");
    assert_eq!(restored, baseline);
}

fn textured_lens(width: u32, height: u32, lens_index: usize) -> LensFrame {
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for row in 0..height {
        for column in 0..width {
            let x = column * 180 / (width - 1);
            let y = row * 170 / (height - 1);
            let diagonal = (column + row) * 150 / (width + height - 2);
            let color = if lens_index == 0 {
                [24 + x as u8, 28 + y as u8, 32 + diagonal as u8]
            } else {
                [30 + y as u8, 22 + diagonal as u8, 26 + x as u8]
            };
            rgb.extend_from_slice(&color);
        }
    }
    LensFrame::new(width, height, rgb).expect("textured lens")
}

fn sorted_channel_differences(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut differences: Vec<_> = first
        .iter()
        .zip(second)
        .map(|(first, second)| first.abs_diff(*second))
        .collect();
    differences.sort_unstable();
    differences
}

#[test]
fn adapter_discovery_does_not_expose_backend_types() {
    for adapter in insta360_rs::gpu::available_adapters() {
        assert!(!adapter.name.is_empty());
        assert!(!adapter.backend.is_empty());
        assert!(!adapter.device_type.is_empty());
    }
}

#[test]
fn masked_source_pixels_do_not_change_gpu_blending() {
    if !gpu_available_or_skip() {
        return;
    }
    let calibration = x5_v6_underwater_calibration(128, 128);
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let stitcher = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    let render = |exterior| {
        stitcher
            .stitch_with_orientation(
                &common::x5_underwater_lenses_with_exterior(exterior),
                &calibration,
                projection,
                Orientation::IDENTITY,
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
fn underwater_lower_housing_does_not_enter_gpu_panorama() {
    if !gpu_available_or_skip() {
        return;
    }
    let calibration = x5_v6_underwater_calibration(512, 512);
    let projection = EquirectangularProjection {
        width: 512,
        height: 256,
    };
    let stitcher = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    let render = |housing| {
        stitcher
            .stitch_with_orientation(
                &common::x5_underwater_lenses_with_lower_housing(housing),
                &calibration,
                projection,
                Orientation::IDENTITY,
            )
            .expect("underwater housing stitch")
    };
    let black = render(0);
    let white = render(255);
    assert!(black.as_rgb8().iter().all(|channel| *channel == 100));
    assert_eq!(black, white);
}

#[test]
fn gpu_rgb_output_accepts_odd_heights() {
    if !gpu_available_or_skip() {
        return;
    }
    let lenses = [textured_lens(64, 64, 0), textured_lens(64, 64, 1)];
    let calibration = synthetic_dual_fisheye_calibration(64, 64).expect("calibration");
    let cpu = CpuStitcher::new();
    let gpu = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    for width in [2, 6, 10] {
        let projection = EquirectangularProjection {
            width,
            height: width / 2,
        };
        let expected = cpu
            .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
            .expect("CPU RGB stitch");
        let actual = gpu
            .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
            .expect("GPU RGB stitch");
        let maximum = expected
            .as_rgb8()
            .iter()
            .zip(actual.as_rgb8())
            .map(|(cpu, gpu)| cpu.abs_diff(*gpu))
            .max()
            .expect("nonempty panorama");
        assert!(
            maximum <= 2,
            "{width}-pixel panorama differed by {maximum} DN"
        );
    }
}

#[test]
fn gpu_dispatch_matches_cpu_on_synthetic_lenses() {
    if !gpu_available_or_skip() {
        return;
    }
    let solid = |color: [u8; 3]| {
        let mut rgb = Vec::with_capacity(64 * 64 * 3);
        for _ in 0..64 * 64 {
            rgb.extend_from_slice(&color);
        }
        LensFrame::new(64, 64, rgb).expect("solid lens")
    };
    let lenses = [solid([220, 30, 10]), solid([5, 40, 210])];
    let calibration = synthetic_dual_fisheye_calibration(64, 64).expect("synthetic calibration");
    let projection = EquirectangularProjection {
        width: 128,
        height: 64,
    };
    let cpu = CpuStitcher::new()
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("CPU stitch");
    let stitcher = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    let gpu = stitcher
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("GPU stitch");

    assert_eq!(gpu.width(), cpu.width());
    assert_eq!(gpu.height(), cpu.height());
    let maximum_difference = gpu
        .as_rgb8()
        .iter()
        .zip(cpu.as_rgb8())
        .map(|(gpu, cpu)| gpu.abs_diff(*cpu))
        .max()
        .unwrap_or_default();
    assert!(
        maximum_difference <= 2,
        "GPU/CPU channel difference reached {maximum_difference} DN"
    );

    let repeated = stitcher
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("repeated GPU stitch");
    assert_eq!(gpu.as_rgb8(), repeated.as_rgb8());
}

#[test]
fn gpu_matches_cpu_for_x5_v6_underwater_mask_and_orientation() {
    if !gpu_available_or_skip() {
        return;
    }
    let lens_width = 128;
    let lens_height = 128;
    let lenses = [
        textured_lens(lens_width, lens_height, 0),
        textured_lens(lens_width, lens_height, 1),
    ];
    let calibration = x5_v6_underwater_calibration(lens_width, lens_height);
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let orientation = Orientation::from_euler_degrees(13.0, -21.0, 37.0)
        .expect("compound stabilization orientation");
    let cpu = CpuStitcher::new()
        .stitch_with_orientation(&lenses, &calibration, projection, orientation)
        .expect("CPU X5 stitch");
    let renderer = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    let gpu = renderer
        .stitch_with_orientation(&lenses, &calibration, projection, orientation)
        .expect("GPU X5 stitch");

    let cpu_holes = cpu
        .as_rgb8()
        .chunks_exact(3)
        .filter(|pixel| pixel == &[0, 0, 0])
        .count();
    let gpu_holes = gpu
        .as_rgb8()
        .chunks_exact(3)
        .filter(|pixel| pixel == &[0, 0, 0])
        .count();
    assert_eq!(
        cpu_holes, 0,
        "synthetic X5 calibration left CPU coverage holes"
    );
    assert_eq!(gpu_holes, 0, "GPU introduced X5 coverage holes");

    let differences = sorted_channel_differences(gpu.as_rgb8(), cpu.as_rgb8());
    let mean = differences
        .iter()
        .map(|difference| f64::from(*difference))
        .sum::<f64>()
        / differences.len() as f64;
    let percentile_99 = differences[differences.len() * 99 / 100];
    let maximum = *differences.last().expect("non-empty panorama");
    // Vulkan permits device-specific fractional precision for linear filtering:
    // https://docs.vulkan.org/spec/latest/chapters/textures.html#textures-unnormalized-to-integer
    // Mesa 22/25 software filtering measures mean 0.695, p99 3, and max 4 DN.
    // Keep the tighter budgets for hardware adapters and the same maximum for all.
    let adapter = renderer.adapter_info();
    let (maximum_mean, maximum_percentile_99) =
        if adapter.backend == "Vulkan" && adapter.device_type == "Cpu" {
            (1.0, 3)
        } else {
            (0.5, 2)
        };
    assert!(
        mean <= maximum_mean,
        "GPU/CPU mean difference reached {mean:.4} DN"
    );
    assert!(
        percentile_99 <= maximum_percentile_99,
        "GPU/CPU p99 difference reached {percentile_99} DN"
    );
    assert!(
        maximum <= 8,
        "GPU/CPU maximum difference reached {maximum} DN"
    );
}

#[test]
fn gpu_matches_cpu_for_a_zero_width_hard_seam() {
    if !gpu_available_or_skip() {
        return;
    }
    let lens_width = 128;
    let lens_height = 128;
    let lenses = [
        textured_lens(lens_width, lens_height, 0),
        textured_lens(lens_width, lens_height, 1),
    ];
    let mut calibration = x5_v6_underwater_calibration(lens_width, lens_height);
    for geometry in &mut calibration.lens_geometry {
        let geometry = geometry.as_mut().expect("resolved X5 geometry");
        geometry.blend_angle_degrees = 180.0;
        geometry.blend_angle_recorded = true;
    }
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let cpu = CpuStitcher::new()
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("CPU hard seam");
    let gpu = insta360_rs::gpu::GpuStitcher::new()
        .expect("GPU renderer")
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("GPU hard seam");

    assert!(cpu
        .as_rgb8()
        .chunks_exact(3)
        .all(|pixel| pixel != [0, 0, 0]));
    assert!(gpu
        .as_rgb8()
        .chunks_exact(3)
        .all(|pixel| pixel != [0, 0, 0]));
    let maximum = gpu
        .as_rgb8()
        .iter()
        .zip(cpu.as_rgb8())
        .map(|(gpu, cpu)| gpu.abs_diff(*cpu))
        .max()
        .unwrap_or_default();
    assert!(
        maximum <= 8,
        "GPU/CPU hard-seam difference reached {maximum} DN"
    );
}

#[cfg(feature = "media")]
#[test]
fn gpu_exports_real_x5_midpoint_png_when_configured() {
    use std::path::PathBuf;
    use std::time::Duration;

    use insta360_rs::{
        probe, EffectiveBackend, FrameSelection, ImageExportOptions, ImageFormat, InputSet,
        OpticalSetup, ProcessingBackend, Stabilization,
    };

    let Some(path) = std::env::var_os("INSTA360_RS_X5_SAMPLE").map(PathBuf::from) else {
        return;
    };
    if !gpu_available_or_skip() {
        return;
    }
    let inputs = InputSet::new(vec![path]).expect("real X5 input set");
    let duration = probe(&inputs)
        .expect("probe real X5 sample")
        .duration
        .expect("real X5 sample duration");
    let midpoint = Duration::from_secs_f64(duration.as_secs_f64() * 0.5);
    let directory = tempfile::tempdir().expect("temporary real-media output directory");
    let config = insta360_rs::StitchConfig {
        optical_setup: OpticalSetup::InvisibleDiveCaseUnderwater,
        stabilization: Stabilization::DirectionLock,
        backend: ProcessingBackend::Gpu,
        projection: Some(EquirectangularProjection {
            width: 1_920,
            height: 960,
        }),
        ..insta360_rs::StitchConfig::default()
    };
    let result = insta360_rs::media::Exporter::new(inputs, config)
        .expect("real X5 exporter")
        .export_frames(
            directory.path(),
            FrameSelection::Timestamps(vec![midpoint]),
            ImageExportOptions {
                format: ImageFormat::Png,
                ..ImageExportOptions::default()
            },
        )
        .wait()
        .expect("real X5 midpoint GPU export");

    assert_eq!(result.backend.requested, ProcessingBackend::Gpu);
    assert_eq!(result.backend.selected, EffectiveBackend::Gpu);
    assert!(result.backend.adapter.is_some());
    assert!(result.backend.fallback.is_none());
    assert_eq!(result.frames_written, 1);
    assert_eq!(result.outputs.len(), 1);
    assert_eq!(result.outputs[0].parent(), Some(directory.path()));
    assert_eq!(
        result.outputs[0]
            .extension()
            .and_then(|value| value.to_str()),
        Some("png")
    );

    let decoded = image::open(&result.outputs[0])
        .expect("decode completed midpoint PNG")
        .to_rgb8();
    assert_eq!(decoded.dimensions(), (1_920, 960));
    assert_eq!(decoded.as_raw().len(), 1_920 * 960 * 3);
    assert!(decoded.as_raw().iter().any(|value| *value != 0));

    let entries: Vec<_> = std::fs::read_dir(directory.path())
        .expect("read temporary output directory")
        .collect::<std::io::Result<_>>()
        .expect("read every temporary output entry");
    assert_eq!(
        entries.len(),
        1,
        "export left a partial or unexpected artifact"
    );
    assert_eq!(entries[0].path(), result.outputs[0]);
}

#[test]
fn gpu_yuv420_path_matches_neutral_rgb_and_returns_encoder_planes() {
    use insta360_rs::gpu::{
        GpuChromaLocation, GpuPlane, GpuYuv420Frame, GpuYuvMatrix, GpuYuvRange,
    };

    if !gpu_available_or_skip() {
        return;
    }
    let width = 64;
    let height = 64;
    let y = vec![128_u8; width * height];
    let u = vec![128_u8; width * height / 4];
    let v = vec![128_u8; width * height / 4];
    let yuv_frame = GpuYuv420Frame {
        width: width as u32,
        height: height as u32,
        y: GpuPlane {
            data: &y,
            stride: width,
        },
        u: GpuPlane {
            data: &u,
            stride: width / 2,
        },
        v: GpuPlane {
            data: &v,
            stride: width / 2,
        },
        range: GpuYuvRange::Full,
        matrix: GpuYuvMatrix::Bt709,
        chroma_location: GpuChromaLocation::Center,
    };
    let yuv_lenses = [yuv_frame; 2];
    let rgb_lenses = std::array::from_fn(|_| {
        LensFrame::new(width as u32, height as u32, vec![128; width * height * 3])
            .expect("neutral RGB lens")
    });
    let calibration =
        synthetic_dual_fisheye_calibration(width as u32, height as u32).expect("calibration");
    let projection = EquirectangularProjection {
        width: 128,
        height: 64,
    };
    let stitcher = insta360_rs::gpu::GpuStitcher::new().expect("GPU renderer");
    let rgb = stitcher
        .stitch_with_orientation(&rgb_lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("RGB input stitch");
    let converted = stitcher
        .stitch_yuv420_with_orientation(
            &yuv_lenses,
            &calibration,
            projection,
            Orientation::IDENTITY,
        )
        .expect("YUV input stitch");
    let maximum_difference = rgb
        .as_rgb8()
        .iter()
        .zip(converted.as_rgb8())
        .map(|(rgb, yuv)| rgb.abs_diff(*yuv))
        .max()
        .unwrap_or_default();
    assert!(
        maximum_difference <= 1,
        "neutral YUV differed by {maximum_difference} DN"
    );

    let encoder_frame = stitcher
        .stitch_yuv420_to_yuv420_with_orientation(
            &yuv_lenses,
            &calibration,
            projection,
            Orientation::IDENTITY,
        )
        .expect("GPU YUV output");
    assert_eq!((encoder_frame.width(), encoder_frame.height()), (128, 64));
    let planes = encoder_frame.planes();
    assert!(planes[0].data.len() >= 128 * 64);
    assert!(planes[1].data.len() >= 64 * 32);
    assert!(planes[2].data.len() >= 64 * 32);

    let odd_output = stitcher
        .stitch_yuv420_to_yuv420_with_orientation(
            &yuv_lenses,
            &calibration,
            EquirectangularProjection {
                width: 6,
                height: 3,
            },
            Orientation::IDENTITY,
        )
        .expect_err("YUV420 output still requires even dimensions");
    assert!(matches!(odd_output, insta360_rs::Error::InvalidMedia(_)));
}

#[test]
fn gpu_color_compensation_keeps_bright_lens_hemispheres_visible() {
    if !gpu_available_or_skip() {
        return;
    }
    let calibration = x5_v6_underwater_calibration(128, 128);
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let renderer = insta360_rs::gpu::GpuStitcher::new().expect("GPU");
    for levels in [[22, 220], [220, 22]] {
        let lenses = levels
            .map(|level| LensFrame::new(128, 128, vec![level; 128 * 128 * 3]).expect("solid lens"));
        let panorama = renderer
            .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
            .expect("unequal exposures stitch");
        let minimum = *panorama.as_rgb8().iter().min().expect("pixels");
        assert!(
            minimum >= 20,
            "GPU color compensation erased a valid ray: {minimum}"
        );
        for (column, level) in [(127, levels[0]), (255, levels[1])] {
            let pixel = ((64 * projection.width + column) * 3) as usize;
            assert_eq!(
                panorama.as_rgb8()[pixel],
                level,
                "optical axis must stay neutral"
            );
        }
        let cpu = CpuStitcher::new()
            .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
            .expect("CPU");
        assert!(panorama
            .as_rgb8()
            .iter()
            .zip(cpu.as_rgb8())
            .all(|(a, b)| a.abs_diff(*b) <= 1));
    }
}

#[test]
fn gpu_color_compensation_rotates_with_the_camera() {
    if !gpu_available_or_skip() {
        return;
    }
    let lenses = [
        LensFrame::new(128, 128, vec![60; 128 * 128 * 3]).expect("first lens"),
        LensFrame::new(128, 128, vec![180; 128 * 128 * 3]).expect("second lens"),
    ];
    let calibration = x5_v6_underwater_calibration(128, 128);
    let projection = EquirectangularProjection {
        width: 256,
        height: 128,
    };
    let renderer = insta360_rs::gpu::GpuStitcher::new().expect("GPU");
    let baseline = renderer
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("baseline");
    let orientation =
        Orientation::from_axis_angle([1.0, 0.0, 0.0], std::f64::consts::PI).expect("half turn");
    let rotated = renderer
        .stitch_with_orientation(&lenses, &calibration, projection, orientation)
        .expect("rotated");
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
    let cpu = CpuStitcher::new()
        .stitch_with_orientation(&lenses, &calibration, projection, orientation)
        .expect("CPU");
    assert!(rotated
        .as_rgb8()
        .iter()
        .zip(cpu.as_rgb8())
        .all(|(a, b)| a.abs_diff(*b) <= 1));
}

#[test]
fn gpu_color_smoothing_wraps_windows_wider_than_the_panorama() {
    if !gpu_available_or_skip() {
        return;
    }
    let lenses = [textured_lens(128, 128, 0), textured_lens(128, 128, 1)];
    let mut calibration = x5_v6_underwater_calibration(128, 128);
    // Broad synthetic overlap keeps radiometry active at this tiny output size.
    calibration.camera_model = None;
    for geometry in calibration.lens_geometry.iter_mut().flatten() {
        geometry.full_fov_degrees = 360.0;
        geometry.blend_angle_degrees = 360.0;
    }
    let projection = EquirectangularProjection {
        width: 8,
        height: 4,
    };
    let cpu = CpuStitcher::new()
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("CPU");
    let gpu = insta360_rs::gpu::GpuStitcher::new()
        .expect("GPU")
        .stitch_with_orientation(&lenses, &calibration, projection, Orientation::IDENTITY)
        .expect("GPU panorama");
    let maximum = gpu
        .as_rgb8()
        .iter()
        .zip(cpu.as_rgb8())
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .expect("pixels");
    assert!(maximum <= 1, "GPU color smoothing differed by {maximum} DN");
}
